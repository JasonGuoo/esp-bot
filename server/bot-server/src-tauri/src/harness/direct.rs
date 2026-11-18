//! OpenCode has no per-provider socket interface option. Give only its three
//! managed chat adapters private loopback URLs; keep the destination and TLS
//! hostname from the validated provider configuration. No system route changes.
use super::{config::ProviderNetwork, HarnessConfig, ModelRoute};
use axum::{
    body::{to_bytes, Body},
    extract::{Path, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Router,
};
use reqwest::Client;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{watch, Semaphore},
};
use uuid::Uuid;

#[cfg(target_os = "macos")]
mod network {
    use super::*;
    use futures_util::{stream, StreamExt};
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};
    use rustls::pki_types::ServerName;
    use socket2::SockRef;
    use std::{
        ffi::CStr,
        io,
        net::{IpAddr, SocketAddr},
        num::NonZeroU32,
        os::raw::c_char,
        sync::LazyLock,
    };
    use tokio::net::TcpSocket;
    use tokio_rustls::TlsConnector;

    extern "C" {
        fn espbot_active_interface(
            name: *mut c_char,
            capacity: usize,
            dns: *mut c_char,
            dns_capacity: usize,
        ) -> u32;
    }

    #[derive(Clone, Eq, PartialEq)]
    pub(super) struct ProviderDns {
        interface: String,
        index: NonZeroU32,
        resolvers: Vec<String>,
        port: u16,
    }

    fn active_network(port: u16) -> Option<ProviderDns> {
        let mut name = [0 as c_char; libc::IF_NAMESIZE];
        let mut dns = [0 as c_char; 512];
        let found = unsafe {
            espbot_active_interface(name.as_mut_ptr(), name.len(), dns.as_mut_ptr(), dns.len())
        };
        let index = NonZeroU32::new(found)?;
        let interface = unsafe { CStr::from_ptr(name.as_ptr()) }
            .to_str()
            .ok()?
            .to_owned();
        let list = unsafe { CStr::from_ptr(dns.as_ptr()) }.to_str().ok()?;
        let mut resolvers = Vec::new();
        for address in list.split(',') {
            let literal = address.split('%').next().unwrap_or_default();
            let Ok(ip) = literal.parse::<IpAddr>() else {
                continue;
            };
            let scoped = match ip {
                IpAddr::V6(v6) if v6.is_unicast_link_local() => format!("{v6}%{interface}"),
                _ => ip.to_string(),
            };
            if !resolvers.contains(&scoped) {
                resolvers.push(scoped);
            }
        }
        Some(ProviderDns {
            interface,
            index,
            resolvers,
            port,
        })
    }

    async fn addresses(host: &str, server: Option<&str>, family: &str) -> Vec<SocketAddr> {
        let mut dig = tokio::process::Command::new("/usr/bin/dig");
        dig.kill_on_drop(true).env_clear().env("HOME", "/var/empty");
        if let Some(ip) = server {
            dig.arg(format!("@{ip}"));
        }
        // A lost DHCP-resolver reply can hide GLM's only usable address family.
        let (tries, deadline_ms) = if host == "open.bigmodel.cn" && server.is_some() && family == "AAAA" {
            ("+tries=2", 2400)
        } else {
            ("+tries=1", 1500)
        };
        let query = dig
            .args(["+time=1", tries, "+short", "-q", host, "-t", family])
            .output();
        match tokio::time::timeout(Duration::from_millis(deadline_ms), query).await {
            Ok(Ok(out)) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|s| s.trim().parse::<IpAddr>().ok())
                .take(32)
                .map(|ip| SocketAddr::new(ip, 0))
                .collect(),
            _ => Vec::new(),
        }
    }

    // Some CDN edges accept TCP on the physical interface but stall during
    // TLS. A fresh GLM connection must pick an edge that authenticated the
    // provider hostname. No HTTP headers or prompt are sent during this race.
    async fn tls_peer(
        index: NonZeroU32,
        host: ServerName<'static>,
        address: SocketAddr,
    ) -> io::Result<()> {
        static TLS: LazyLock<TlsConnector> = LazyLock::new(|| {
            let roots =
                rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        });
        let socket = if address.is_ipv6() {
            let socket = TcpSocket::new_v6()?;
            SockRef::from(&socket).bind_device_by_index_v6(Some(index))?;
            socket
        } else {
            let socket = TcpSocket::new_v4()?;
            SockRef::from(&socket).bind_device_by_index_v4(Some(index))?;
            socket
        };
        TLS.connect(host, socket.connect(address).await?).await?;
        Ok(())
    }

    async fn select_glm_peer(
        candidates: &[SocketAddr],
        index: NonZeroU32,
        port: u16,
    ) -> io::Result<SocketAddr> {
        let host = ServerName::try_from("open.bigmodel.cn").expect("valid provider hostname");
        let family = |ipv6| {
            let host = host.clone();
            let addresses = candidates
                .iter()
                .copied()
                .filter(move |addr| addr.is_ipv6() == ipv6);
            stream::iter(addresses.map(move |address| {
                let probe = tls_peer(index, host.clone(), SocketAddr::new(address.ip(), port));
                async move {
                    let outcome = tokio::time::timeout(Duration::from_secs(3), probe).await;
                    matches!(outcome, Ok(Ok(()))).then_some(address)
                }
            }))
            .buffer_unordered(4)
        };
        let first = async {
            let mut replies = stream::select(family(true), family(false));
            while let Some(result) = replies.next().await {
                if let Some(address) = result {
                    return Some(address);
                }
            }
            None
        };
        tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .ok()
            .flatten()
            .ok_or_else(|| io::Error::other("no_direct_glm_tls_peer"))
    }

    impl Resolve for ProviderDns {
        fn resolve(&self, name: Name) -> Resolving {
            let resolvers = self.resolvers.clone();
            let (index, port) = (self.index, self.port);
            Box::pin(async move {
                let host = name.as_str();
                let system = tokio::net::lookup_host((host, 0));
                // Include the old international endpoint for installations that
                // explicitly selected it. Other names keep the system's split DNS.
                let known = matches!(
                    host,
                    "open.bigmodel.cn"
                        | "api.z.ai"
                        | "api.deepseek.com"
                        | "coding.dashscope.aliyuncs.com"
                );
                if !known {
                    let resolved: Vec<_> = system.await?.collect();
                    return Ok(Box::new(resolved.into_iter()) as Addrs);
                }
                // A VPN resolver may omit AAAA records even when the physical
                // interface has a usable IPv6 path. Query both address families.
                let sources = resolvers
                    .iter()
                    .map(|ip| Some(ip.as_str()))
                    .chain(std::iter::once(None));
                let lookups = sources
                    .flat_map(|server| ["AAAA", "A"].map(|family| addresses(host, server, family)));
                let (system, extra) = tokio::join!(
                    tokio::time::timeout(Duration::from_millis(1500), system),
                    futures_util::future::join_all(lookups)
                );
                let mut candidates = Vec::new();
                let system = system
                    .ok()
                    .and_then(Result::ok)
                    .into_iter()
                    .flatten()
                    .take(64);
                for addr in system.chain(extra.into_iter().flatten()) {
                    let addr = std::net::SocketAddr::new(addr.ip().to_canonical(), addr.port());
                    if !candidates.contains(&addr) {
                        candidates.push(addr);
                    }
                }
                if candidates.is_empty() {
                    return Err(io::Error::other("provider_dns_unavailable").into());
                }
                if host == "open.bigmodel.cn" && candidates.len() > 1 {
                    let address = select_glm_peer(&candidates, index, port).await?;
                    return Ok(Box::new(std::iter::once(address)) as Addrs);
                }
                Ok(Box::new(candidates.into_iter()) as Addrs)
            })
        }
    }

    pub(super) async fn client(
        connections: &Observations,
        target: &str,
    ) -> Result<(String, Client), &'static str> {
        let port = reqwest::Url::parse(target)
            .ok()
            .and_then(|url| url.port_or_known_default())
            .ok_or("provider_interface_no_endpoint")?;
        let network = tokio::task::spawn_blocking(move || active_network(port))
            .await
            .map_err(|_| "provider_interface_unavailable")?
            .ok_or("provider_interface_unavailable")?;
        let interface = network.interface.clone();
        let mut pool = connections
            .pool
            .lock()
            .map_err(|_| "provider_interface_client_failed")?;
        if let Some((previous, client)) = pool.as_ref() {
            if previous == &network {
                return Ok((interface, client.clone()));
            }
        }
        let client = Client::builder()
            .no_proxy()
            .https_only(true)
            .interface(&interface)
            .user_agent(concat!("Xiaodai-Bot-Server/", env!("CARGO_PKG_VERSION")))
            .dns_resolver(Arc::new(network.clone()))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(8))
            .pool_idle_timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "provider_interface_client_failed")?;
        *pool = Some((network, client.clone()));
        Ok((interface, client))
    }
}

#[cfg(not(target_os = "macos"))]
mod network {
    use super::*;
    pub(super) async fn client(_: &Observations, _: &str) -> Result<(String, Client), &'static str> {
        Err("provider_interface_requires_macos")
    }
}

pub(super) async fn probe(route: &ModelRoute, connections: &Observations) -> Result<Value, String> {
    if !route.supports_physical_network() {
        return Err("provider_interface_unsupported_route".into());
    }
    let start = Instant::now();
    let base = route
        .base_url
        .as_deref()
        .ok_or("provider_interface_no_endpoint")?;
    let (interface, client) = network::client(connections, base).await?;
    // No credentials, no inference, no redirects. Even a 401 proves HTTPS connectivity.
    let response = client
        .get(format!("{}/models", base.trim_end_matches('/')))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|_| format!("provider_interface_connect_failed: {interface}"))?;
    Ok(json!({
        "route_id": route.id,
        "interface": interface,
        "status": response.status().as_u16(),
        "peer_ip": response.remote_addr().map(|addr| addr.ip().to_canonical()),
        "elapsed_ms": start.elapsed().as_millis(),
    }))
}

#[derive(Default, Clone)]
pub(super) struct Observations {
    last: Arc<Mutex<HashMap<String, Value>>>,
    #[cfg(target_os = "macos")]
    pool: Arc<Mutex<Option<(network::ProviderDns, Client)>>>,
}
impl Observations {
    fn record(&self, route: &str, interface: Option<&str>, reply: Option<&reqwest::Response>) {
        if let Ok(mut last) = self.last.lock() {
            last.insert(
                route.to_owned(),
                json!({
                    "interface": interface,
                    "status": reply.map(|r| r.status().as_u16()),
                    "peer_ip": reply.and_then(|r| r.remote_addr()).map(|addr| addr.ip().to_canonical()),
                    "at_ms": super::now_ms(),
                }),
            );
        }
    }
    pub(super) fn last(&self, route: &str) -> Option<Value> {
        self.last.lock().ok()?.get(route).cloned()
    }
}

struct RelayState {
    targets: HashMap<String, (String, String)>,
    authority: String,
    timeout: Duration,
    slots: Arc<Semaphore>,
    last: Observations,
    closed: watch::Receiver<bool>,
}

pub(super) struct Relay {
    close: watch::Sender<bool>,
}

impl Relay {
    pub(super) async fn start(
        config: &HarnessConfig,
        effective: &mut Value,
        last: Observations,
    ) -> Result<Option<Self>, String> {
        if config.provider_network != ProviderNetwork::ActiveInterface {
            return Ok(None);
        }
        let routes: Vec<_> = config
            .routes
            .iter()
            .filter(|r| r.enabled && r.physical_network_target())
            .collect();
        if routes.is_empty() {
            return Ok(None);
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| "provider_interface_listener_failed")?;
        let authority = listener
            .local_addr()
            .map_err(|_| "provider_interface_listener_failed")?
            .to_string();
        let mut targets = HashMap::new();
        for route in routes {
            let base = route
                .base_url
                .as_deref()
                .filter(|_| route.supports_physical_network())
                .ok_or("provider_interface_unsupported_route")?;
            let token = Uuid::new_v4().simple().to_string();
            effective["providers"][&route.provider_id]["settings"]["baseURL"] =
                json!(format!("http://{authority}/{token}"));
            targets.insert(
                token,
                (
                    route.id.clone(),
                    format!("{}/chat/completions", base.trim_end_matches('/')),
                ),
            );
        }
        let (close, mut closed) = watch::channel(false);
        let state = Arc::new(RelayState {
            targets,
            authority,
            last,
            timeout: Duration::from_millis(config.turn_timeout_ms),
            slots: Arc::new(Semaphore::new(6)),
            closed: closed.clone(),
        });
        let app = Router::new()
            .route("/{token}/chat/completions", post(forward))
            .with_state(state);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = closed.wait_for(|v| *v).await;
                })
                .await;
        });
        Ok(Some(Self { close }))
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.close.send_replace(true);
    }
}

async fn forward(
    State(state): State<Arc<RelayState>>,
    Path(token): Path<String>,
    request: Request,
) -> Response {
    let Some((route, target)) = state.targets.get(&token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if request.headers().get(header::HOST).and_then(|v| v.to_str().ok()) != Some(state.authority.as_str())
        || request.uri().query().is_some()
        || request.headers().contains_key(header::ORIGIN)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(slot) = state.slots.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut closed = state.closed.clone();
    let exchange = async {
        let (parts, body) = request.into_parts();
        let Ok(body) = to_bytes(body, 16 * 1024 * 1024).await else {
            return StatusCode::PAYLOAD_TOO_LARGE.into_response();
        };
        let Ok((interface, client)) = network::client(&state.last, target).await else {
            state.last.record(route, None, None);
            return (StatusCode::BAD_GATEWAY, "provider_interface_unavailable").into_response();
        };
        let mut upstream = client.post(target).timeout(state.timeout).body(body);
        for name in [
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::CONTENT_ENCODING,
            header::ACCEPT_ENCODING,
            header::USER_AGENT,
        ] {
            if let Some(value) = parts.headers.get(&name) {
                upstream = upstream.header(name, value);
            }
        }
        let reply = match upstream.send().await {
            Ok(reply) => reply,
            Err(_) => {
                state.last.record(route, Some(&interface), None);
                return (StatusCode::BAD_GATEWAY, "provider_interface_connect_failed").into_response();
            }
        };
        state
            .last
            .record(route, Some(&interface), Some(&reply));
        if reply.status().is_redirection() {
            return (StatusCode::BAD_GATEWAY, "provider_interface_redirect_refused").into_response();
        }
        let mut out = Response::builder().status(reply.status());
        for name in [
            header::CONTENT_TYPE,
            header::CONTENT_ENCODING,
            header::CACHE_CONTROL,
            header::RETRY_AFTER,
        ] {
            if let Some(value) = reply.headers().get(&name) {
                out = out.header(name, value);
            }
        }
        let stream = reply.bytes_stream();
        // Hold the slot for the complete SSE body and cut it off when the child stops.
        let stream = futures_util::stream::unfold(
            (Box::pin(stream), state.closed.clone(), slot),
            |(mut stream, mut stop, slot)| async move {
                use futures_util::StreamExt;
                let part = tokio::select! { biased;
                    _ = stop.wait_for(|v| *v) => None,
                    chunk = stream.next() => chunk,
                };
                part.map(|part| (part, (stream, stop, slot)))
            },
        );
        out.body(Body::from_stream(stream))
            .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
    };
    tokio::select! { biased;
        _ = closed.wait_for(|v| *v) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        result = tokio::time::timeout(state.timeout, exchange) => result.unwrap_or_else(|_|StatusCode::GATEWAY_TIMEOUT.into_response())
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    fn has_peer(observation: &Value) -> bool {
        observation["interface"].as_str().is_some_and(|name| !name.is_empty())
            && observation["peer_ip"].as_str()
                .is_some_and(|ip| ip.parse::<std::net::IpAddr>().is_ok())
    }

    #[tokio::test]
    async fn only_configured_chat_routes_can_reach_the_relay() {
        let mut config = HarnessConfig::default();
        config.provider_network = ProviderNetwork::ActiveInterface;
        config.mode = "attach".into();
        assert!(config.validate().is_err());
        config.mode = "managed".into();
        let mut inherited = config.clone();
        inherited.routes[0].base_url = None;
        assert!(inherited.validate().is_err());
        let mut custom = config.routes[0].clone();
        custom.id = "other".into();
        custom.provider_id = "other".into();
        custom.base_url = Some("https://custom.example.test/v1".into());
        config.routes.push(custom);
        config.validate().unwrap();
        assert_eq!(
            config.provider_no_proxy(".internal"),
            ".internal,127.0.0.1,custom.example.test"
        );
        let saved = config.overlay();
        let mut effective = saved.clone();
        let relay = Relay::start(&config, &mut effective, Default::default())
            .await
            .unwrap()
            .unwrap();
        let base = |v: &Value, idx: usize| {
            v["providers"][&config.routes[idx].provider_id]["settings"]["baseURL"]
                .as_str()
                .map(str::to_owned)
        };
        assert_eq!(base(&saved, 3), base(&effective, 3));
        assert!(base(&effective, 2).is_none());
        let local = base(&effective, 0).unwrap();
        assert!(local.starts_with("http://127.0.0.1:"));
        assert_ne!(base(&effective, 1), Some(local.clone()));
        assert!(base(&saved, 0).unwrap().starts_with("https://"));
        let client = Client::builder().no_proxy().build().unwrap();
        let chat = format!("{local}/chat/completions");
        assert_eq!(client.get(&chat).send().await.unwrap().status(), 405);
        assert_eq!(
            client
                .post(format!("{local}/models"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            client
                .post(format!("{chat}?url=elsewhere"))
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .post(&chat)
                .header("host", "other.invalid")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        assert_eq!(
            client
                .post(&chat)
                .header("origin", "http://example.test")
                .send()
                .await
                .unwrap()
                .status(),
            403
        );
        drop(relay);
        tokio::time::timeout(Duration::from_secs(2), async {
            while client.get(&chat).send().await.is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "contacts three real providers; requires a working physical route to each endpoint"]
    async fn live_direct_endpoints_and_chat_paths() {
        let mut config = HarnessConfig::default();
        config.provider_network = ProviderNetwork::ActiveInterface;
        for route in &mut config.routes {
            route.enabled = true;
        }
        let mut overlay = config.overlay();
        let last = Observations::default();
        let _relay = Relay::start(&config, &mut overlay, last.clone()).await.unwrap();
        let client = Client::builder().no_proxy().build().unwrap();
        let mut failed = Vec::new();
        for route in &config.routes {
            let check = probe(route, &last).await;
            let local = overlay["providers"][&route.provider_id]["settings"]["baseURL"]
                .as_str()
                .unwrap();
            let status = client.post(format!("{local}/chat/completions")).bearer_auth("intentionally-invalid-smoke-key")
                .json(&json!({"model":route.model_id,"messages":[{"role":"user","content":"一"}],"max_tokens":1})).send().await.unwrap().status();
            let actual = last.last(&route.id).unwrap();
            println!("{}: probe={check:?} chat={status} observation={actual}", route.id);
            if !check.as_ref().is_ok_and(has_peer) || !has_peer(&actual)
                || actual["status"] != status.as_u16() || !matches!(status.as_u16(), 401 | 403)
            {
                failed.push(&route.id);
            }
        }
        assert!(
            failed.is_empty(),
            "providers without a confirmed direct unauthenticated chat response: {failed:?}"
        );
    }

    #[tokio::test]
    #[ignore = "requires the official OpenCode and the three real endpoints; synthetic keys, no inference"]
    async fn live_opencode_uses_private_provider_url() {
        use crate::harness::{secrets::SessionKeys, Harness, TurnRequest};
        let mut failed = Vec::new();
        for (index, route_id) in ["glm-coding-plan", "deepseek-flash", "qwen-coding-plan"]
            .into_iter()
            .enumerate()
        {
            let dir = std::env::temp_dir().join(format!("xiaodai-route-test-{}", Uuid::new_v4().simple()));
            struct Cleanup(std::path::PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_dir_all(&self.0);
                }
            }
            let _cleanup = Cleanup(dir.clone());
            let mut config = HarnessConfig::default();
            config.provider_network = ProviderNetwork::ActiveInterface;
            config.executable = std::env::var("XIAODAI_TEST_OPENCODE").unwrap();
            config.required_mcp.clear();
            config.native_actions.clear();
            config.read_tools.clear();
            for route in &mut config.routes {
                route.enabled = route.id == route_id;
            }
            let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
            config.base_url = format!("http://{}", socket.local_addr().unwrap());
            drop(socket);
            let keys = Arc::new(SessionKeys::default());
            keys.set(
                config.routes[index].api_key_env.as_deref().unwrap(),
                Some(zeroize::Zeroizing::new(
                    "intentionally-invalid-local-smoke-key".into(),
                )),
            )
            .unwrap();
            let harness =
                Harness::new(config, dir.join("opencode"), dir.clone(), Arc::new(|_| {}), keys).unwrap();
            harness.start().await.unwrap();
            let turn = TurnRequest {
                turn_id: "synthetic_probe".into(),
                conversation_id: "isolated_test".into(),
                user_text: "只回复数字一。".into(),
                system_prompt: "不用工具。".into(),
                response_schema: None,
                clarify_on_device: false,
                context_revision: None,
            };
            let result = tokio::time::timeout(Duration::from_secs(55), harness.run(turn)).await;
            harness.stop().await.unwrap();
            let last = harness.connections.last(route_id).unwrap_or(Value::Null);
            println!("OpenCode → {route_id}: {last}");
            if !matches!(result, Ok(Err(_)))
                || !has_peer(&last)
                || !matches!(last["status"].as_u64(), Some(401 | 403))
            {
                failed.push(route_id);
            }
        }
        assert!(
            failed.is_empty(),
            "OpenCode could not confirm direct provider authentication rejection: {failed:?}"
        );
    }
}

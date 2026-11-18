//! Direct HTTP routes: send GLM / DeepSeek / Qwen traffic out of the active physical Wi-Fi or
//! Ethernet interface instead of whatever the default route is (e.g. a VPN `utun`).
//!
//! Same approach as esp-bot's "通过活动 Wi-Fi 或有线网卡直连" (reimplemented here, nothing copied):
//! 1. Pick the interface: walk macOS's network service order (`Setup:/Network/Global/IPv4`
//!    `ServiceOrder`), keep services whose interface hardware is `AirPort` or `Ethernet`, whose link
//!    is not reported down, and whose `State:/Network/Service/<id>/IPv4|IPv6` has a router and an
//!    address on that device. That service's DNS servers are used too. (Read with `scutil`.)
//! 2. Resolve provider hosts with that interface's DNS servers (A + AAAA via `dig`) plus the system
//!    resolver; drop VPN fake-IP answers (198.18.0.0/15) and non-global addresses.
//! 3. Bind every socket to the interface (`IP_BOUND_IF` / `IPV6_BOUND_IF`, via reqwest's
//!    `.interface()`), no proxy. TLS SNI / certificate checks keep the provider hostname.
//! 4. When a host has several addresses, race TLS handshakes on the bound interface (no HTTP is
//!    sent) and put the first edge that completes first, because some CDN edges accept TCP on the
//!    physical path and then stall in TLS. The winner is cached for a few minutes.
//!
//! No system route changes. Non-macOS: no interface is found, so `auto` uses the system path.
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How routes marked `direct` reach the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HttpNetwork {
    /// The normal system path (follows the VPN if one is up).
    System,
    /// Bind to the active Wi-Fi/Ethernet interface; fail the attempt if there is none.
    ActiveInterface,
    /// Active interface when one is found, else the system path.
    Auto,
}

impl std::str::FromStr for HttpNetwork {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "system" => Ok(Self::System),
            "active-interface" | "direct" => Ok(Self::ActiveInterface),
            "auto" => Ok(Self::Auto),
            _ => Err(format!("unknown http network '{s}' (system | active-interface | auto)")),
        }
    }
}

/// The interface chosen for direct routes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActiveInterface {
    pub name: String,
    pub index: u32,
    pub hardware: String,
    pub ipv4: bool,
    pub ipv6: bool,
    /// Up to 3 DNS servers configured for that service.
    pub dns: Vec<String>,
}

/// Parse `scutil` `show` output (one or more blocks) into JSON values; `None` = "No such key".
pub fn parse_scutil(out: &str) -> Vec<Option<serde_json::Value>> {
    use serde_json::{Map, Value};
    let mut blocks = Vec::new();
    // Stack of (container, key-in-parent).
    let mut stack: Vec<(Value, Option<String>)> = Vec::new();
    for raw in out.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if stack.is_empty() {
            if line == "No such key" {
                blocks.push(None);
            } else if line.ends_with('{') {
                let v = if line.starts_with("<array>") { Value::Array(vec![]) } else { Value::Object(Map::new()) };
                stack.push((v, None));
            }
            continue;
        }
        if line == "}" {
            let (v, key) = stack.pop().unwrap();
            match stack.last_mut() {
                None => blocks.push(Some(v)),
                Some((parent, _)) => insert(parent, key.unwrap_or_default(), v),
            }
            continue;
        }
        let Some((k, v)) = line.split_once(" : ") else { continue };
        let (k, v) = (k.trim().to_owned(), v.trim());
        if v.ends_with('{') {
            let c = if v.starts_with("<array>") { Value::Array(vec![]) } else { Value::Object(Map::new()) };
            stack.push((c, Some(k)));
        } else if let Some((parent, _)) = stack.last_mut() {
            insert(parent, k, Value::String(v.to_owned()));
        }
    }
    fn insert(parent: &mut Value, key: String, v: Value) {
        match parent {
            Value::Array(a) => a.push(v),
            Value::Object(o) => {
                o.insert(key, v);
            }
            _ => {}
        }
    }
    blocks
}

#[cfg(target_os = "macos")]
fn scutil(keys: &[String]) -> Option<Vec<Option<serde_json::Value>>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/sbin/scutil")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_clear()
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        for k in keys {
            writeln!(stdin, "show {k}").ok()?;
        }
    }
    let out = child.wait_with_output().ok()?;
    let blocks = parse_scutil(&String::from_utf8_lossy(&out.stdout));
    (blocks.len() == keys.len()).then_some(blocks)
}

/// The active physical interface, by service order (blocking: runs `scutil` up to 3 times).
#[cfg(target_os = "macos")]
pub fn detect_active_interface() -> Option<ActiveInterface> {
    use serde_json::Value;
    let order = scutil(&["Setup:/Network/Global/IPv4".into()])?;
    let services: Vec<String> = order.first()?.as_ref()?["ServiceOrder"]
        .as_array()?
        .iter()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let mut keys = Vec::new();
    for s in &services {
        keys.push(format!("Setup:/Network/Service/{s}/Interface"));
        keys.push(format!("State:/Network/Service/{s}/IPv4"));
        keys.push(format!("State:/Network/Service/{s}/IPv6"));
        keys.push(format!("State:/Network/Service/{s}/DNS"));
    }
    let info = scutil(&keys)?;
    struct Cand {
        device: String,
        hardware: String,
        ipv4: bool,
        ipv6: bool,
        dns: Vec<String>,
    }
    let usable = |state: &Option<Value>, device: &str| {
        state.as_ref().is_some_and(|s| {
            s["InterfaceName"].as_str() == Some(device)
                && s["Router"].as_str().is_some_and(|r| !r.is_empty())
                && s["Addresses"].as_array().is_some_and(|a| !a.is_empty())
        })
    };
    let mut cands = Vec::new();
    for (i, _) in services.iter().enumerate() {
        let setup = &info[i * 4];
        let Some(setup) = setup else { continue };
        let (Some(device), Some(hw)) = (setup["DeviceName"].as_str(), setup["Hardware"].as_str()) else { continue };
        if hw != "AirPort" && hw != "Ethernet" {
            continue;
        }
        let (ipv4, ipv6) = (usable(&info[i * 4 + 1], device), usable(&info[i * 4 + 2], device));
        if !ipv4 && !ipv6 {
            continue;
        }
        let dns = info[i * 4 + 3]
            .as_ref()
            .and_then(|d| d["ServerAddresses"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .take(3)
            .collect();
        cands.push(Cand { device: device.to_owned(), hardware: hw.to_owned(), ipv4, ipv6, dns });
    }
    if cands.is_empty() {
        return None;
    }
    let links = scutil(&cands.iter().map(|c| format!("State:/Network/Interface/{}/Link", c.device)).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![None; cands.len()]);
    for (c, link) in cands.into_iter().zip(links) {
        let down = link.as_ref().and_then(|l| l["Active"].as_str().map(|a| a != "TRUE")).unwrap_or(false);
        if down {
            continue;
        }
        let Ok(cname) = std::ffi::CString::new(c.device.clone()) else { continue };
        let index = unsafe { libc::if_nametoindex(cname.as_ptr()) };
        if index == 0 {
            continue;
        }
        return Some(ActiveInterface { name: c.device, index, hardware: c.hardware, ipv4: c.ipv4, ipv6: c.ipv6, dns: c.dns });
    }
    None
}

#[cfg(not(target_os = "macos"))]
pub fn detect_active_interface() -> Option<ActiveInterface> {
    None
}

/// Public unicast addresses only; VPN fake-IP ranges (198.18.0.0/15) are dropped.
pub fn usable_addr(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || (o[0] == 198 && (o[1] & 0xfe) == 18)
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64))
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            !(v6.is_loopback() || v6.is_unspecified() || v6.is_multicast() || (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xfe00) == 0xfc00)
                && v6.to_ipv4_mapped().is_none()
        }
    }
}

const DNS_TTL: Duration = Duration::from_secs(300);

#[derive(Default)]
struct DnsCache {
    hosts: HashMap<String, (Instant, Vec<SocketAddr>)>,
}

#[derive(Clone)]
struct IfaceResolver {
    iface: ActiveInterface,
    cache: Arc<Mutex<DnsCache>>,
}

async fn dig(host: &str, server: &str, family: &str) -> Vec<IpAddr> {
    let mut cmd = tokio::process::Command::new("/usr/bin/dig");
    cmd.kill_on_drop(true).env_clear().env("HOME", "/var/empty");
    let server = server.split('%').next().unwrap_or(server);
    cmd.args([&format!("@{server}"), "+time=1", "+tries=1", "+short", "-q", host, "-t", family]);
    match tokio::time::timeout(Duration::from_millis(1500), cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).lines().filter_map(|l| l.trim().parse().ok()).take(16).collect(),
        _ => Vec::new(),
    }
}

impl IfaceResolver {
    async fn lookup(&self, host: &str) -> Vec<SocketAddr> {
        let system = tokio::time::timeout(Duration::from_millis(1500), tokio::net::lookup_host((host, 0)));
        let mut queries = Vec::new();
        for s in &self.iface.dns {
            if self.iface.ipv6 {
                queries.push(dig(host, s, "AAAA"));
            }
            if self.iface.ipv4 {
                queries.push(dig(host, s, "A"));
            }
        }
        let (system, extra) = tokio::join!(system, futures_util::future::join_all(queries));
        let mut out: Vec<SocketAddr> = Vec::new();
        let sys: Vec<SocketAddr> = system.ok().and_then(Result::ok).map(|i| i.collect()).unwrap_or_default();
        // The interface's own DNS first: it answers for the physical path's location.
        for ip in extra.into_iter().flatten().chain(sys.into_iter().map(|a| a.ip())) {
            let ip = match ip {
                IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(IpAddr::V6(v6)),
                v4 => v4,
            };
            if !usable_addr(&ip) || (ip.is_ipv6() && !self.iface.ipv6) || (ip.is_ipv4() && !self.iface.ipv4) {
                continue;
            }
            let a = SocketAddr::new(ip, 0);
            if !out.contains(&a) {
                out.push(a);
            }
        }
        out
    }
}

#[cfg(target_os = "macos")]
async fn tls_handshake(index: u32, host: &str, addr: SocketAddr) -> std::io::Result<()> {
    use std::num::NonZeroU32;
    use std::sync::LazyLock;
    use tokio_rustls::rustls;
    static TLS: LazyLock<tokio_rustls::TlsConnector> = LazyLock::new(|| {
        let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        tokio_rustls::TlsConnector::from(Arc::new(cfg))
    });
    let idx = NonZeroU32::new(index).ok_or_else(|| std::io::Error::other("bad interface index"))?;
    let sock = if addr.is_ipv6() {
        let s = tokio::net::TcpSocket::new_v6()?;
        socket2::SockRef::from(&s).bind_device_by_index_v6(Some(idx))?;
        s
    } else {
        let s = tokio::net::TcpSocket::new_v4()?;
        socket2::SockRef::from(&s).bind_device_by_index_v4(Some(idx))?;
        s
    };
    let name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(std::io::Error::other)?;
    let tcp = sock.connect(addr).await?;
    let _ = TLS.connect(name, tcp).await?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
async fn tls_handshake(_: u32, _: &str, _: SocketAddr) -> std::io::Result<()> {
    Err(std::io::Error::other("unsupported"))
}

/// Put the first address that completes a TLS handshake on the bound interface first.
async fn order_by_tls(index: u32, host: &str, port: u16, cands: Vec<SocketAddr>) -> Vec<SocketAddr> {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    if cands.len() < 2 {
        return cands;
    }
    let mut race: FuturesUnordered<_> = cands
        .iter()
        .take(8)
        .map(|a| {
            let a = *a;
            async move { (a, tokio::time::timeout(Duration::from_secs(3), tls_handshake(index, host, SocketAddr::new(a.ip(), port))).await) }
        })
        .collect();
    let mut best = None;
    while let Some((a, r)) = race.next().await {
        if matches!(r, Ok(Ok(()))) {
            best = Some(a);
            break;
        }
    }
    match best {
        Some(b) => std::iter::once(b).chain(cands.into_iter().filter(|a| *a != b)).collect(),
        None => cands,
    }
}

impl reqwest::dns::Resolve for IfaceResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let me = self.clone();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            if let Some((t, v)) = me.cache.lock().unwrap().hosts.get(&host) {
                if t.elapsed() < DNS_TTL && !v.is_empty() {
                    return Ok(Box::new(v.clone().into_iter()) as reqwest::dns::Addrs);
                }
            }
            let cands = me.lookup(&host).await;
            if cands.is_empty() {
                return Err(Box::new(std::io::Error::other(format!("no usable address for {host} on {}", me.iface.name))) as Box<dyn std::error::Error + Send + Sync>);
            }
            let ordered = order_by_tls(me.iface.index, &host, 443, cands).await;
            me.cache.lock().unwrap().hosts.insert(host, (Instant::now(), ordered.clone()));
            Ok(Box::new(ordered.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

const IFACE_TTL: Duration = Duration::from_secs(10);

/// Lazily built, interface-bound clients, rebuilt when the active interface changes.
#[derive(Default)]
pub(crate) struct DirectNet {
    state: tokio::sync::Mutex<DirectState>,
}

#[derive(Default)]
struct DirectState {
    checked: Option<Instant>,
    iface: Option<ActiveInterface>,
    client: Option<(ActiveInterface, Client)>,
}

impl DirectNet {
    /// The interface-bound client, or None if no active physical interface was found.
    pub(crate) async fn client(&self) -> Option<(ActiveInterface, Client)> {
        let mut st = self.state.lock().await;
        if st.checked.is_none_or(|t| t.elapsed() > IFACE_TTL) {
            st.iface = tokio::task::spawn_blocking(detect_active_interface).await.ok().flatten();
            st.checked = Some(Instant::now());
        }
        let iface = st.iface.clone()?;
        if let Some((i, c)) = &st.client {
            if *i == iface {
                return Some((i.clone(), c.clone()));
            }
        }
        let c = build_direct_client(&iface).ok()?;
        st.client = Some((iface.clone(), c.clone()));
        Some((iface, c))
    }

    pub(crate) async fn interface(&self) -> Option<ActiveInterface> {
        self.client().await.map(|(i, _)| i)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn build_direct_client(iface: &ActiveInterface) -> reqwest::Result<Client> {
    Client::builder()
        .use_rustls_tls()
        .no_proxy()
        .https_only(true)
        .interface(&iface.name)
        .dns_resolver(Arc::new(IfaceResolver { iface: iface.clone(), cache: Default::default() }))
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .connect_timeout(Duration::from_secs(3))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn build_direct_client(_: &ActiveInterface) -> reqwest::Result<Client> {
    Client::builder().build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scutil_blocks_parse() {
        let out = "<dictionary> {\n  ServiceOrder : <array> {\n    0 : A\n    1 : B\n  }\n}\n  No such key\n<dictionary> {\n  AdditionalRoutes : <array> {\n    0 : <dictionary> {\n      DestinationAddress : 1.2.3.4\n    }\n  }\n  Addresses : <array> {\n    0 : 192.168.3.103\n  }\n  InterfaceName : en0\n  Router : 192.168.3.1\n}\n";
        let b = parse_scutil(out);
        assert_eq!(b.len(), 3);
        assert_eq!(b[0].as_ref().unwrap()["ServiceOrder"][1], "B");
        assert!(b[1].is_none());
        let s = b[2].as_ref().unwrap();
        assert_eq!(s["InterfaceName"], "en0");
        assert_eq!(s["Addresses"][0], "192.168.3.103");
        assert_eq!(s["AdditionalRoutes"][0]["DestinationAddress"], "1.2.3.4");
    }

    #[test]
    fn fake_ip_and_private_addresses_are_dropped() {
        for bad in ["198.18.0.5", "198.19.255.1", "10.0.0.1", "192.168.3.1", "127.0.0.1", "100.64.1.1", "fe80::1", "fd00::1", "::1"] {
            assert!(!usable_addr(&bad.parse().unwrap()), "{bad}");
        }
        for good in ["47.251.169.173", "3.173.21.63", "240b:4004:b:181c::1", "198.20.0.1"] {
            assert!(usable_addr(&good.parse().unwrap()), "{good}");
        }
    }

    #[test]
    fn network_mode_parses() {
        assert_eq!("auto".parse::<HttpNetwork>().unwrap(), HttpNetwork::Auto);
        assert_eq!("direct".parse::<HttpNetwork>().unwrap(), HttpNetwork::ActiveInterface);
        assert!("vpn".parse::<HttpNetwork>().is_err());
    }
}

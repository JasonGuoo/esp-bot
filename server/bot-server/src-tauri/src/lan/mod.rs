mod discovery;
mod protocol;
mod socket;

use axum::{routing::any, Router};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpListener;
use uuid::Uuid;

pub use protocol::SERVICE_TYPE;
pub type EventSink = Arc<dyn Fn(LanEvent) + Send + Sync + 'static>;

#[derive(Clone, Debug, Serialize)]
pub struct Bot {
    pub device_id: String,
    pub name: String,
    pub peer: String,
    pub connected_at_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct LanSnapshot {
    pub running: bool,
    pub service_type: String,
    pub server_id: String,
    pub instance_name: String,
    pub port: Option<u16>,
    pub bots: Vec<Bot>,
    pub error: Option<String>,
}

impl Default for LanSnapshot {
    fn default() -> Self {
        Self {
            running: false,
            service_type: SERVICE_TYPE.to_owned(),
            server_id: String::new(),
            instance_name: String::new(),
            port: None,
            bots: Vec::new(),
            error: None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct LanEvent {
    pub kind: String,
    pub device_id: Option<String>,
    pub detail: String,
    pub at_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

impl LanEvent {
    pub fn new(kind: &str, device_id: Option<&str>, detail: &str, payload: Option<Value>) -> Self {
        Self {
            kind: kind.to_owned(),
            device_id: device_id.map(str::to_owned),
            detail: detail.to_owned(),
            at_ms: now_ms(),
            payload,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

struct BotSession {
    bot: Bot,
    session_id: String,
}

#[derive(Default)]
pub struct LanState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    snapshot: LanSnapshot,
    connections: HashMap<String, BotSession>,
}

impl LanState {
    pub fn snapshot(&self) -> LanSnapshot {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).snapshot.clone()
    }

    pub fn fail(&self, error: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.snapshot.running = false;
        inner.snapshot.error = Some(error.to_owned());
        inner.snapshot.bots.clear();
        inner.connections.clear();
    }

    fn started(&self, server_id: &str, port: u16) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.snapshot.running = true;
        inner.snapshot.server_id = server_id.to_owned();
        inner.snapshot.instance_name = format!("ESP Bot Server {}", &server_id[..8]);
        inner.snapshot.port = Some(port);
        inner.snapshot.error = None;
    }

    fn connected(&self, bot: Bot, session_id: String) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.connections.insert(bot.device_id.clone(), BotSession { bot, session_id });
        sync_bots(&mut inner);
    }

    fn disconnected(&self, device_id: &str, session_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.connections.get(device_id).is_some_and(|s| s.session_id == session_id) {
            inner.connections.remove(device_id);
            sync_bots(&mut inner);
            true
        } else {
            false
        }
    }
}

fn sync_bots(inner: &mut Inner) {
    inner.snapshot.bots = inner.connections.values().map(|s| s.bot.clone()).collect();
    inner.snapshot.bots.sort_by(|a, b| a.device_id.cmp(&b.device_id));
}

#[derive(Clone)]
struct RouteState {
    state: Arc<LanState>,
    sink: EventSink,
    server_id: String,
}

/// Starts independently of the Tauri window and accepts all well-formed Bot
/// connections on local interfaces. No LLM/tools/face command execution here.
pub async fn serve(state: Arc<LanState>, sink: EventSink) -> Result<(), String> {
    let listener = TcpListener::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("cannot bind LAN listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("cannot read listener port: {e}"))?
        .port();
    let server_id = Uuid::new_v4().to_string();
    let mdns = discovery::advertise(&server_id, port)?;
    let routes = RouteState {
        state: Arc::clone(&state),
        sink: Arc::clone(&sink),
        server_id: server_id.clone(),
    };
    let router = Router::new()
        .route(protocol::WS_PATH, any(socket::upgrade))
        .with_state(routes);
    state.started(&server_id, port);
    sink(LanEvent::new("server_started", None, &format!("mDNS and WebSocket on port {port}"), None));

    let result = axum::serve(listener, router.into_make_service_with_connect_info::<std::net::SocketAddr>()).await;
    let _ = mdns.shutdown();
    result.map_err(|e| format!("LAN listener failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_disconnect_does_not_remove_new_session() {
        let state = LanState::default();
        let bot = Bot { device_id: "b1".into(), name: "Bot".into(), peer: "192.168.1.2".into(), connected_at_ms: 1 };
        state.connected(bot.clone(), "old".into());
        state.connected(bot, "new".into());
        assert!(!state.disconnected("b1", "old"));
        assert_eq!(state.snapshot().bots.len(), 1);
        assert!(state.disconnected("b1", "new"));
        assert!(state.snapshot().bots.is_empty());
    }
}

//! Mac-owned OpenCode 2 harness. The official process owns model adapters,
//! tool execution and MCP. This module owns turn lifetime, routing, local
//! approval presentation and a narrow, authenticated loopback connection.
//!
//! Protocol source: anomalyco/opencode v2.2.5, packages/protocol/src/groups.
//! V1 `/session/:id/message`, `modelID` and `OPENCODE_SERVER_USERNAME` are not
//! silently mixed into the native V2 API.

mod config;
mod http;
mod routing;
mod turn;

pub use config::{HarnessConfig, ModelRoute, SOURCE_VERSION};

use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex as StdMutex, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Client, Method};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{process::{Child, Command}, sync::{watch, Mutex}, time::Instant};
use uuid::Uuid;

use config::AGENT_ID;
use http::Api;
use routing::RoutingState;

pub type HarnessEventSink = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnRequest {
    pub turn_id: String,
    pub conversation_id: String,
    pub user_text: String,
    pub system_prompt: String,
    #[serde(default)]
    pub response_schema: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HarnessReply {
    pub text: String,
    pub provider_id: String,
    pub model_id: String,
    pub session_id: String,
    pub structured: Option<Value>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct SessionLedger {
    conversations: BTreeMap<String, Conversation>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Conversation {
    current_session: String,
    /// Deleting a native V2 root also deletes all of its fork children.
    roots: Vec<String>,
    updated_at_ms: u64,
    delete_pending: bool,
    context_fingerprint: String,
    abandoned: bool,
}

struct ActiveTurn {
    conversation_id: String,
    session_id: StdMutex<Option<String>>,
    cancel: watch::Sender<bool>,
}

struct ManagedProcess {
    child: Child,
    group: Option<u32>,
}

impl ManagedProcess {
    fn signal_group(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.group.and_then(|id| i32::try_from(id).ok()).filter(|id| *id > 0) {
            // The child was placed in its own process group before spawn.
            // This targets only this managed OpenCode and inherited MCP children.
            unsafe { libc::kill(-pid, libc::SIGTERM); }
        }
    }

    fn kill_group(&self) {
        #[cfg(unix)]
        if let Some(pid) = self.group.and_then(|id| i32::try_from(id).ok()).filter(|id| *id > 0) {
            unsafe { libc::kill(-pid, libc::SIGKILL); }
        }
    }

    async fn shutdown(&mut self) {
        self.signal_group();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        // Finish cleanup of an MCP child that ignored SIGTERM.
        self.kill_group();
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        self.group = None;
    }
}

impl Drop for ManagedProcess {
    fn drop(&mut self) {
        self.kill_group();
        let _ = self.child.start_kill();
    }
}

pub struct Harness {
    config: HarnessConfig,
    data_dir: PathBuf,
    workspace: PathBuf,
    client: Client,
    process: Mutex<Option<ManagedProcess>>,
    ready: AtomicBool,
    password: RwLock<String>,
    version: RwLock<Option<String>>,
    last_error: RwLock<Option<String>>,
    // One active conversation turn keeps quota use and provider failover bounded.
    turn_gate: Mutex<()>,
    active: StdMutex<HashMap<String, Arc<ActiveTurn>>>,
    pending_permissions: StdMutex<BTreeMap<String, Value>>,
    sessions: StdMutex<SessionLedger>,
    routing: StdMutex<RoutingState>,
    sink: HarnessEventSink,
}

/// A network client or Tauri caller can drop a future without calling cancel.
/// Keep ownership cleanup independent of normal async return paths.
struct TurnGuard<'a> {
    harness: &'a Harness,
    turn_id: String,
    active: Arc<ActiveTurn>,
    armed: bool,
}

impl TurnGuard<'_> {
    fn clear(&self) {
        if let Ok(mut turns) = self.harness.active.lock() { turns.remove(&self.turn_id); }
        if let Ok(mut requests) = self.harness.pending_permissions.lock() {
            requests.retain(|_, value| value["turn_id"] != self.turn_id);
        }
    }

    fn finish(&mut self, succeeded: bool) -> Result<(), String> {
        {
            // Serialize success persistence with cancel's retirement. An app
            // crash before this write leaves the root retired after restart.
            let mut turns = self.harness.active.lock().map_err(|_| "opencode_state: active lock")?;
            if succeeded && !*self.active.cancel.borrow() {
                let mut sessions = self.harness.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
                if let Some(record) = sessions.conversations.get_mut(&self.active.conversation_id) {
                    record.abandoned = false;
                    if let Err(error) = write_state(&self.harness.data_dir.join("sessions.json"), &*sessions) {
                        if let Some(record) = sessions.conversations.get_mut(&self.active.conversation_id) { record.abandoned = true; }
                        return Err(error);
                    }
                }
            }
            turns.remove(&self.turn_id);
        }
        if let Ok(mut requests) = self.harness.pending_permissions.lock() {
            requests.retain(|_, value| value["turn_id"] != self.turn_id);
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for TurnGuard<'_> {
    fn drop(&mut self) {
        if !self.armed { return; }
        self.active.cancel.send_replace(true);
        self.clear();
        if let Ok(mut sessions) = self.harness.sessions.lock() {
            if let Some(record) = sessions.conversations.get_mut(&self.active.conversation_id) {
                record.abandoned = true;
                if write_state(&self.harness.data_dir.join("sessions.json"), &*sessions).is_err() {
                    self.harness.emit(json!({"kind":"harness_state_error","code":"abandoned_turn_not_persisted"}));
                }
            }
        }
        let session = self.active.session_id.lock().ok().and_then(|value| value.clone());
        if let (Some(session), Ok(api)) = (session, self.harness.api()) {
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = api.json(Method::POST, &format!("/api/session/{session}/interrupt?resume=false"), None).await;
                });
            } else if let Ok(process) = self.harness.process.try_lock() {
                if let Some(managed) = process.as_ref() { managed.signal_group(); }
            }
        }
    }
}

impl Harness {
    /// Creates state, not a subprocess. Starting models remains a console action.
    pub fn new(config: HarnessConfig, data_dir: PathBuf, sink: HarnessEventSink) -> Result<Arc<Self>, String> {
        config.validate()?;
        private_dir(&data_dir)?;
        let workspace = config.workspace(&data_dir);
        if config.working_directory.is_some() {
            if !workspace.is_dir() { return Err("opencode_config: working_directory does not exist".into()); }
        } else {
            private_dir(&workspace)?;
        }
        if config.inherit_config.as_ref().is_some_and(|path| !path.is_file()) {
            return Err("opencode_config: inherit_config does not name an existing file".into());
        }
        let sessions: SessionLedger = read_state(&data_dir.join("sessions.json"))?;
        let routing: RoutingState = read_state(&data_dir.join("routing.json"))?;
        if sessions.conversations.len() > 256 {
            return Err("opencode_state: too many retained conversations; delete old conversations before starting".into());
        }
        let client = Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5)).build()
            .map_err(|_| "opencode_config: cannot construct HTTP client".to_owned())?;
        Ok(Arc::new(Self {
            config, data_dir, workspace, client,
            process: Mutex::new(None), ready: AtomicBool::new(false),
            password: RwLock::new(String::new()), version: RwLock::new(None), last_error: RwLock::new(None),
            turn_gate: Mutex::new(()), active: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(BTreeMap::new()),
            sessions: StdMutex::new(sessions), routing: StdMutex::new(routing), sink,
        }))
    }

    pub async fn start(&self) -> Result<Value, String> {
        let mut process = self.process.lock().await;
        if self.ready.load(Ordering::Acquire) {
            if let Some(managed) = process.as_mut() {
                if managed.child.try_wait().map_err(|_| "opencode_process: cannot inspect process")?.is_some() {
                    self.ready.store(false, Ordering::Release);
                    *process = None;
                } else {
                    return self.info().await.map(|info| json!({"running":true,"version":info["version"]}));
                }
            } else {
                return self.info().await.map(|info| json!({"running":true,"version":info["version"]}));
            }
        }
        // A caller may have dropped an earlier startup future after spawn but
        // before readiness. Reclaim that owned process before rotating its
        // password or binding its port again.
        if let Some(mut interrupted_start) = process.take() {
            interrupted_start.shutdown().await;
        }
        let password = match std::env::var(&self.config.password_env).ok().filter(|s| !s.is_empty()) {
            Some(password) => password,
            None if self.config.mode == "managed" => format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
            None => return Err(format!("opencode_auth: set {} before attaching", self.config.password_env)),
        };
        if password.len() < 16 || password.len() > 4096 {
            return Err("opencode_auth: server password must be 16..4096 bytes".into());
        }
        *self.password.write().map_err(|_| "opencode_state: password lock")? = password.clone();
        let overlay = self.config.overlay();
        write_state(&self.data_dir.join("opencode.generated.json"), &overlay)?;
        if self.config.mode == "managed" {
            let url = self.config.validate()?;
            let host = url.host_str().unwrap_or("127.0.0.1").trim_start_matches('[').trim_end_matches(']');
            let port = url.port().ok_or("opencode_config: port is required")?;
            // Reject port collisions instead of accidentally attaching to another process.
            let address: std::net::SocketAddr = format!("{}:{port}", if host == "::1" { "[::1]" } else { host })
                .parse().map_err(|_| "opencode_config: invalid bind address")?;
            drop(std::net::TcpListener::bind(address).map_err(|_| "opencode_process: configured port is already in use")?);
            let mut effective = match std::env::var("OPENCODE_CONFIG_CONTENT").ok().filter(|s| !s.trim().is_empty()) {
                Some(content) => serde_json::from_str::<Value>(&content).map_err(|_| {
                    "opencode_config: existing OPENCODE_CONFIG_CONTENT must be JSON; put JSONC in inherit_config"
                })?,
                None => json!({}),
            };
            if !effective.is_object() { return Err("opencode_config: inline configuration must be an object".into()); }
            merge(&mut effective, overlay);
            let mut command = Command::new(&self.config.executable);
            command.args(["serve", "--hostname", host, "--port", &port.to_string()])
                .current_dir(&self.workspace)
                .env("OPENCODE_PASSWORD", password)
                .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&effective).map_err(|_| "opencode_config: cannot encode overlay")?)
                .env("OPENCODE_CLIENT", "esp-bot-xiaodai")
                // Explicit/global MCP config is inherited. Unrelated ancestor
                // project agents, scripts and instructions are not loaded.
                .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "true")
                .env_remove("OPENCODE_SIMULATE")
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
                .kill_on_drop(true);
            if let Some(path) = &self.config.inherit_config { command.env("OPENCODE_CONFIG", path); }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.as_std_mut().process_group(0);
            }
            let child = command.spawn().map_err(|_| {
                "opencode_process: cannot launch OpenCode; install v2.2.5+ or configure an absolute executable path"
            })?;
            let group = child.id();
            *process = Some(ManagedProcess { child, group });
        }
        let deadline = Instant::now() + Duration::from_millis(self.config.startup_timeout_ms);
        let result = loop {
            if let Some(managed) = process.as_mut() {
                match managed.child.try_wait() {
                    Ok(Some(_)) => break Err("opencode_process: process exited during startup; see the manual launch instructions".to_owned()),
                    Err(_) => break Err("opencode_process: cannot inspect startup process".to_owned()),
                    Ok(None) => (),
                }
            }
            match self.info().await {
                Ok(info) => {
                    let version = info["version"].as_str().unwrap_or("");
                    if !compatible_version(version) {
                        break Err(format!("opencode_version: native API requires 2.2.5 <= version < 3; got {version}"));
                    }
                    *self.version.write().map_err(|_| "opencode_state: version lock")? = Some(version.into());
                    break Ok(info);
                }
                Err(error) if self.config.mode == "attach" => break Err(error),
                Err(_) if Instant::now() >= deadline => break Err("opencode_start_timeout: server did not become ready".into()),
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        };
        match result {
            Ok(info) => {
                self.ready.store(true, Ordering::Release);
                *self.last_error.write().map_err(|_| "opencode_state: error lock")? = None;
                self.emit(json!({"kind":"harness_started","version":info["version"],"mode":self.config.mode}));
                Ok(json!({"running":true,"version":info["version"],"mode":self.config.mode}))
            }
            Err(error) => {
                self.ready.store(false, Ordering::Release);
                if let Some(mut managed) = process.take() {
                    managed.shutdown().await;
                }
                *self.last_error.write().map_err(|_| "opencode_state: error lock")? = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn stop(&self) -> Result<(), String> {
        let mut process = self.process.lock().await;
        self.ready.store(false, Ordering::Release);
        let turns = self.active.lock().map_err(|_| "opencode_state: active lock")?
            .keys().cloned().collect::<Vec<_>>();
        for turn in turns { let _ = self.cancel(&turn).await; }
        if let Some(mut managed) = process.take() { managed.shutdown().await; }
        self.emit(json!({"kind":"harness_stopped"}));
        Ok(())
    }

    pub async fn status(&self) -> Value {
        let running = self.ready.load(Ordering::Acquire);
        let health = if running { self.info().await } else { Err("not started".into()) };
        let routing = self.routing.lock().ok().map(|r| r.clone()).unwrap_or_default();
        let now = now_ms();
        let routes = self.config.routes.iter().map(|route| {
            let state = routing.routes.get(&route.id);
            json!({
                "id":route.id,"label":route.label,"enabled":route.enabled,
                "provider_id":route.provider_id,"model_id":route.model_id,
                "credential_env":route.api_key_env,
                "credential_env_present":route.api_key_env.as_ref().map(|key| std::env::var(key).is_ok_and(|s| !s.is_empty())),
                "inherited_provider":route.base_url.is_none(),
                "last_failure":state.map(|s| s.last_failure),
                "retry_at_ms":state.map(|s| s.retry_at_ms),
                "cooling_down":state.is_some_and(|s| s.retry_at_ms > now),
                "reset_is_provider_hint":state.is_some_and(|s| s.reset_is_provider_hint),
                "remaining_quota":null
            })
        }).collect::<Vec<_>>();
        let (mcp, mcp_error) = if health.is_ok() {
            match self.api().and_then(|api| Ok((api, location_path("/api/mcp", &self.workspace)))) {
                Ok((api, path)) => match api.json(Method::GET, &path, None).await {
                    Ok(value) => (self.mcp_summary(&value), None),
                    Err(error) => (Value::Null, Some(error.public_message())),
                },
                Err(error) => (Value::Null, Some(error)),
            }
        } else { (Value::Null, None) };
        json!({
            "harness":"opencode","source_version":SOURCE_VERSION,
            "running":running,"healthy":health.is_ok(),
            "version":self.version.read().ok().and_then(|v| v.clone()),
            "mode":self.config.mode,"base_url":self.config.base_url,
            "working_directory":self.workspace,
            "health_error":health.err(),
            "last_error":self.last_error.read().ok().and_then(|v| v.clone()),
            "routes":routes,"mcp":mcp,"mcp_error":mcp_error,
            "active_turns":self.active.lock().map(|m| m.len()).unwrap_or(0),
            "retained_conversations":self.sessions.lock().map(|m| m.conversations.len()).unwrap_or(0),
            "pending_permissions":self.pending_permissions.lock().map(|m| m.len()).unwrap_or(0)
        })
    }

    pub async fn run(&self, request: TurnRequest) -> Result<HarnessReply, String> {
        validate_turn(&request)?;
        if !self.ready.load(Ordering::Acquire) { return Err("opencode_not_started: start the harness first".into()); }
        let _gate = self.turn_gate.try_lock().map_err(|_| "opencode_busy: another turn is active")?;
        let (cancel, mut cancel_rx) = watch::channel(false);
        let active = Arc::new(ActiveTurn {
            conversation_id: request.conversation_id.clone(), session_id: StdMutex::new(None), cancel,
        });
        {
            let mut turns = self.active.lock().map_err(|_| "opencode_state: active lock")?;
            // Pair admission with stop's active snapshot. If stop has already
            // cleared readiness it must not miss a just-created active turn.
            if !self.ready.load(Ordering::Acquire) { return Err("opencode_not_started: harness is stopping".into()); }
            if turns.contains_key(&request.turn_id) { return Err("duplicate_turn: turn is already active".into()); }
            turns.insert(request.turn_id.clone(), Arc::clone(&active));
        }
        let mut guard = TurnGuard { harness: self, turn_id: request.turn_id.clone(), active: Arc::clone(&active), armed: true };
        let deadline = Instant::now() + Duration::from_millis(self.config.turn_timeout_ms);
        let outcome = tokio::select! {
            biased;
            _ = cancel_rx.changed() => Err("turn_cancelled: active turn was interrupted".to_owned()),
            result = tokio::time::timeout_at(deadline, self.run_inner(&request, &active)) => {
                result.unwrap_or_else(|_| Err("turn_timeout: OpenCode exceeded the whole-turn budget".into()))
            }
        };
        // An HTTP future being dropped does not stop a durably admitted prompt.
        // Interrupt the upstream loop on every local cancellation/failure.
        let mut cleanup_confirmed = true;
        if outcome.is_err() {
            // Retire failed/cancelled roots even after a confirmed interrupt:
            // another cancel caller may still have an interrupt in flight.
            cleanup_confirmed = self.retire_conversation(&active.conversation_id).is_ok();
            let session = active.session_id.lock().ok().and_then(|s| s.clone());
            if let Some(session) = session {
                cleanup_confirmed &= self.interrupt_session(&session).await.is_ok();
            }
        }
        if cleanup_confirmed { guard.finish(outcome.is_ok())?; }
        // A cancel that raced a terminal event still suppresses the reply.
        if *active.cancel.borrow() {
            self.emit(json!({"kind":"harness_turn_cancelled","turn_id":request.turn_id}));
            return Err("turn_cancelled: late reply suppressed".into());
        }
        if let Err(error) = &outcome {
            if let Ok(mut last) = self.last_error.write() { *last = Some(error.clone()); }
            self.emit(json!({"kind":"harness_turn_failed","turn_id":request.turn_id,"code":error.split(':').next()}));
        }
        outcome
    }

    pub async fn cancel(&self, turn_id: &str) -> Result<(), String> {
        let (active, retirement) = {
            // Keep active membership locked until the retirement flag is set.
            // A completing turn cannot release its gate between those steps.
            let turns = self.active.lock().map_err(|_| "opencode_state: active lock")?;
            let Some(active) = turns.get(turn_id).cloned() else { return Ok(()) };
            let retirement = self.retire_conversation(&active.conversation_id);
            active.cancel.send_replace(true);
            (active, retirement)
        };
        let session = active.session_id.lock().map_err(|_| "opencode_state: session lock")?.clone();
        if let Some(session) = session { self.interrupt_session(&session).await?; }
        retirement
    }

    pub async fn permissions(&self) -> Result<Value, String> {
        let pending = self.pending_permissions.lock().map_err(|_| "opencode_state: permission lock")?;
        Ok(json!(pending.values().cloned().collect::<Vec<_>>()))
    }

    /// Called only from the local parent console. A child utterance, face or
    /// voice match is never a permission credential. No persistent `always`.
    pub async fn reply_permission(&self, session_id: &str, request_id: &str, allow: bool) -> Result<(), String> {
        checked_id(session_id, "ses")?;
        checked_id(request_id, "per")?;
        let pending = self.pending_permissions.lock().map_err(|_| "opencode_state: permission lock")?
            .get(request_id).cloned().ok_or("permission_expired: request is no longer pending")?;
        if pending["session_id"].as_str() != Some(session_id) {
            return Err("permission_mismatch: session does not own this request".into());
        }
        let turn_id = pending["turn_id"].as_str().ok_or("permission_mismatch: missing turn")?;
        let active = self.active.lock().map_err(|_| "opencode_state: active lock")?
            .get(turn_id).cloned().ok_or("permission_expired: turn has ended")?;
        if *active.cancel.borrow() { return Err("permission_expired: turn was cancelled".into()); }
        let action = pending["action"].as_str().unwrap_or("");
        if allow && !self.config.allowed_mcp_action(action) {
            return Err("permission_denied: only configured browser/search MCP actions can be approved here".into());
        }
        self.api()?.json(Method::POST, &format!("/api/session/{session_id}/permission/{request_id}/reply"),
            Some(&json!({"decision":if allow { "once" } else { "reject" }}))).await
            .map_err(|e| e.public_message())?;
        self.pending_permissions.lock().map_err(|_| "opencode_state: permission lock")?.remove(request_id);
        self.emit(json!({"kind":"harness_permission_replied","turn_id":turn_id,"request_id":request_id,"allow":allow}));
        Ok(())
    }

    pub async fn reset_route(&self, route_id: &str) -> Result<(), String> {
        if !self.config.routes.iter().any(|route| route.id == route_id) { return Err("unknown_route".into()); }
        let mut routing = self.routing.lock().map_err(|_| "opencode_state: route lock")?;
        routing.routes.remove(route_id);
        write_state(&self.data_dir.join("routing.json"), &*routing)?;
        self.emit(json!({"kind":"harness_route_reset","route_id":route_id}));
        Ok(())
    }

    /// Retains the ledger if the official deletion API cannot be reached, so
    /// the console can retry instead of falsely claiming complete deletion.
    pub async fn forget_conversation(&self, conversation_id: &str) -> Result<(), String> {
        let turns = self.active.lock().map_err(|_| "opencode_state: active lock")?.iter()
            .filter(|(_, turn)| turn.conversation_id == conversation_id).map(|(id, _)| id.clone()).collect::<Vec<_>>();
        for id in turns { self.cancel(&id).await?; }
        let _gate = tokio::time::timeout(Duration::from_secs(30), self.turn_gate.lock()).await
            .map_err(|_| "opencode_busy: turn cleanup has not completed")?;
        let roots = {
            let mut ledger = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
            let Some(conversation) = ledger.conversations.get_mut(conversation_id) else { return Ok(()) };
            conversation.delete_pending = true;
            let roots = conversation.roots.clone();
            write_state(&self.data_dir.join("sessions.json"), &*ledger)?;
            roots
        };
        // Deletion is useful after an app restart, before a parent has started
        // any models. Temporarily start/attach only the official local server;
        // no prompt or provider call is involved.
        let started_for_deletion = !self.ready.load(Ordering::Acquire);
        if started_for_deletion { self.start().await?; }
        let deleted = async {
            let api = self.api()?;
            for root in roots {
                checked_id(&root, "ses")?;
                match api.json(Method::DELETE, &format!("/api/session/{root}"), None).await {
                    Ok(_) => (),
                    Err(error) if error.status == Some(404) => (),
                    Err(error) => return Err(format!("conversation_delete_pending: {}", error.public_message())),
                }
            }
            let mut ledger = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
            ledger.conversations.remove(conversation_id);
            write_state(&self.data_dir.join("sessions.json"), &*ledger)
        }.await;
        if started_for_deletion { let _ = self.stop().await; }
        deleted
    }

    fn api(&self) -> Result<Api, String> {
        let password = self.password.read().map_err(|_| "opencode_state: password lock")?.clone();
        if password.is_empty() { return Err("opencode_not_started: no local server credential".into()); }
        Ok(Api::new(self.client.clone(), &self.config.base_url, password, self.config.request_timeout_ms))
    }

    fn retire_conversation(&self, conversation_id: &str) -> Result<(), String> {
        let mut sessions = self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?;
        if let Some(record) = sessions.conversations.get_mut(conversation_id) {
            record.abandoned = true;
            write_state(&self.data_dir.join("sessions.json"), &*sessions)?;
        }
        Ok(())
    }

    async fn info(&self) -> Result<Value, String> {
        self.api()?.json(Method::GET, "/api/info", None).await.map_err(|e| e.public_message())
    }

    async fn interrupt_session(&self, session: &str) -> Result<(), String> {
        checked_id(session, "ses")?;
        self.api()?.json(Method::POST, &format!("/api/session/{session}/interrupt?resume=false"), None).await
            .map(|_| ()).map_err(|e| e.public_message())
    }

    fn emit(&self, event: Value) { (self.sink)(event); }

    fn mcp_summary(&self, value: &Value) -> Value {
        let servers = value.get("data").and_then(Value::as_array);
        json!(self.config.required_mcp.iter().map(|name| {
            let server = servers.and_then(|items| items.iter().find(|s| s["name"].as_str() == Some(name.as_str())));
            json!({"name":name,"configured":server.is_some(),"status":server.and_then(|s| s.pointer("/status/status")).and_then(Value::as_str)})
        }).collect::<Vec<_>>())
    }
}

fn compatible_version(version: &str) -> bool {
    let pieces = version.trim_start_matches('v').split('.')
        .map(str::parse::<u32>).collect::<Result<Vec<_>, _>>();
    matches!(pieces, Ok(ref parts) if parts.len() == 3 && parts[0] == 2 && (parts[1],parts[2]) >= (2,5))
}

fn validate_turn(request: &TurnRequest) -> Result<(), String> {
    for id in [&request.turn_id, &request.conversation_id] {
        if !config::identifier(id) { return Err("invalid_turn: IDs must be 1..128 letters, digits, _ or -".into()); }
    }
    if request.user_text.trim().is_empty() || request.user_text.chars().count() > 8_000
        || request.system_prompt.len() > 65_536
        || request.response_schema.as_ref().is_some_and(|schema| !schema.is_object() || schema.to_string().len() > 32_768)
    {
        return Err("invalid_turn: empty input or exceeded prompt/schema budget".into());
    }
    Ok(())
}

fn checked_id(id: &str, prefix: &str) -> Result<(), String> {
    if !id.starts_with(prefix) || !config::identifier(id) { return Err("opencode_protocol: invalid resource ID".into()); }
    Ok(())
}

fn location_path(path: &str, directory: &Path) -> String {
    let mut url = reqwest::Url::parse("http://127.0.0.1").expect("constant URL");
    url.set_path(path);
    url.query_pairs_mut().append_pair("directory", &directory.to_string_lossy());
    format!("{}?{}", url.path(), url.query().unwrap_or(""))
}

fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|_| "opencode_state: cannot create private state directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|_| "opencode_state: cannot protect state directory")?;
    }
    Ok(())
}

fn read_state<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(_) => return Err("opencode_state: cannot inspect persisted state".into()),
        Ok(metadata) if metadata.len() > 2 * 1024 * 1024 => return Err("opencode_state: persisted state exceeds its budget".into()),
        Ok(_) => (),
    }
    let bytes = fs::read(path).map_err(|_| "opencode_state: cannot read persisted state")?;
    serde_json::from_slice(&bytes).map_err(|_| "opencode_state: malformed state; preserve the file and repair it instead of silently losing session ownership".into())
}

fn write_state<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let data = serde_json::to_vec_pretty(value).map_err(|_| "opencode_state: cannot encode state")?;
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let result = (|| {
        let mut file = options.open(&temp).map_err(|_| "opencode_state: cannot create state file")?;
        file.write_all(&data).and_then(|_| file.sync_all()).map_err(|_| "opencode_state: cannot persist state")?;
        fs::rename(&temp, path).map_err(|_| "opencode_state: cannot replace state file")?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(temp); }
    result
}

fn merge(target: &mut Value, overlay: Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay { merge(target.entry(key).or_insert(Value::Null), value); }
        }
        (target, overlay) => *target = overlay,
    }
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis().min(u64::MAX as u128) as u64
}

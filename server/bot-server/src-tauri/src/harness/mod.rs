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
mod compaction;
pub(crate) mod secrets;

pub use config::{HarnessConfig, ModelRoute, SOURCE_VERSION};

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
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
use tokio::{process::{Child, Command}, sync::{watch, Mutex, RwLock as AsyncRwLock, Semaphore}, time::Instant};
use uuid::Uuid;

use config::AGENT_ID;
use http::Api;
use routing::RoutingState;

pub type HarnessEventSink = Arc<dyn Fn(Value) + Send + Sync>;

// A Keychain request may remain blocked in macOS after its async caller times
// out. Serialize reads across reloads so retries cannot create more prompts.
static CREDENTIAL_READ_GATE: StdMutex<()> = StdMutex::new(());
type LoadedCredentials = (BTreeMap<String, zeroize::Zeroizing<String>>, BTreeSet<String>);

struct CredentialReadGuard(Arc<AtomicBool>);
impl Drop for CredentialReadGuard {
    fn drop(&mut self) { self.0.store(true, Ordering::Release); }
}

fn load_credentials_with(
    routes: &[ModelRoute],
    uses_deepseek_search: bool,
    gate: &StdMutex<()>,
    cancelled: &AtomicBool,
    mut read: impl FnMut(&str) -> Result<Option<zeroize::Zeroizing<String>>, String>,
) -> Result<LoadedCredentials, String> {
    let _guard = gate.lock().map_err(|_| "api_key_keychain_lock_failed")?;
    let mut saved = BTreeMap::new();
    let mut present = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for route in routes.iter().filter(|route| route.base_url.is_some() &&
        (route.enabled || (uses_deepseek_search && route.api_key_env.as_deref() == Some("DEEPSEEK_API_KEY")))) {
        if cancelled.load(Ordering::Acquire) { return Err("api_key_keychain_read_cancelled".into()); }
        if let Some(account) = &route.api_key_env {
            if !seen.insert(account.clone()) { continue; }
            let key = read(account)?;
            // A timed-out OS request can finish later. Drop its secret and do
            // not start another item's prompt once the caller has gone away.
            if cancelled.load(Ordering::Acquire) { return Err("api_key_keychain_read_cancelled".into()); }
            if let Some(key) = key {
                saved.insert(account.clone(), key);
                present.insert(account.clone());
            } else if secrets::environment_present(account) {
                present.insert(account.clone());
            }
        }
    }
    Ok((saved, present))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnRequest {
    pub turn_id: String,
    pub conversation_id: String,
    pub user_text: String,
    pub system_prompt: String,
    #[serde(default)]
    pub response_schema: Option<Value>,
    #[serde(default)]
    pub clarify_on_device: bool,
    /// Stable persona/schema/policy revision; retrieval ordering is deliberately excluded.
    #[serde(default)] pub context_revision: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HarnessReply {
    pub text: String,
    pub provider_id: String,
    pub model_id: String,
    pub session_id: String,
    pub structured: Option<Value>,
    #[serde(skip)]
    pub(crate) evidence: Vec<ToolEvidence>,
}

#[derive(Clone, Debug)]
pub(crate) struct ToolEvidence {
    pub name: String,
    pub input: Value,
    pub output: String,
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
    completed_turns: u64,
    compacted_through: u64,
    compact_pending: bool,
    compact_message_id: Option<String>,
    compact_target: u64,
    compact_context_digest: Option<String>,
    compacted_at_ms: Option<u64>,
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
    data_root: PathBuf,
    workspace: PathBuf,
    client: Client,
    process: Mutex<Option<ManagedProcess>>,
    ready: AtomicBool,
    credential_envs: RwLock<BTreeSet<String>>,
    password: RwLock<String>,
    version: RwLock<Option<String>>,
    last_error: RwLock<Option<String>>,
    // One active conversation turn keeps quota use and provider failover bounded.
    turn_gate: AsyncRwLock<()>,
    conversation_gates: StdMutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
    turn_slots: Semaphore,
    active: StdMutex<HashMap<String, Arc<ActiveTurn>>>,
    pending_permissions: StdMutex<BTreeMap<String, Value>>,
    pending_questions: StdMutex<BTreeMap<String, Value>>,
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
        if let Ok(mut requests) = self.harness.pending_questions.lock() {
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
                    record.completed_turns = record.completed_turns.saturating_add(1);
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
        if let Ok(mut requests) = self.harness.pending_questions.lock() {
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
    fn conversation_gate(&self, id: &str) -> Result<Arc<Mutex<()>>, String> {
        let mut gates = self.conversation_gates.lock().map_err(|_| "opencode_state: conversation gate")?;
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(id).and_then(std::sync::Weak::upgrade) { return Ok(gate); }
        let gate = Arc::new(Mutex::new(()));
        gates.insert(id.to_owned(), Arc::downgrade(&gate));
        Ok(gate)
    }
    pub fn conversation_ids(&self) -> Result<Vec<String>, String> {
        Ok(self.sessions.lock().map_err(|_| "opencode_state: ledger lock")?.conversations.keys().cloned().collect())
    }
    pub(crate) fn workspace(&self) -> &std::path::Path { &self.workspace }
    /// Creates state, not a subprocess. Starting models remains a console action.
    pub fn new(config: HarnessConfig, data_dir: PathBuf, data_root: PathBuf, sink: HarnessEventSink) -> Result<Arc<Self>, String> {
        config.validate()?;
        private_dir(&data_dir)?;
        let workspace = config.workspace(&data_dir);
        if config.working_directory.is_some() {
            if !workspace.is_dir() { return Err("opencode_config: working_directory does not exist".into()); }
        } else {
            private_dir(&workspace)?;
        }
        let real_root = data_root.canonicalize().map_err(|_| "server_data_root_invalid")?;
        let real_workspace = workspace.canonicalize().map_err(|_| "opencode_workspace_invalid")?;
        if !real_workspace.starts_with(&real_root) {
            return Err("opencode_workspace_must_be_inside_server_directory".into());
        }
        for part in ["data", "config", "cache", "state", "tmp"] {
            private_dir(&data_root.join("runtime/xdg").join(part))?;
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
            config, data_dir, data_root, workspace, client,
            process: Mutex::new(None), ready: AtomicBool::new(false),
            credential_envs: RwLock::new(BTreeSet::new()),
            password: RwLock::new(String::new()), version: RwLock::new(None), last_error: RwLock::new(None),
            turn_gate: AsyncRwLock::new(()), conversation_gates: StdMutex::new(HashMap::new()), turn_slots: Semaphore::new(3), active: StdMutex::new(HashMap::new()),
            pending_permissions: StdMutex::new(BTreeMap::new()),
            pending_questions: StdMutex::new(BTreeMap::new()),
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
            // Resolve the user's global config before replacing the child's
            // XDG roots. OpenCode's config directory is independent of its
            // private data/cache/state directories.
            let global_config = inherited_global_config_dir()?;
            let mut command = Command::new(&self.config.executable);
            let routes = self.config.routes.clone();
            let uses_deepseek_search = self.config.required_mcp.iter().any(|name| name == "deepseek-search");
            let cancelled = Arc::new(AtomicBool::new(false));
            let _cancel_on_drop = CredentialReadGuard(Arc::clone(&cancelled));
            // A rebuilt, locally signed App can require fresh Keychain consent.
            // Bound the wait so an unattended system prompt cannot hold the
            // lifecycle gate forever. A timed-out read never launches a child;
            // its Zeroizing values are dropped when the OS request finishes.
            let credentials = tokio::time::timeout(Duration::from_secs(60), tokio::task::spawn_blocking(move || {
                load_credentials_with(&routes, uses_deepseek_search, &CREDENTIAL_READ_GATE, &cancelled, secrets::load)
            })).await.map_err(|_| "api_key_keychain_authorization_timeout: complete the macOS Keychain prompt, then retry startup")?
                .map_err(|_| "api_key_keychain_task_failed")??;
            for (account, key) in &credentials.0 { command.env(account, key.as_str()); }
            let inherited = std::env::var("no_proxy").ok().filter(|value| !value.is_empty())
                .or_else(|| std::env::var("NO_PROXY").ok()).unwrap_or_default();
            let no_proxy = self.config.provider_no_proxy(&inherited);
            if !no_proxy.is_empty() {
                command.env("NO_PROXY", &no_proxy).env("no_proxy", &no_proxy);
            }
            command.args(["serve", "--hostname", host, "--port", &port.to_string()])
                .current_dir(&self.workspace)
                .env("OPENCODE_PASSWORD", password)
                .env("OPENCODE_CONFIG_CONTENT", serde_json::to_string(&effective).map_err(|_| "opencode_config: cannot encode overlay")?)
                .env("OPENCODE_CLIENT", "esp-bot-xiaodai")
                // Keep OpenCode's sessions, logs and caches alongside 小呆's
                // memory instead of sharing the user's global OpenCode store.
                .env("XDG_DATA_HOME", self.data_root.join("runtime/xdg/data"))
                .env("XDG_CONFIG_HOME", self.data_root.join("runtime/xdg/config"))
                .env("OPENCODE_CONFIG_DIR", global_config)
                .env("XDG_CACHE_HOME", self.data_root.join("runtime/xdg/cache"))
                .env("XDG_STATE_HOME", self.data_root.join("runtime/xdg/state"))
                .env("TMPDIR", self.data_root.join("runtime/xdg/tmp"))
                // Explicit/global MCP config is inherited. Unrelated ancestor
                // project agents, scripts and instructions are not loaded.
                .env("OPENCODE_DISABLE_PROJECT_CONFIG", "true")
                .env("OPENCODE_DISABLE_CLAUDE_CODE", "true")
                .env_remove("OPENCODE_SIMULATE")
                .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(path) = &self.config.inherit_config { command.env("OPENCODE_CONFIG", path); }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.as_std_mut().process_group(0);
            }
            let mut child = command.spawn().map_err(|_| {
                "opencode_process: cannot launch OpenCode; install v2.2.5+ or configure an absolute executable path"
            })?;
            crate::diagnostics::capture_stderr(child.stderr.take(), "opencode");
            *self.credential_envs.write().map_err(|_| "opencode_state: credential lock")? = credentials.1;
            drop(command);
            drop(credentials.0);
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
        let configured_routes = self.config.routes.clone();
        let key_status = tokio::task::spawn_blocking(move || configured_routes.iter().filter_map(|route| {
            route.api_key_env.as_ref().map(|account| (account.clone(), secrets::exists(account)))
        }).collect::<BTreeMap<_, _>>()).await.unwrap_or_default();
        let routes = self.config.routes.iter().map(|route| {
            let state = routing.routes.get(&route.id);
            let saved = route.api_key_env.as_ref().and_then(|key| key_status.get(key));
            let keychain_saved = saved.is_some_and(|result| matches!(result, Ok(true)));
            let environment_present = route.api_key_env.as_ref().is_some_and(|key| secrets::environment_present(key));
            json!({
                "id":route.id,"label":route.label,"enabled":route.enabled,
                "provider_id":route.provider_id,"model_id":route.model_id,
                "credential_env":route.api_key_env,
                "credential_env_present":route.api_key_env.as_ref().map(|_| environment_present),
                "credential_present":keychain_saved || environment_present,
                "keychain_saved":keychain_saved,
                "credential_source":if keychain_saved {"keychain"} else if environment_present {"environment"} else {"none"},
                "credential_error":saved.and_then(|result| result.as_ref().err()),
                "credential_editable":cfg!(target_os = "macos") && self.config.mode == "managed" && route.base_url.is_some() && route.api_key_env.is_some(),
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

    pub fn api_key_account(&self, route_id: &str) -> Result<String, String> {
        if self.config.mode != "managed" { return Err("api_key_attach_mode: configure credentials in the external OpenCode service".into()); }
        self.config.routes.iter().find(|route| route.id == route_id && route.base_url.is_some())
            .and_then(|route| route.api_key_env.clone()).ok_or_else(|| "api_key_route_not_configurable".into())
    }

    pub async fn run(&self, request: TurnRequest) -> Result<HarnessReply, String> {
        validate_turn(&request)?;
        if !self.ready.load(Ordering::Acquire) { return Err("opencode_not_started: start the harness first".into()); }
        let _gate = self.turn_gate.read().await;
        let conversation_gate = self.conversation_gate(&request.conversation_id)?;
        let _conversation = tokio::time::timeout(Duration::from_millis(self.config.turn_timeout_ms),conversation_gate.lock()).await.map_err(|_| "opencode_conversation_queue_timeout")?;
        let _slot = self.turn_slots.try_acquire().map_err(|_| "opencode_capacity_exceeded")?;
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

    pub async fn questions(&self) -> Result<Value, String> {
        let pending = self.pending_questions.lock().map_err(|_| "opencode_state: question lock")?;
        Ok(json!(pending.values().cloned().collect::<Vec<_>>()))
    }

    pub async fn reply_question(&self, session_id: &str, form_id: &str, answer: Option<Value>) -> Result<(), String> {
        checked_id(session_id, "ses")?;
        checked_id(form_id, "frm_")?;
        let pending = self.pending_questions.lock().map_err(|_| "opencode_state: question lock")?
            .get(form_id).cloned().ok_or("question_expired")?;
        if pending["session_id"].as_str() != Some(session_id) { return Err("question_mismatch".into()); }
        let turn_id = pending["turn_id"].as_str().ok_or("question_mismatch")?;
        let active = self.active.lock().map_err(|_| "opencode_state: active lock")?
            .get(turn_id).cloned().ok_or("question_expired")?;
        if *active.cancel.borrow() { return Err("question_expired".into()); }
        let path = format!("/api/session/{session_id}/form/{form_id}");
        if let Some(answer) = answer {
            if !turn::valid_question_answer(&pending["fields"], &answer) { return Err("question_invalid_answer".into()); }
            self.api()?.json(Method::POST, &format!("{path}/reply"), Some(&json!({"answer":answer}))).await
                .map_err(|error| error.public_message())?;
        } else {
            self.api()?.json(Method::DELETE, &format!("{path}?message=User%20chose%20not%20to%20answer"), None).await
                .map_err(|error| error.public_message())?;
        }
        self.pending_questions.lock().map_err(|_| "opencode_state: question lock")?.remove(form_id);
        self.emit(json!({"kind":"harness_question_replied","turn_id":turn_id,"form_id":form_id}));
        Ok(())
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
        if allow && !self.config.allowed_approval_action(action) {
            return Err("permission_denied: this action is not enabled in the console".into());
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
        let _gate = tokio::time::timeout(Duration::from_secs(30), self.turn_gate.write()).await
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

fn inherited_global_config_dir() -> Result<PathBuf, String> {
    // Match OpenCode v2.2.5 Global.Service: an explicit OPENCODE_CONFIG_DIR
    // wins, otherwise use the parent process's XDG config root or home.
    // Preserve an explicitly supplied value instead of silently replacing it.
    if let Some(path) = std::env::var_os("OPENCODE_CONFIG_DIR") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path).join("opencode"));
    }
    // On the supported Unix hosts this also covers a GUI process without
    // HOME by looking up the current account, as Node's os.homedir() does.
    #[allow(deprecated)]
    let home = std::env::home_dir().ok_or("opencode_config: cannot locate the user's global configuration directory")?;
    Ok(home.join(".config/opencode"))
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

#[cfg(test)]
mod credential_read_tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn cancelled_read_does_not_request_the_next_key() {
        let routes = HarnessConfig::default().routes;
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let gate = StdMutex::new(());
            let mut requests = 0;
            let result = load_credentials_with(&routes, true, &gate, &worker_cancelled, |_account| {
                requests += 1;
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Some(zeroize::Zeroizing::new("test-key".to_owned())))
            });
            (result, requests)
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        cancelled.store(true, Ordering::Release);
        release_tx.send(()).unwrap();
        drop(release_tx);
        let (result, requests) = worker.join().unwrap();
        assert!(result.is_err());
        assert_eq!(requests, 1);
    }

    #[test]
    fn cancelled_waiter_does_not_start_a_keychain_request() {
        let gate = Arc::new(StdMutex::new(()));
        let held = gate.lock().unwrap();
        let worker_gate = Arc::clone(&gate);
        let worker = std::thread::spawn(move || {
            let cancelled = AtomicBool::new(true);
            let mut requests = 0;
            let result = load_credentials_with(&HarnessConfig::default().routes, true, &worker_gate, &cancelled, |_account| {
                requests += 1;
                Ok(None)
            });
            (result, requests)
        });
        drop(held);
        let (result, requests) = worker.join().unwrap();
        assert!(result.is_err());
        assert_eq!(requests, 0);
    }

    #[test]
    fn search_can_use_the_deepseek_key_when_its_model_route_is_disabled() {
        let mut routes = HarnessConfig::default().routes;
        for route in &mut routes { route.enabled = false; }
        let gate = StdMutex::new(());
        let cancelled = AtomicBool::new(false);
        let mut requested = Vec::new();
        let (saved, _) = load_credentials_with(&routes, true, &gate, &cancelled, |account| {
            requested.push(account.to_owned());
            Ok(Some(zeroize::Zeroizing::new("test-key".to_owned())))
        }).unwrap();
        assert_eq!(requested, ["DEEPSEEK_API_KEY"]);
        assert!(saved.contains_key("DEEPSEEK_API_KEY"));
    }
}

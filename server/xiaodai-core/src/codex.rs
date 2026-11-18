//! Codex engine: one long-lived `codex app-server --stdio` child, newline-delimited JSON-RPC
//! (the `jsonrpc` field is omitted, as the app-server expects). Own thin client; no code copied
//! from other projects. Protocol checked against `codex app-server generate-json-schema`.
//!
//! - Uses the user's existing Codex login (CODEX_HOME / ~/.codex); never reads token files.
//! - Threads run with cwd = workspace root, `workspace-write` sandbox (writable root = workspace
//!   root, network on), approval policy `never`. Any approval request that still arrives is
//!   accepted only if everything it touches is inside the workspace root, else declined.
//! - Unknown server requests get a JSON-RPC error so a turn can never stall.
use crate::client::AttemptMsg;
use crate::config::{BargeIn, CodexConfig};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub type BoxRead = Pin<Box<dyn AsyncRead + Send>>;
pub type BoxWrite = Pin<Box<dyn AsyncWrite + Send>>;
/// Produces the (stdout, stdin) of an app-server. Used by tests to plug in a fake server.
pub type Connector = Arc<dyn Fn() -> Result<(BoxRead, BoxWrite), String> + Send + Sync>;

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------------------------
// Workspace policy for server-initiated requests
// ---------------------------------------------------------------------------------------------

/// Lexical normalisation (`.`/`..`), relative paths resolved against `base`, `~/` expanded.
pub fn normalize(path: &str, base: &Path) -> PathBuf {
    let p = if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest)
    } else {
        PathBuf::from(path)
    };
    let p = if p.is_absolute() { p } else { base.join(p) };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Minimal POSIX-ish word splitter (quotes and backslashes) for approval checks.
pub fn shell_words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = s.chars();
    let (mut sq, mut dq) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !dq => { sq = !sq; has = true; }
            '"' if !sq => { dq = !dq; has = true; }
            '\\' if !sq => { if let Some(n) = chars.next() { cur.push(n); has = true; } }
            c if c.is_whitespace() && !sq && !dq => {
                if has { out.push(std::mem::take(&mut cur)); has = false; }
            }
            ';' | '|' | '&' if !sq && !dq => {
                if has { out.push(std::mem::take(&mut cur)); has = false; }
            }
            c => { cur.push(c); has = true; }
        }
    }
    if has { out.push(cur); }
    out
}

#[derive(Clone, Debug)]
pub struct WorkspacePolicy {
    root: PathBuf,
    root_canon: Option<PathBuf>,
    /// MCP server whose elicitations (tool-call confirmations) are accepted: the Grok Bot
    /// delegate when enabled. Every other server's elicitation is declined.
    delegate_server: Option<String>,
}

impl WorkspacePolicy {
    pub fn new(root: &Path) -> Self {
        let root = normalize(&root.to_string_lossy(), Path::new("/"));
        let root_canon = std::fs::canonicalize(&root).ok();
        Self { root, root_canon, delegate_server: None }
    }

    /// Accept elicitations from this MCP server only (the Grok Bot delegate).
    pub fn with_delegate(mut self, server: Option<&str>) -> Self {
        self.delegate_server = server.map(str::to_owned);
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Is `path` (relative to `base`, default the root) inside the workspace root?
    pub fn inside(&self, path: &str, base: Option<&Path>) -> bool {
        let p = normalize(path, base.unwrap_or(&self.root));
        if p.starts_with(&self.root) {
            return true;
        }
        if let (Some(rc), Ok(pc)) = (&self.root_canon, std::fs::canonicalize(&p)) {
            return pc.starts_with(rc);
        }
        false
    }

    /// Command approval: cwd inside root and no path-looking argument pointing outside it.
    pub fn command_ok(&self, command: &Value, cwd: Option<&str>) -> bool {
        let Some(cwd) = cwd.filter(|c| !c.is_empty()) else { return false };
        if !self.inside(cwd, None) {
            return false;
        }
        let base = normalize(cwd, &self.root);
        let words: Vec<String> = match command {
            Value::String(s) => shell_words(s),
            Value::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
            _ => return false,
        };
        // Skip the program itself (e.g. /bin/ls); check every path-like argument.
        words.iter().skip(1).all(|w| {
            let w = w.split_once('=').map(|(_, v)| v).filter(|v| v.starts_with('/') || v.starts_with('~')).unwrap_or(w);
            let pathish = w.starts_with('/') || w.starts_with("~/") || w.starts_with("..") || w.contains("/../");
            !pathish || self.inside(w, Some(&base))
        })
    }

    fn all_inside<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> bool {
        let mut any = false;
        for p in paths {
            any = true;
            if !self.inside(p, None) {
                return false;
            }
        }
        any
    }

    /// Answer one server-initiated request. Returns (result-or-error, short audit note).
    pub fn handle(&self, method: &str, p: &Value, file_items: &HashMap<String, Vec<String>>) -> (Result<Value, (i64, String)>, String) {
        let yes_no = |ok: bool, yes: &str, no: &str| if ok { yes.to_owned() } else { no.to_owned() };
        match method {
            "item/commandExecution/requestApproval" => {
                let ok = self.command_ok(&p["command"], p["cwd"].as_str());
                let d = yes_no(ok, "accept", "decline");
                (Ok(json!({ "decision": d })), format!("{method}: {d}"))
            }
            "execCommandApproval" => {
                let ok = self.command_ok(&p["command"], p["cwd"].as_str());
                let d = yes_no(ok, "approved", "denied");
                (Ok(json!({ "decision": d })), format!("{method}: {d}"))
            }
            "item/fileChange/requestApproval" => {
                let mut paths: Vec<&str> = file_items.get(p["itemId"].as_str().unwrap_or("")).map(|v| v.iter().map(String::as_str).collect()).unwrap_or_default();
                if let Some(g) = p["grantRoot"].as_str() {
                    paths.push(g);
                }
                let ok = self.all_inside(paths);
                let d = yes_no(ok, "accept", "decline");
                (Ok(json!({ "decision": d })), format!("{method}: {d}"))
            }
            "applyPatchApproval" => {
                let keys: Vec<&str> = p["fileChanges"].as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default();
                let mut keys = keys;
                if let Some(g) = p["grantRoot"].as_str() {
                    keys.push(g);
                }
                let ok = self.all_inside(keys);
                let d = yes_no(ok, "approved", "denied");
                (Ok(json!({ "decision": d })), format!("{method}: {d}"))
            }
            "item/permissions/requestApproval" => {
                let mut paths = Vec::new();
                collect_strings(&p["permissions"]["fileSystem"], &mut paths);
                let ok = paths.iter().all(|s| self.inside(s, None));
                let granted = if ok {
                    let mut g = serde_json::Map::new();
                    for k in ["fileSystem", "network"] {
                        if !p["permissions"][k].is_null() {
                            g.insert(k.into(), p["permissions"][k].clone());
                        }
                    }
                    Value::Object(g)
                } else {
                    json!({})
                };
                (Ok(json!({ "permissions": granted, "scope": "turn" })), format!("{method}: {}", yes_no(ok, "granted", "declined")))
            }
            "currentTime/read" => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                (Ok(json!({ "currentTimeAt": now })), format!("{method}: answered"))
            }
            "item/tool/requestUserInput" => (Ok(json!({ "answers": {} })), format!("{method}: no answers")),
            "mcpServer/elicitation/request" => {
                let server = p["serverName"].as_str().unwrap_or("");
                if self.delegate_server.as_deref().is_some_and(|d| d == server) {
                    (Ok(json!({ "action": "accept", "content": {} })), format!("{method}: accept ({server})"))
                } else {
                    (Ok(json!({ "action": "decline" })), format!("{method}: decline"))
                }
            }
            "item/tool/call" => (Ok(json!({ "contentItems": [], "success": false })), format!("{method}: unsupported")),
            _ => (Err((-32601, format!("{method} is not supported by xiaodai core"))), format!("{method}: error")),
        }
    }
}

/// Collect path-like strings from a permissions profile (keys `path`, `read`, `write`, entries...).
fn collect_strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) if s.starts_with('/') || s.starts_with('~') || s.starts_with('.') => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| collect_strings(x, out)),
        Value::Object(m) => m.values().for_each(|x| collect_strings(x, out)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// JSON-RPC connection
// ---------------------------------------------------------------------------------------------

type PendingMap = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>;
type SubMap = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>;

pub struct RpcConn {
    out: mpsc::UnboundedSender<String>,
    next_id: AtomicI64,
    pending: PendingMap,
    subs: SubMap,
    alive: Arc<AtomicBool>,
    closed: CancellationToken,
    /// Audit trail of server-request decisions (method + decision only).
    pub audit: Arc<Mutex<Vec<String>>>,
    _child: Mutex<Option<tokio::process::Child>>,
}

impl RpcConn {
    /// Wrap an app-server's stdout (`reader`) and stdin (`writer`).
    pub fn from_io(reader: BoxRead, writer: BoxWrite, policy: WorkspacePolicy) -> Self {
        let (out, out_rx) = mpsc::unbounded_channel::<String>();
        let pending: PendingMap = Default::default();
        let subs: SubMap = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let closed = CancellationToken::new();
        let audit: Arc<Mutex<Vec<String>>> = Default::default();
        tokio::spawn(writer_loop(writer, out_rx));
        tokio::spawn(reader_loop(reader, pending.clone(), subs.clone(), out.clone(), policy, alive.clone(), closed.clone(), audit.clone()));
        Self { out, next_id: AtomicI64::new(1), pending, subs, alive, closed, audit, _child: Mutex::new(None) }
    }

    pub fn spawn(bin: &Path, args: &[String], policy: WorkspacePolicy) -> Result<Self, String> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("app-server").arg("--stdio").args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let conn = Self::from_io(Box::pin(stdout), Box::pin(stdin), policy);
        *conn._child.lock().unwrap() = Some(child);
        Ok(conn)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Fires when the app-server's stdout closes (child exited / crashed).
    pub fn closed(&self) -> CancellationToken {
        self.closed.clone()
    }

    pub async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        if !self.is_alive() {
            return Err("codex app-server is not running".into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let line = json!({ "id": id, "method": method, "params": params }).to_string();
        if self.out.send(line).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err("codex app-server is not running".into());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(Ok(v))) => Ok(v),
            Ok(Ok(Err(e))) => Err(format!("{method}: {e}")),
            Ok(Err(_)) => Err(format!("{method}: codex app-server exited")),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("{method}: timed out after {} ms", timeout.as_millis()))
            }
        }
    }

    pub fn notify(&self, method: &str, params: Value) {
        let _ = self.out.send(json!({ "method": method, "params": params }).to_string());
    }

    /// Route notifications carrying `params.threadId == thread_id` to the returned receiver.
    pub fn subscribe(&self, thread_id: &str) -> mpsc::UnboundedReceiver<Value> {
        let (tx, rx) = mpsc::unbounded_channel();
        if self.is_alive() {
            self.subs.lock().unwrap().insert(thread_id.to_owned(), tx);
        }
        rx
    }

    pub fn unsubscribe(&self, thread_id: &str) {
        self.subs.lock().unwrap().remove(thread_id);
    }
}

async fn writer_loop(mut w: BoxWrite, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(mut line) = rx.recv().await {
        line.push('\n');
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn reader_loop(
    r: BoxRead,
    pending: PendingMap,
    subs: SubMap,
    out: mpsc::UnboundedSender<String>,
    policy: WorkspacePolicy,
    alive: Arc<AtomicBool>,
    closed: CancellationToken,
    audit: Arc<Mutex<Vec<String>>>,
) {
    let mut lines = BufReader::new(r).lines();
    // fileChange item id -> paths, learned from item/started so approvals can be checked.
    let mut file_items: HashMap<String, Vec<String>> = HashMap::new();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let method = v.get("method").and_then(Value::as_str);
        let id = v.get("id").filter(|i| !i.is_null()).cloned();
        match (method, id) {
            (Some(m), Some(id)) => {
                let (res, note) = policy.handle(m, &v["params"], &file_items);
                audit.lock().unwrap().push(note);
                let msg = match res {
                    Ok(r) => json!({ "id": id, "result": r }),
                    Err((code, message)) => json!({ "id": id, "error": { "code": code, "message": message } }),
                };
                let _ = out.send(msg.to_string());
            }
            (Some(m), None) => {
                if m == "item/started" || m == "item/completed" {
                    let item = &v["params"]["item"];
                    if item["type"] == "fileChange" {
                        let paths = item["changes"].as_array().map(|a| a.iter().filter_map(|c| c["path"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
                        if let Some(iid) = item["id"].as_str() {
                            file_items.insert(iid.to_owned(), paths);
                        }
                    }
                }
                if let Some(tid) = v["params"]["threadId"].as_str() {
                    let tx = subs.lock().unwrap().get(tid).cloned();
                    if let Some(tx) = tx {
                        let _ = tx.send(v);
                    }
                }
            }
            (None, Some(id)) => {
                if let Some(i) = id.as_i64() {
                    let tx = pending.lock().unwrap().remove(&i);
                    if let Some(tx) = tx {
                        let r = match v.get("error").filter(|e| !e.is_null()) {
                            Some(e) => Err(e["message"].as_str().map(str::to_owned).unwrap_or_else(|| e.to_string())),
                            None => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(r);
                    }
                }
            }
            _ => {}
        }
    }
    alive.store(false, Ordering::SeqCst);
    pending.lock().unwrap().clear(); // drops senders: waiters see "exited"
    subs.lock().unwrap().clear(); // closes turn streams
    closed.cancel();
}

// ---------------------------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize)]
pub struct ModelInfo {
    pub id: String,
    pub display_name: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}

/// Cold-start report (no secrets: account type/plan only).
#[derive(Clone, Debug, Default, Serialize)]
pub struct CodexWarmup {
    pub binary: String,
    pub user_agent: String,
    pub spawn_to_initialized_ms: f64,
    pub account: String,
    pub account_read_ms: f64,
    pub model_list_ms: f64,
    pub models: Vec<ModelInfo>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub efforts: Vec<String>,
    /// Model catalog source: "static <path>" (no remote refresh) or "remote (...)".
    pub catalog: String,
    pub thread_start_ms: f64,
    pub prime_turn_ms: Option<f64>,
    pub total_ms: f64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct InterruptStats {
    pub turn_id: String,
    /// cancel observed -> `turn/interrupt` written.
    pub cancel_to_interrupt_sent_ms: f64,
    /// cancel observed -> `turn/completed` received.
    pub cancel_to_completed_ms: Option<f64>,
    pub final_status: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct SteerStats {
    pub turn_id: String,
    /// `turn/steer` written -> its response received.
    pub ack_ms: f64,
    /// `turn/steer` written -> first visible token of the steered answer.
    pub first_token_ms: Option<f64>,
    pub ok: bool,
    pub error: Option<String>,
}

/// Where the running turn currently delivers its output. A steer hands it to the follow-up caller.
struct Sink {
    idx: usize,
    tx: mpsc::UnboundedSender<(usize, AttemptMsg)>,
    cancel: CancellationToken,
}

impl Sink {
    fn send(&self, m: AttemptMsg) {
        let _ = self.tx.send((self.idx, m));
    }
}

/// The one in-flight turn on the conversation thread.
#[derive(Clone)]
struct Active {
    id: i64,
    turn_id: Option<String>,
    sink: Arc<Mutex<Sink>>,
    /// Wakes `run_turn` when the sink or claim changed.
    changed: Arc<tokio::sync::Notify>,
    /// A follow-up `respond` claimed this turn for a steer; while set, the old caller's cancel (e.g.
    /// dropping its `Turn`) no longer interrupts, only this token does.
    claim: Arc<Mutex<Option<CancellationToken>>>,
    /// Engine-requested interrupt (barge-in policy `interrupt`).
    interrupt: CancellationToken,
    /// After an accepted steer: drop deltas of the pre-steer answer until the steer's userMessage starts.
    steer_pending: Arc<AtomicBool>,
    steer_sent_at: Arc<Mutex<Option<Instant>>>,
    /// The turn completed while a steer was still pending (its text was never answered).
    steer_unanswered: Arc<AtomicBool>,
    done: CancellationToken,
}

impl Active {
    fn new(id: i64, sink: Sink) -> Self {
        Self {
            id,
            turn_id: None,
            sink: Arc::new(Mutex::new(sink)),
            changed: Default::default(),
            claim: Default::default(),
            interrupt: CancellationToken::new(),
            steer_pending: Default::default(),
            steer_sent_at: Default::default(),
            steer_unanswered: Default::default(),
            done: CancellationToken::new(),
        }
    }

    /// The token whose cancellation interrupts the turn right now.
    fn effective_cancel(&self) -> CancellationToken {
        if let Some(c) = self.claim.lock().unwrap().clone() {
            return c;
        }
        self.sink.lock().unwrap().cancel.clone()
    }

    /// Still producing output for somebody (not finished, not being interrupted).
    fn live(&self) -> bool {
        !self.done.is_cancelled() && !self.interrupt.is_cancelled() && !self.effective_cancel().is_cancelled()
    }
}

#[derive(Default)]
struct State {
    conn: Option<Arc<RpcConn>>,
    model: Option<String>,
    effort: Option<String>,
    /// The single conversation thread (None until init / after the child died).
    thread: Option<String>,
    thread_turns: u32,
    threads_started: u32,
    last_failure: Option<(Instant, String)>,
    warmup: Option<CodexWarmup>,
}

struct Inner {
    cfg: CodexConfig,
    policy: WorkspacePolicy,
    connector: Option<Connector>,
    state: tokio::sync::Mutex<State>,
    /// Serialises "steer or start a turn" so two utterances never race for the thread.
    gate: tokio::sync::Mutex<()>,
    active: Mutex<Option<Active>>,
    next_active: AtomicI64,
    interrupt: Mutex<Option<InterruptStats>>,
    steer: Mutex<Option<SteerStats>>,
    /// Health-probe thread: (connection identity, thread id). Separate from the conversation.
    probe: tokio::sync::Mutex<Option<(usize, String)>>,
}

impl Inner {
    fn clear_active(&self, id: i64) {
        let mut g = self.active.lock().unwrap();
        if g.as_ref().is_some_and(|a| a.id == id) {
            *g = None;
        }
    }
}

#[derive(Clone)]
pub struct CodexEngine {
    inner: Arc<Inner>,
}

const EFFORT_RANK: [&str; 8] = ["none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra"];

fn norm_model(s: &str) -> String {
    s.trim().to_lowercase().replace([' ', '_'], "-")
}

/// The health-probe utterance (on the probe thread only).
pub const PROBE_TEXT: &str = "健康检查：只回复一个字“好”。";

/// Default binary: `XIAODAI_CODEX_BIN`, else /usr/local/bin/codex, else the ChatGPT app's bundled
/// codex, else the node-installed codex in ~/working/codex-cli, else `codex` on PATH.
///
/// /usr/local/bin/codex needs 0.169+ for gpt-6-luna: 0.153.4 was rejected by the ChatGPT backend
/// ("not supported when using Codex with a ChatGPT account"); 0.170 (upgraded 2026-11-18) works.
pub fn default_codex_binary() -> PathBuf {
    if let Some(v) = std::env::var_os("XIAODAI_CODEX_BIN").filter(|v| !v.is_empty()) {
        return PathBuf::from(v);
    }
    let installed = Path::new("/usr/local/bin/codex");
    if installed.exists() {
        return installed.to_path_buf();
    }
    let bundled = Path::new("/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex");
    if bundled.exists() {
        return bundled.to_path_buf();
    }
    if let Some(home) = std::env::var_os("HOME") {
        let node = PathBuf::from(home).join("working/codex-cli/node_modules/.bin/codex");
        if node.exists() {
            return node;
        }
    }
    PathBuf::from("codex")
}

/// `-c` overrides for the core's own codex child (before the model catalog): the lean set, then
/// the Grok Bot MCP server when delegation is enabled. ~/.codex/config.toml is never modified.
pub fn child_overrides(cfg: &CodexConfig) -> Vec<String> {
    let mut args = if cfg.lean { lean_overrides() } else { Vec::new() };
    args.extend(cfg.grok_bot.codex_overrides());
    args
}

/// `-c` overrides that switch off user MCP servers, plugins and apps for this child only.
/// Reads only `[mcp_servers.<name>]` header names from config.toml (no values).
pub fn lean_overrides() -> Vec<String> {
    let mut args = vec!["-c".into(), "features.plugins=false".into(), "-c".into(), "features.apps=false".into()];
    let home = std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")));
    let Some(home) = home else { return args };
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else { return args };
    let mut names: Vec<String> = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("[mcp_servers.") else { continue };
        let name = if let Some(q) = rest.strip_prefix('"') {
            q.split('"').next().unwrap_or("").to_owned()
        } else {
            rest.split(['.', ']']).next().unwrap_or("").to_owned()
        };
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    for n in names {
        let key = if n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') { n } else { format!("\"{}\"", n.replace('"', "")) };
        args.push("-c".into());
        args.push(format!("mcp_servers.{key}.enabled=false"));
    }
    args
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Write `<bin> debug models --bundled` to a temp file for `-c model_catalog_json=...`.
/// A static catalog stops the app-server from re-fetching the remote model list, which on this
/// network times out after 5 s and was being retried on model/list, thread/start and *every turn*.
async fn bundled_catalog(bin: &Path) -> Result<PathBuf, String> {
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(bin)
            .args(["debug", "models", "--bundled"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "debug models --bundled timed out".to_string())?
    .map_err(|e| format!("debug models --bundled: {e}"))?;
    if !out.status.success() {
        return Err(format!("debug models --bundled exited with {}", out.status));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("bundled catalog is not JSON: {e}"))?;
    if v["models"].as_array().is_none_or(|a| a.is_empty()) {
        return Err("bundled catalog has no models".into());
    }
    let dir = std::env::temp_dir().join("xiaodai-core");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let tag: String = bin.display().to_string().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let tag = &tag[tag.len().saturating_sub(60)..];
    let path = dir.join(format!("codex-models-{tag}.json"));
    let tmp = dir.join(format!(".codex-models-{tag}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &out.stdout).map_err(|e| format!("write catalog: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("write catalog: {e}"))?;
    Ok(path)
}

impl CodexEngine {
    pub fn new(cfg: CodexConfig, workspace_root: &Path) -> Self {
        Self::build(cfg, workspace_root, None)
    }

    /// Same engine, but `connector` supplies the app-server streams (tests / custom transports).
    pub fn with_connector(cfg: CodexConfig, workspace_root: &Path, connector: Connector) -> Self {
        Self::build(cfg, workspace_root, Some(connector))
    }

    fn build(cfg: CodexConfig, root: &Path, connector: Option<Connector>) -> Self {
        Self {
            inner: Arc::new(Inner {
                policy: WorkspacePolicy::new(root).with_delegate(cfg.grok_bot.enabled.then_some(crate::config::GROK_BOT_MCP_NAME)),
                cfg,
                connector,
                state: tokio::sync::Mutex::new(State::default()),
                gate: tokio::sync::Mutex::new(()),
                active: Mutex::new(None),
                next_active: AtomicI64::new(1),
                interrupt: Mutex::new(None),
                steer: Mutex::new(None),
                probe: tokio::sync::Mutex::new(None),
            }),
        }
    }

    pub fn config(&self) -> &CodexConfig {
        &self.inner.cfg
    }

    pub fn workspace_root(&self) -> &Path {
        self.inner.policy.root()
    }

    /// "codex/<model>" for timings.
    pub fn label(&self) -> String {
        let m = self.inner.state.try_lock().ok().and_then(|s| s.model.clone()).unwrap_or_else(|| self.inner.cfg.model.clone());
        format!("codex/{m}")
    }

    /// Server-request decisions made so far on the current connection.
    pub fn audit(&self) -> Vec<String> {
        self.inner.state.try_lock().ok().and_then(|s| s.conn.as_ref().map(|c| c.audit.lock().unwrap().clone())).unwrap_or_default()
    }

    /// The conversation thread id (one thread for the whole conversation).
    pub fn thread_id(&self) -> Option<String> {
        self.inner.state.try_lock().ok().and_then(|s| s.thread.clone())
    }

    /// How many threads were started on the current/previous connections (1 = never rotated).
    pub fn threads_started(&self) -> u32 {
        self.inner.state.try_lock().map(|s| s.threads_started).unwrap_or(0)
    }

    /// True while a Codex turn is running on the thread.
    pub fn turn_active(&self) -> bool {
        self.inner.active.lock().unwrap().as_ref().is_some_and(|a| !a.done.is_cancelled())
    }

    pub fn last_interrupt(&self) -> Option<InterruptStats> {
        self.inner.interrupt.lock().unwrap().clone()
    }

    pub fn last_steer(&self) -> Option<SteerStats> {
        self.inner.steer.lock().unwrap().clone()
    }

    /// Wait (poll) until the last interrupt saw its `turn/completed`, or timeout.
    pub async fn wait_interrupt_completed(&self, timeout: Duration) -> Option<InterruptStats> {
        let t = Instant::now();
        loop {
            let s = self.last_interrupt();
            if s.as_ref().is_some_and(|s| s.cancel_to_completed_ms.is_some()) || t.elapsed() > timeout {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Wait until no Codex turn is running (or timeout). Returns true if idle.
    pub async fn wait_idle(&self, timeout: Duration) -> bool {
        let t = Instant::now();
        while self.turn_active() {
            if t.elapsed() > timeout {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        true
    }

    /// Called synchronously by `Core::respond` before anything else: if a Codex turn is still
    /// producing output and the barge-in policy is `steer`, reserve it for this follow-up so that
    /// dropping/cancelling the previous `Turn` does not interrupt it before the steer lands.
    /// `token` is the follow-up's own cancel token. Returns true if a turn was claimed.
    pub fn claim_for_followup(&self, token: &CancellationToken) -> bool {
        if self.inner.cfg.barge_in != BargeIn::Steer {
            return false;
        }
        let g = self.inner.active.lock().unwrap();
        let Some(a) = g.as_ref() else { return false };
        if !a.live() {
            return false;
        }
        *a.claim.lock().unwrap() = Some(token.clone());
        a.changed.notify_one();
        true
    }

    /// Health probe: one tiny turn on a separate throwaway thread (created + primed on first use,
    /// so the conversation thread never sees probes). Respawns the child if it died (respecting
    /// `respawn_backoff_ms`). Returns ms from `turn/start` to the first visible text.
    pub async fn probe(&self, timeout: Duration) -> Result<u64, String> {
        let conn = self.ensure_ready().await?;
        let (model, effort) = {
            let st = self.inner.state.lock().await;
            (st.model.clone().unwrap_or_default(), st.effort.clone().unwrap_or_default())
        };
        let ident = Arc::as_ptr(&conn) as usize;
        let tid = {
            let mut p = self.inner.probe.lock().await;
            match p.as_ref() {
                Some((c, t)) if *c == ident => t.clone(),
                _ => {
                    let (t, _, _) = self.new_thread(&conn, &model, &effort, true).await?;
                    *p = Some((ident, t.clone()));
                    t
                }
            }
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let act = Active::new(0, Sink { idx: 0, tx, cancel: cancel.clone() });
        let params = self.turn_params(&tid, PROBE_TEXT, &model, &effort);
        let t = Instant::now();
        let h = tokio::spawn(run_turn(conn, tid, params, act, timeout, None, None));
        let r = loop {
            match tokio::time::timeout_at(tokio::time::Instant::from_std(t + timeout), rx.recv()).await {
                Err(_) => break Err(format!("probe timed out after {} ms", timeout.as_millis())),
                Ok(None) => break Err("probe: turn ended without output".into()),
                Ok(Some((_, AttemptMsg::Token(s)))) if crate::splitter::has_visible(&s) => break Ok(t.elapsed().as_millis() as u64),
                Ok(Some((_, AttemptMsg::Failed(e)))) => break Err(e),
                Ok(Some((_, AttemptMsg::Finished))) => break Err("probe: empty answer".into()),
                Ok(Some(_)) => {}
            }
        };
        // Let the one-word answer finish on its own (an interrupt would cost codex a reconnect);
        // only stop it if it runs on.
        if tokio::time::timeout(Duration::from_secs(10), &mut Box::pin(h)).await.is_err() {
            cancel.cancel();
        }
        if r.is_err() {
            // A broken probe thread must not poison the next probe.
            *self.inner.probe.lock().await = None;
        }
        r
    }

    /// Spawn + initialize + account check + model/list + the conversation thread (primed). Idempotent.
    pub async fn warmup(&self) -> CodexWarmup {
        let r = self.ensure_ready().await;
        let st = self.inner.state.lock().await;
        let mut w = st.warmup.clone().unwrap_or_default();
        if let Err(e) = r {
            w.error = Some(e);
        }
        w
    }

    async fn ensure_ready(&self) -> Result<Arc<RpcConn>, String> {
        let mut st = self.inner.state.lock().await;
        if let Some(c) = &st.conn {
            if c.is_alive() {
                return Ok(c.clone());
            }
            st.conn = None;
            st.thread = None;
            st.last_failure = Some((Instant::now(), "codex app-server exited".into()));
        }
        if let Some((t, e)) = &st.last_failure {
            if t.elapsed() < Duration::from_millis(self.inner.cfg.respawn_backoff_ms) {
                return Err(format!("codex unhealthy: {e}"));
            }
        }
        let mut w = CodexWarmup::default();
        let r = self.init(&mut st, &mut w).await;
        match r {
            Ok(c) => {
                st.last_failure = None;
                st.warmup = Some(w);
                Ok(c)
            }
            Err(e) => {
                w.error = Some(e.clone());
                st.warmup = Some(w);
                st.conn = None;
                st.thread = None;
                st.last_failure = Some((Instant::now(), e.clone()));
                Err(e)
            }
        }
    }

    async fn connect(&self, w: &mut CodexWarmup) -> Result<RpcConn, String> {
        if let Some(f) = &self.inner.connector {
            w.binary = "<connector>".into();
            w.catalog = "n/a (connector)".into();
            let (r, wr) = f()?;
            return Ok(RpcConn::from_io(r, wr, self.inner.policy.clone()));
        }
        let cfg = &self.inner.cfg;
        let bin = cfg.binary.clone().unwrap_or_else(default_codex_binary);
        w.binary = bin.display().to_string();
        let mut args = child_overrides(cfg);
        let catalog = match &cfg.model_catalog_json {
            Some(p) => Ok(p.clone()),
            None if cfg.static_model_catalog => bundled_catalog(&bin).await,
            None => Err("disabled".into()),
        };
        match catalog {
            Ok(p) => {
                w.catalog = format!("static {}", p.display());
                args.push("-c".into());
                args.push(format!("model_catalog_json={}", toml_string(&p.display().to_string())));
            }
            Err(e) => w.catalog = format!("remote (static catalog: {e})"),
        }
        RpcConn::spawn(&bin, &args, self.inner.policy.clone())
    }

    async fn init(&self, st: &mut State, w: &mut CodexWarmup) -> Result<Arc<RpcConn>, String> {
        let cfg = &self.inner.cfg;
        let rt = Duration::from_millis(cfg.request_timeout_ms);
        let t0 = Instant::now();
        let conn = Arc::new(self.connect(w).await?);
        let init = conn
            .request(
                "initialize",
                json!({ "clientInfo": { "name": "xiaodai_core", "title": "Xiaodai Core", "version": env!("CARGO_PKG_VERSION") },
                        "capabilities": { "experimentalApi": true } }),
                rt,
            )
            .await?;
        conn.notify("initialized", json!({}));
        w.spawn_to_initialized_ms = ms_since(t0);
        w.user_agent = init["userAgent"].as_str().unwrap_or("").to_owned();

        let t = Instant::now();
        let acc = conn.request("account/read", json!({}), rt).await?;
        w.account_read_ms = ms_since(t);
        if acc["account"].is_null() {
            if acc["requiresOpenaiAuth"].as_bool().unwrap_or(true) {
                return Err("codex is not logged in (run `codex login` once)".into());
            }
            w.account = "none required".into();
        } else {
            w.account = format!("{} {}", acc["account"]["type"].as_str().unwrap_or("?"), acc["account"]["planType"].as_str().unwrap_or("")).trim().to_owned();
        }

        let t = Instant::now();
        let mut models = Vec::new();
        let mut cursor = Value::Null;
        for _ in 0..10 {
            let r = conn.request("model/list", json!({ "includeHidden": true, "cursor": cursor }), rt).await?;
            for m in r["data"].as_array().cloned().unwrap_or_default() {
                models.push(ModelInfo {
                    id: m["id"].as_str().unwrap_or("").to_owned(),
                    display_name: m["displayName"].as_str().unwrap_or("").to_owned(),
                    efforts: m["supportedReasoningEfforts"].as_array().map(|a| a.iter().filter_map(|e| e["reasoningEffort"].as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                    default_effort: m["defaultReasoningEffort"].as_str().unwrap_or("").to_owned(),
                });
            }
            cursor = r["nextCursor"].clone();
            if cursor.is_null() {
                break;
            }
        }
        w.model_list_ms = ms_since(t);
        w.models = models.clone();
        let want = norm_model(&cfg.model);
        let Some(m) = models.iter().find(|m| norm_model(&m.id) == want || norm_model(&m.display_name) == want) else {
            let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
            return Err(format!("model '{}' not found in model/list; available: {}", cfg.model, ids.join(", ")));
        };
        let effort = match &cfg.effort {
            Some(e) if m.efforts.iter().any(|x| x == e) => e.clone(),
            Some(e) => return Err(format!("effort '{e}' not supported by {}; supported: {}", m.id, m.efforts.join(", "))),
            None => m.efforts.iter().min_by_key(|e| EFFORT_RANK.iter().position(|r| r == e).unwrap_or(99)).cloned().unwrap_or_else(|| m.default_effort.clone()),
        };
        w.model = Some(m.id.clone());
        w.effort = Some(effort.clone());
        w.efforts = m.efforts.clone();
        st.model = Some(m.id.clone());
        st.effort = Some(effort.clone());

        let (tid, ts, prime) = self.new_thread(&conn, &m.id, &effort, cfg.prime_threads).await?;
        w.thread_start_ms = ts;
        w.prime_turn_ms = prime;
        st.thread = Some(tid);
        st.thread_turns = 0;
        st.threads_started += 1;
        st.conn = Some(conn.clone());
        w.total_ms = ms_since(t0);
        Ok(conn)
    }

    fn turn_params(&self, thread: &str, text: &str, model: &str, effort: &str) -> Value {
        let cfg = &self.inner.cfg;
        let root = self.inner.policy.root().display().to_string();
        let mut p = json!({
            "threadId": thread,
            "input": [{ "type": "text", "text": text }],
            "model": model,
            "effort": effort,
            "cwd": root,
            "approvalPolicy": "never",
            "sandboxPolicy": { "type": "workspaceWrite", "writableRoots": [root], "networkAccess": cfg.network_access },
        });
        if let Some(t) = &cfg.service_tier {
            p["serviceTier"] = json!(t);
        }
        p
    }

    /// thread/start (+ optional priming turn). Returns (thread id, thread/start ms, prime ms).
    async fn new_thread(&self, conn: &Arc<RpcConn>, model: &str, effort: &str, prime: bool) -> Result<(String, f64, Option<f64>), String> {
        let cfg = &self.inner.cfg;
        let rt = Duration::from_millis(cfg.request_timeout_ms);
        let root = self.inner.policy.root().display().to_string();
        let mut p = json!({
            "cwd": root,
            "sandbox": "workspace-write",
            "approvalPolicy": "never",
            "ephemeral": true,
            "model": model,
            "developerInstructions": cfg.effective_instructions(),
        });
        if let Some(t) = &cfg.service_tier {
            p["serviceTier"] = json!(t);
        }
        let t = Instant::now();
        let r = conn.request("thread/start", p, rt).await?;
        let tid = r["thread"]["id"].as_str().ok_or("thread/start: no thread id")?.to_owned();
        let ts = ms_since(t);
        let mut prime_ms = None;
        if prime {
            let t = Instant::now();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let act = Active::new(0, Sink { idx: 0, tx, cancel: CancellationToken::new() });
            let params = self.turn_params(&tid, "你好", model, effort);
            run_turn(conn.clone(), tid.clone(), params, act, Duration::from_millis(cfg.turn_timeout_ms), None, None).await;
            while let Ok((_, m)) = rx.try_recv() {
                if let AttemptMsg::Failed(e) = m {
                    return Err(format!("priming turn: {e}"));
                }
            }
            prime_ms = Some(ms_since(t));
        }
        Ok((tid, ts, prime_ms))
    }

    /// Forget the conversation thread, e.g. because the caller deleted context it had sent or a
    /// new conversation day began. The next turn starts on a new thread. With `prime`, and a live
    /// app-server, a primed thread is prepared now in the background (same cost as warmup's).
    /// The probe thread is unaffected.
    pub async fn reset_thread(&self, prime: bool) {
        let conn = {
            let mut st = self.inner.state.lock().await;
            st.thread = None;
            st.thread_turns = 0;
            st.conn.clone().filter(|c| c.is_alive())
        };
        let (true, Some(conn)) = (prime, conn) else { return };
        let me = self.clone();
        tokio::spawn(async move {
            let mut st = me.inner.state.lock().await;
            if st.thread.is_some() {
                return;
            }
            let (m, e) = (st.model.clone().unwrap_or_default(), st.effort.clone().unwrap_or_default());
            if let Ok((id, _, _)) = me.new_thread(&conn, &m, &e, true).await {
                st.thread = Some(id);
                st.thread_turns = 0;
                st.threads_started += 1;
            }
        });
    }

    /// The conversation thread for the next turn (rotates only if `max_turns_per_thread` > 0).
    async fn thread_for_turn(&self, conn: &Arc<RpcConn>) -> Result<(String, String, String), String> {
        let mut st = self.inner.state.lock().await;
        let (m, e) = (st.model.clone().unwrap_or_default(), st.effort.clone().unwrap_or_default());
        let max = self.inner.cfg.max_turns_per_thread;
        if st.thread.is_none() || (max > 0 && st.thread_turns >= max) {
            let (id, _, _) = self.new_thread(conn, &m, &e, false).await?;
            st.thread = Some(id);
            st.thread_turns = 0;
            st.threads_started += 1;
        }
        st.thread_turns += 1;
        Ok((st.thread.clone().unwrap_or_default(), m, e))
    }

    /// `turn/steer` into the active turn; on success the turn's output is handed to this caller.
    #[allow(clippy::too_many_arguments)]
    async fn steer(&self, conn: &Arc<RpcConn>, a: &Active, tid: &str, turn_id: &str, text: &str, cancel: &CancellationToken, idx: usize, tx: &mpsc::UnboundedSender<(usize, AttemptMsg)>) -> bool {
        let p = json!({ "threadId": tid, "expectedTurnId": turn_id, "input": [{ "type": "text", "text": text }] });
        let t = Instant::now();
        let r = conn.request("turn/steer", p, Duration::from_millis(self.inner.cfg.request_timeout_ms)).await;
        let ack_ms = ms_since(t);
        let mut stats = SteerStats { turn_id: turn_id.to_owned(), ack_ms, ..Default::default() };
        let ok = match r {
            Ok(_) if !a.done.is_cancelled() && !a.interrupt.is_cancelled() => {
                *a.steer_sent_at.lock().unwrap() = Some(t);
                a.steer_pending.store(true, Ordering::SeqCst);
                let old = std::mem::replace(&mut *a.sink.lock().unwrap(), Sink { idx, tx: tx.clone(), cancel: cancel.clone() });
                *a.claim.lock().unwrap() = None;
                a.changed.notify_one();
                old.send(AttemptMsg::Superseded);
                stats.ok = true;
                true
            }
            Ok(_) => {
                stats.error = Some("turn finished before the steer was applied".into());
                false
            }
            Err(e) => {
                stats.error = Some(e);
                false
            }
        };
        *self.inner.steer.lock().unwrap() = Some(stats);
        ok
    }

    /// One user utterance. Steers into a running turn (policy `steer`), or interrupts it (policy
    /// `interrupt`) and starts a new turn on the same thread. Sends Token/Reasoning, then
    /// Finished/Failed (unless cancelled) or Superseded (a later utterance took over).
    async fn turn(&self, text: String, cancel: CancellationToken, idx: usize, tx: mpsc::UnboundedSender<(usize, AttemptMsg)>) {
        let mut gate = self.inner.gate.lock().await;
        let conn = match self.ensure_ready().await {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send((idx, AttemptMsg::Failed(e)));
                return;
            }
        };
        let prev = self.inner.active.lock().unwrap().clone();
        if let Some(a) = prev {
            if a.live() {
                let tid = self.thread_id().unwrap_or_default();
                match (self.inner.cfg.barge_in, a.turn_id.clone()) {
                    (BargeIn::Steer, Some(turn_id)) if !cancel.is_cancelled() => {
                        if self.steer(&conn, &a, &tid, &turn_id, &text, &cancel, idx, &tx).await {
                            drop(gate);
                            a.done.cancelled().await;
                            if !a.steer_unanswered.load(Ordering::SeqCst) {
                                return;
                            }
                            // The turn ended before it consumed the steer: answer it in a new turn.
                            gate = self.inner.gate.lock().await;
                        }
                    }
                    _ => {
                        // Interrupt policy: the previous caller is told it was superseded, then the turn stops.
                        a.sink.lock().unwrap().send(AttemptMsg::Superseded);
                        a.interrupt.cancel();
                    }
                }
            }
            // Never start a turn while the previous one is still winding down (single thread).
            let _ = tokio::time::timeout(Duration::from_secs(15), a.done.cancelled()).await;
        }
        if cancel.is_cancelled() {
            return;
        }
        let (tid, model, effort) = match self.thread_for_turn(&conn).await {
            Ok(x) => x,
            Err(e) => {
                let _ = tx.send((idx, AttemptMsg::Failed(e)));
                return;
            }
        };
        let id = self.inner.next_active.fetch_add(1, Ordering::SeqCst);
        let act = Active::new(id, Sink { idx, tx, cancel });
        *self.inner.active.lock().unwrap() = Some(act.clone());
        let params = self.turn_params(&tid, &text, &model, &effort);
        let (started_tx, started_rx) = oneshot::channel::<Result<(), String>>();
        let timeout = Duration::from_millis(self.inner.cfg.turn_timeout_ms);
        let h = tokio::spawn(run_turn(conn, tid, params, act, timeout, Some(self.inner.clone()), Some(started_tx)));
        // Hold the gate until the turn id is known, so a follow-up can always steer it.
        if let Ok(Err(e)) = started_rx.await {
            if e.contains("thread") {
                // e.g. thread not found / not loaded: start a fresh thread next time.
                self.inner.state.lock().await.thread = None;
            }
        }
        drop(gate);
        let _ = h.await;
    }
}

async fn run_turn(
    conn: Arc<RpcConn>,
    tid: String,
    params: Value,
    mut act: Active,
    timeout: Duration,
    inner: Option<Arc<Inner>>,
    started: Option<oneshot::Sender<Result<(), String>>>,
) {
    let rt = Duration::from_secs(30);
    let mut rx = conn.subscribe(&tid);
    let mut buffered: Vec<Value> = Vec::new();
    let finish = |act: &Active| {
        conn.unsubscribe(&tid);
        if let Some(i) = &inner {
            i.clear_active(act.id);
        }
        act.done.cancel();
    };
    let start = conn.request("turn/start", params, rt);
    tokio::pin!(start);
    let turn_id = loop {
        tokio::select! {
            r = &mut start => match r.and_then(|v| v["turn"]["id"].as_str().map(str::to_owned).ok_or_else(|| "turn/start: no turn id".to_string())) {
                Ok(id) => break id,
                Err(e) => {
                    act.sink.lock().unwrap().send(AttemptMsg::Failed(e.clone()));
                    finish(&act);
                    if let Some(s) = started { let _ = s.send(Err(e)); }
                    return;
                }
            },
            Some(n) = rx.recv() => buffered.push(n),
        }
    };
    act.turn_id = Some(turn_id.clone());
    if let Some(i) = &inner {
        if let Some(a) = i.active.lock().unwrap().as_mut() {
            if a.id == act.id {
                a.turn_id = Some(turn_id.clone());
            }
        }
    }
    if let Some(s) = started {
        let _ = s.send(Ok(()));
    }

    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    let mut cancelled_at: Option<Instant> = None;
    let mut last_error: Option<String> = None;
    let mut stats = InterruptStats { turn_id: turn_id.clone(), ..Default::default() };
    let mut pending: std::collections::VecDeque<Value> = buffered.into();

    let send_interrupt = |conn: &Arc<RpcConn>| {
        let c = conn.clone();
        let p = json!({ "threadId": tid, "turnId": turn_id });
        tokio::spawn(async move {
            let _ = c.request("turn/interrupt", p, Duration::from_secs(10)).await;
        });
    };

    loop {
        let cur_cancel = act.effective_cancel();
        let msg = if let Some(m) = pending.pop_front() {
            Some(m)
        } else {
            tokio::select! {
                biased;
                _ = cur_cancel.cancelled(), if cancelled_at.is_none() => None,
                _ = act.interrupt.cancelled(), if cancelled_at.is_none() => None,
                _ = act.changed.notified() => continue,
                n = rx.recv() => match n {
                    Some(n) => Some(n),
                    None => {
                        if cancelled_at.is_none() { act.sink.lock().unwrap().send(AttemptMsg::Failed("codex app-server exited mid-turn".into())); }
                        break;
                    }
                },
                _ = &mut deadline => {
                    if cancelled_at.is_none() {
                        send_interrupt(&conn);
                        act.sink.lock().unwrap().send(AttemptMsg::Failed(format!("codex turn timed out after {} ms", timeout.as_millis())));
                    }
                    break;
                }
            }
        };
        let Some(v) = msg else {
            // A stale token (the turn was claimed / handed over meanwhile) is not a cancel.
            if !act.interrupt.is_cancelled() && !act.effective_cancel().is_cancelled() {
                continue;
            }
            // Cancelled (caller, claimer, or engine barge-in): interrupt upstream.
            let t = Instant::now();
            cancelled_at = Some(t);
            send_interrupt(&conn);
            stats.cancel_to_interrupt_sent_ms = ms_since(t);
            if let Some(s) = &inner { *s.interrupt.lock().unwrap() = Some(stats.clone()); }
            // Give the server a bounded time to confirm.
            deadline.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
            continue;
        };
        if let Some(t) = v["params"]["turnId"].as_str() {
            if t != turn_id {
                continue;
            }
        }
        let live = cancelled_at.is_none() && !act.effective_cancel().is_cancelled();
        match v["method"].as_str().unwrap_or("") {
            "item/started" => {
                if v["params"]["item"]["type"] == "userMessage" && act.steer_pending.load(Ordering::SeqCst) {
                    // The steer input is now part of the turn; what follows answers it.
                    act.steer_pending.store(false, Ordering::SeqCst);
                }
            }
            "item/agentMessage/delta" => {
                if live && !act.steer_pending.load(Ordering::SeqCst) {
                    if let Some(d) = v["params"]["delta"].as_str() {
                        if let Some(i) = &inner {
                            if let Some(t0) = *act.steer_sent_at.lock().unwrap() {
                                let mut s = i.steer.lock().unwrap();
                                if let Some(s) = s.as_mut().filter(|s| s.turn_id == turn_id && s.first_token_ms.is_none()) {
                                    s.first_token_ms = Some(ms_since(t0));
                                }
                            }
                        }
                        act.sink.lock().unwrap().send(AttemptMsg::Token(d.to_owned()));
                    }
                }
            }
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                if live {
                    let n = v["params"]["delta"].as_str().map(|s| s.chars().count()).unwrap_or(0);
                    act.sink.lock().unwrap().send(AttemptMsg::Reasoning(n));
                }
            }
            "error" => {
                if !v["params"]["willRetry"].as_bool().unwrap_or(false) {
                    last_error = v["params"]["error"]["message"].as_str().map(str::to_owned);
                }
            }
            "turn/completed" => {
                let turn = &v["params"]["turn"];
                let status = turn["status"].as_str().unwrap_or("?").to_owned();
                if let Some(c) = cancelled_at {
                    stats.cancel_to_completed_ms = Some(ms_since(c));
                    stats.final_status = Some(status);
                    if let Some(s) = &inner { *s.interrupt.lock().unwrap() = Some(stats.clone()); }
                    // Whoever owns the output now did not cancel (e.g. it steered just as the turn
                    // was being interrupted): end its attempt instead of leaving it waiting.
                    let sink = act.sink.lock().unwrap();
                    if !sink.cancel.is_cancelled() {
                        sink.send(AttemptMsg::Failed("codex turn was interrupted".into()));
                    }
                } else if act.steer_pending.load(Ordering::SeqCst) && status == "completed" {
                    // Accepted steer never consumed; `turn()` re-asks it as a new turn.
                    act.steer_unanswered.store(true, Ordering::SeqCst);
                } else {
                    let msg = match status.as_str() {
                        "completed" => AttemptMsg::Finished,
                        "failed" => AttemptMsg::Failed(format!(
                            "codex turn failed: {}",
                            turn["error"]["message"].as_str().map(str::to_owned).or(last_error.clone()).unwrap_or_else(|| "unknown error".into())
                        )),
                        other => AttemptMsg::Failed(format!("codex turn ended: {other}")),
                    };
                    act.sink.lock().unwrap().send(msg);
                }
                break;
            }
            _ => {}
        }
    }
    finish(&act);
}

/// Attempt wrapper for the core: returns as soon as `cancel` fires (the turn task keeps running
/// in the background just long enough to send `turn/interrupt` and see `turn/completed`).
pub(crate) async fn attempt(engine: CodexEngine, text: String, cancel: CancellationToken, idx: usize, tx: mpsc::UnboundedSender<(usize, AttemptMsg)>) {
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let c = cancel.clone();
    tokio::spawn(async move {
        engine.turn(text, c, idx, tx).await;
        let _ = done_tx.send(());
    });
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {}
        _ = done_rx => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pol() -> WorkspacePolicy {
        WorkspacePolicy::new(Path::new("/srv/小呆 Server"))
    }

    #[test]
    fn elicitation_accepted_only_for_the_delegate_server() {
        let items = HashMap::new();
        let (r, _) = pol().handle("mcpServer/elicitation/request", &json!({"serverName":"xiaodai_grok_bot"}), &items);
        assert_eq!(r.unwrap()["action"], "decline", "no delegate configured");
        let p = pol().with_delegate(Some("xiaodai_grok_bot"));
        let (r, _) = p.handle("mcpServer/elicitation/request", &json!({"serverName":"xiaodai_grok_bot"}), &items);
        assert_eq!(r.unwrap()["action"], "accept");
        let (r, _) = p.handle("mcpServer/elicitation/request", &json!({"serverName":"other"}), &items);
        assert_eq!(r.unwrap()["action"], "decline");
    }

    #[test]
    fn inside_root_checks() {
        let p = pol();
        assert!(p.inside("/srv/小呆 Server/runtime/tmp/x", None));
        assert!(p.inside("runtime/tmp/x", None));
        assert!(!p.inside("/srv/小呆 Server/../etc/passwd", None));
        assert!(!p.inside("/srv/小呆 ServerX/a", None));
        assert!(!p.inside("../outside", None));
        assert!(!p.inside("/etc/hosts", None));
    }

    #[test]
    fn shell_words_respects_quotes() {
        assert_eq!(shell_words(r#"cat "/srv/小呆 Server/a b.txt" 'x y'"#), vec!["cat", "/srv/小呆 Server/a b.txt", "x y"]);
        assert_eq!(shell_words(r"ls a\ b; rm /x"), vec!["ls", "a b", "rm", "/x"]);
    }

    #[test]
    fn command_approval_policy() {
        let p = pol();
        let inside = "/srv/小呆 Server";
        assert!(p.command_ok(&json!("ls -la runtime"), Some(inside)));
        assert!(p.command_ok(&json!(r#"cat "/srv/小呆 Server/brain.json""#), Some(inside)));
        assert!(p.command_ok(&json!(["/bin/ls", "-la"]), Some(inside)));
        assert!(!p.command_ok(&json!("rm -rf /Users/jason/GitHub"), Some(inside)));
        assert!(!p.command_ok(&json!("cat ../../etc/passwd"), Some(inside)));
        assert!(!p.command_ok(&json!("ls"), Some("/tmp")));
        assert!(!p.command_ok(&json!("ls"), None));
        assert!(!p.command_ok(&json!("cp a --target=/etc"), Some(inside)));
    }

    #[test]
    fn server_request_answers() {
        let p = pol();
        let mut items = HashMap::new();
        items.insert("f1".to_string(), vec!["/srv/小呆 Server/runtime/tmp/a.txt".to_string()]);
        items.insert("f2".to_string(), vec!["/etc/hosts".to_string()]);
        let (r, _) = p.handle("item/fileChange/requestApproval", &json!({"itemId":"f1"}), &items);
        assert_eq!(r.unwrap()["decision"], "accept");
        let (r, _) = p.handle("item/fileChange/requestApproval", &json!({"itemId":"f2"}), &items);
        assert_eq!(r.unwrap()["decision"], "decline");
        let (r, _) = p.handle("item/fileChange/requestApproval", &json!({"itemId":"unknown"}), &items);
        assert_eq!(r.unwrap()["decision"], "decline");
        let (r, _) = p.handle("currentTime/read", &json!({"threadId":"t"}), &items);
        assert!(r.unwrap()["currentTimeAt"].as_u64().unwrap() > 1_700_000_000);
        let (r, _) = p.handle("account/chatgptAuthTokens/refresh", &json!({}), &items);
        assert_eq!(r.unwrap_err().0, -32601);
        let (r, _) = p.handle("item/permissions/requestApproval", &json!({"permissions":{"fileSystem":{"write":["/srv/小呆 Server/x"]}}}), &items);
        assert_eq!(r.unwrap()["permissions"]["fileSystem"]["write"][0], "/srv/小呆 Server/x");
        let (r, _) = p.handle("item/permissions/requestApproval", &json!({"permissions":{"fileSystem":{"write":["/"]}}}), &items);
        assert_eq!(r.unwrap()["permissions"], json!({}));
    }

    #[test]
    fn model_names_normalize() {
        assert_eq!(norm_model("GPT-6 Luna"), "gpt-6-luna");
        assert_eq!(norm_model("gpt-6-luna"), "gpt-6-luna");
        assert_ne!(norm_model("gpt-5.6-luna"), norm_model("gpt-6 luna"));
    }

    /// Framing: split writes, CRLF, blank lines, string ids from the server, and EOF handling.
    #[tokio::test]
    async fn jsonrpc_framing_and_dispatch() {
        let (client_end, server_end) = tokio::io::duplex(64);
        let (cr, cw) = tokio::io::split(client_end);
        let conn = RpcConn::from_io(Box::pin(cr), Box::pin(cw), pol());
        let (sr, mut sw) = tokio::io::split(server_end);
        let mut sr = BufReader::new(sr).lines();
        let mut sub = conn.subscribe("th1");
        let c2 = Arc::new(conn);
        let c3 = c2.clone();
        let req = tokio::spawn(async move { c3.request("model/list", json!({}), Duration::from_secs(2)).await });
        let line = sr.next_line().await.unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["method"], "model/list");
        assert!(v.get("jsonrpc").is_none());
        let id = v["id"].as_i64().unwrap();
        // Notification + server request + response, written in awkward fragments.
        let blob = format!(
            "{{\"method\":\"item/agentMessage/delta\",\"params\":{{\"threadId\":\"th1\",\"turnId\":\"t\",\"itemId\":\"i\",\"delta\":\"你好\"}}}}\r\n\n{{\"id\":\"srv-7\",\"method\":\"currentTime/read\",\"params\":{{\"threadId\":\"th1\"}}}}\n{{\"id\":{id},\"result\":{{\"data\":[]}}}}\n"
        );
        for chunk in blob.as_bytes().chunks(7) {
            sw.write_all(chunk).await.unwrap();
            sw.flush().await.unwrap();
        }
        let n = sub.recv().await.unwrap();
        assert_eq!(n["params"]["delta"], "你好");
        let reply: Value = serde_json::from_str(&sr.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(reply["id"], "srv-7");
        assert!(reply["result"]["currentTimeAt"].is_u64());
        assert_eq!(req.await.unwrap().unwrap()["data"], json!([]));
        // Error response.
        let c4 = c2.clone();
        let req = tokio::spawn(async move { c4.request("thread/start", json!({}), Duration::from_secs(2)).await });
        let v: Value = serde_json::from_str(&sr.next_line().await.unwrap().unwrap()).unwrap();
        sw.write_all(format!("{{\"id\":{},\"error\":{{\"code\":-1,\"message\":\"nope\"}}}}\n", v["id"]).as_bytes()).await.unwrap();
        assert!(req.await.unwrap().unwrap_err().contains("nope"));
        // EOF: pending requests fail, subscriptions close, conn is dead.
        let c5 = c2.clone();
        let req = tokio::spawn(async move { c5.request("turn/start", json!({}), Duration::from_secs(5)).await });
        let _ = sr.next_line().await;
        drop(sw);
        drop(sr);
        assert!(req.await.unwrap().unwrap_err().contains("exited"));
        assert!(sub.recv().await.is_none());
        assert!(!c2.is_alive());
    }
}

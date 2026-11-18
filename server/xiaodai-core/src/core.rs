//! The core: race/failover across routes, stream sentences, cancel promptly.
use crate::client::{self, AttemptMsg, Secret};
use crate::codex::{self, CodexEngine, CodexWarmup, Connector};
use crate::config::{Config, EngineMode, Route};
use crate::health::{self, EngineStats, Health, HealthSnapshot, LunaState, Sample, SampleOutcome};
use crate::net::{ActiveInterface, DirectNet, HttpNetwork};
use crate::splitter::{has_visible, SentenceSplitter};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::{CancellationToken, DropGuard};

#[derive(Debug)]
pub struct CoreError(pub String);
impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for CoreError {}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}
impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self { role: "user".into(), content: text.into() }
    }
}

/// All times are ms since `respond` was called.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Timings {
    /// Engine that produced the answer: "codex" or "http".
    pub engine: Option<String>,
    /// Route that produced the answer ("codex/<model>" or an HTTP route id).
    pub route: Option<String>,
    /// Routes that were started (in order).
    pub routes_started: Vec<String>,
    pub first_token_ms: Option<u64>,
    pub first_sentence_ms: Option<u64>,
    pub total_ms: u64,
    /// Thinking chars received from the winner/racers (should be 0 on the fast path).
    pub reasoning_chars: usize,
    /// Per-route failures seen before a winner (no key values, ever).
    pub errors: Vec<String>,
    /// Ended because a follow-up utterance took over the Codex turn (barge-in steer/interrupt).
    pub superseded: bool,
    /// Dual mode: Luna-first policy when the turn was planned ("preferred" / "degraded: <why>").
    pub luna: Option<String>,
    /// Network path of the winning HTTP route: interface name (direct) or "system".
    pub network: Option<String>,
    /// Every attempt that was started, as recorded in the latency log.
    pub attempts: Vec<AttemptTiming>,
    /// Rolling per-engine time-to-first-text stats after this turn was recorded ("codex" = Luna,
    /// else HTTP route ids; failures and timeouts included). Same data as `Core::health()`.
    pub latency: Vec<EngineStats>,
}

/// One engine attempt inside a turn.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AttemptTiming {
    pub route: String,
    /// When it started (ms since `respond`).
    pub started_ms: u64,
    /// Its own time to first visible text (from its start).
    pub ttft_ms: Option<u64>,
    /// None = not judged (turn cancelled / superseded before it spoke).
    pub outcome: Option<SampleOutcome>,
    pub network: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub enum Event {
    FirstToken { route: String, ms: u64 },
    Sentence { index: usize, text: String, ms: u64 },
    Done { text: String, timings: Timings },
    Cancelled { text: String, timings: Timings },
    Error { message: String, timings: Timings },
}

impl Event {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Event::Done { .. } | Event::Cancelled { .. } | Event::Error { .. })
    }
}

/// One in-flight answer. Dropping it cancels the turn.
pub struct Turn {
    rx: mpsc::UnboundedReceiver<Event>,
    cancel: CancellationToken,
    _guard: DropGuard,
}

impl Turn {
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

#[derive(Debug, Clone)]
pub struct WarmupResult {
    pub route: String,
    pub result: Result<u64, String>,
    /// HTTP routes: interface name (direct) or "system".
    pub network: Option<String>,
}

#[derive(Clone)]
pub struct Core {
    client: Client,
    config: Arc<Config>,
    keys: Arc<HashMap<String, Secret>>,
    codex: CodexEngine,
    direct: Arc<DirectNet>,
    health: Arc<Health>,
}

impl Core {
    /// Builds the shared HTTP/2 client, reads route keys from the process environment, and
    /// prepares (but does not spawn) the Codex engine. Codex settings are fixed here.
    pub fn new(config: Config) -> Result<Self, CoreError> {
        let codex = CodexEngine::new(config.codex.clone(), &config.workspace_root);
        Self::build(config, codex)
    }

    /// Like `new`, but the Codex engine talks to whatever `connector` returns (tests / fakes).
    pub fn with_codex_connector(config: Config, connector: Connector) -> Result<Self, CoreError> {
        let codex = CodexEngine::with_connector(config.codex.clone(), &config.workspace_root, connector);
        Self::build(config, codex)
    }

    fn build(config: Config, codex: CodexEngine) -> Result<Self, CoreError> {
        for id in &config.order {
            if !config.routes.iter().any(|r| &r.id == id) {
                return Err(CoreError(format!("order references unknown route '{id}'")));
            }
        }
        let client = client::build_client().map_err(|e| CoreError(format!("http client: {e}")))?;
        let mut keys = HashMap::new();
        for r in &config.routes {
            if r.api_key_env.is_empty() {
                continue;
            }
            if let Ok(v) = std::env::var(&r.api_key_env) {
                let v = v.trim().to_owned();
                if !v.is_empty() {
                    keys.insert(r.id.clone(), Secret::new(v));
                }
            }
        }
        let health = Arc::new(Health::new(config.health.clone()));
        Ok(Self { client, config: Arc::new(config), keys: Arc::new(keys), codex, direct: Default::default(), health })
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Same client, Codex engine and latency history, with route keys supplied by the caller
    /// instead of (or on top of) the process environment. `keys` maps a route's `api_key_env`
    /// name to its value; an empty value removes that route's key. Values are never logged.
    pub fn with_keys<I, K, V>(&self, keys: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut map = (*self.keys).clone();
        for (name, value) in keys {
            let value = value.as_ref().trim();
            for r in self.config.routes.iter().filter(|r| !r.api_key_env.is_empty() && r.api_key_env == name.as_ref()) {
                if value.is_empty() {
                    map.remove(&r.id);
                } else {
                    map.insert(r.id.clone(), Secret::new(value.to_owned()));
                }
            }
        }
        Self { keys: Arc::new(map), ..self.clone() }
    }

    /// The shared Codex engine (cold-start report, interrupt stats, approval audit).
    pub fn codex(&self) -> &CodexEngine {
        &self.codex
    }

    /// Same client/connection pool, keys and Codex engine; different settings (e.g. mode, route order).
    pub fn reconfigure(&self, f: impl FnOnce(&mut Config)) -> Self {
        let mut c = (*self.config).clone();
        f(&mut c);
        Self { config: Arc::new(c), ..self.clone() }
    }

    /// Stop the Codex child (e.g. the owning service stops). HTTP routes keep working; Codex
    /// stays down, and background probes stop, until `reopen`.
    pub async fn shutdown(&self) {
        self.codex.shutdown().await;
    }

    /// Allow Codex to spawn again after `shutdown` (the next turn or `warmup` starts it).
    pub fn reopen(&self) {
        self.codex.reopen();
    }

    /// Rolling per-engine latency stats, Luna-first state and recent switches.
    pub fn health(&self) -> HealthSnapshot {
        self.health.snapshot()
    }

    pub fn luna_state(&self) -> LunaState {
        self.health.luna_state()
    }

    /// The active Wi-Fi/Ethernet interface direct routes would use (None = not found / not macOS).
    pub async fn active_interface(&self) -> Option<ActiveInterface> {
        self.direct.interface().await
    }

    /// Client + network label for one HTTP route, per `http_network`.
    async fn http_client(&self, r: &Route) -> Result<(Client, String), String> {
        if !r.direct || self.config.http_network == HttpNetwork::System {
            return Ok((self.client.clone(), "system".into()));
        }
        match self.direct.client().await {
            Some((iface, c)) => Ok((c, iface.name)),
            None if self.config.http_network == HttpNetwork::Auto => Ok((self.client.clone(), "system".into())),
            None => Err("no active Wi-Fi/Ethernet interface for a direct route".into()),
        }
    }

    /// While Luna is degraded (dual mode), probe it in the background until it recovers.
    fn ensure_probe(&self) {
        if self.health.probing.swap(true, Ordering::SeqCst) {
            return;
        }
        let codex = self.codex.clone();
        let health = self.health.clone();
        tokio::spawn(async move {
            let c = health.config().clone();
            loop {
                tokio::time::sleep(Duration::from_millis(c.probe_interval_ms)).await;
                if codex.is_shut_down() || health.luna_degraded().is_none() {
                    break;
                }
                let t = Instant::now();
                let r = codex.probe(Duration::from_millis(c.probe_timeout_ms)).await;
                let elapsed = t.elapsed().as_millis() as u64;
                let (outcome, ttft, error) = match r {
                    Ok(ms) => (SampleOutcome::Ok, Some(ms), None),
                    Err(e) => (if is_timeout(&e) { SampleOutcome::Timeout } else { SampleOutcome::Failed }, None, Some(crate::sse::truncate(&e, 200).to_owned())),
                };
                health.record(Sample { at_ms: health::now_ms(), key: health::LUNA.into(), route: codex.label(), outcome, ttft_ms: ttft, elapsed_ms: elapsed, probe: true, network: None, error });
                if health.luna_degraded().is_none() {
                    break;
                }
            }
            health.probing.store(false, Ordering::SeqCst);
        });
    }

    /// Ordered HTTP routes that can actually be called (key present or no auth needed).
    pub fn available_routes(&self) -> Vec<String> {
        self.routes().into_iter().map(|r| r.id).collect()
    }

    fn routes(&self) -> Vec<Route> {
        self.config
            .order
            .iter()
            .filter_map(|id| self.config.routes.iter().find(|r| &r.id == id))
            .filter(|r| r.api_key_env.is_empty() || self.keys.contains_key(&r.id))
            .cloned()
            .collect()
    }

    fn codex_enabled(&self) -> bool {
        self.config.mode != EngineMode::HttpOnly
    }

    fn http_enabled(&self) -> bool {
        self.config.mode != EngineMode::CodexOnly
    }

    /// Pre-open HTTP connections and (unless http-only) start + prime the Codex app-server,
    /// concurrently. Codex shows up as route "codex" (Ok = its cold-start ms).
    pub async fn warmup(&self) -> Vec<WarmupResult> {
        let http = async {
            if !self.http_enabled() {
                return Vec::new();
            }
            let futs = self.routes().into_iter().map(|r| async move {
                match self.http_client(&r).await {
                    Ok((client, net)) => WarmupResult { result: client::warmup(&client, &r).await, route: r.id, network: Some(net) },
                    Err(e) => WarmupResult { result: Err(e), route: r.id, network: None },
                }
            });
            futures_util::future::join_all(futs).await
        };
        let codex = async {
            if !self.codex_enabled() {
                return None;
            }
            let w = self.codex.warmup().await;
            Some(WarmupResult { route: "codex".into(), result: match w.error { Some(e) => Err(e), None => Ok(w.total_ms as u64) }, network: None })
        };
        let (mut v, c) = tokio::join!(http, codex);
        v.extend(c);
        v
    }

    /// Codex cold start details (spawn, initialize, model list, thread/start, priming turn).
    pub async fn warmup_codex(&self) -> CodexWarmup {
        self.codex.warmup().await
    }

    pub fn respond(&self, text: &str, cancel: CancellationToken) -> Turn {
        self.respond_messages(vec![Message::user(text)], cancel)
    }

    /// `messages` excludes the system prompt (added from config). The Codex engine keeps its own
    /// thread context (one thread for the whole conversation) and only receives the last user
    /// message. Calling this while a Codex turn is still speaking applies `codex.barge_in`:
    /// `steer` hands the running turn to the new `Turn` (the old one ends `Cancelled` with
    /// `timings.superseded`), `interrupt` stops it and starts a new turn on the same thread.
    pub fn respond_messages(&self, messages: Vec<Message>, cancel: CancellationToken) -> Turn {
        self.start_messages(messages, cancel, true)
    }

    /// Start a distinct question on the conversation thread, including while a previous
    /// cancelled turn is winding down. Only explicit follow-ups should steer its output.
    pub fn respond_fresh(&self, messages: Vec<Message>, cancel: CancellationToken) -> Turn {
        self.start_messages(messages, cancel, false)
    }

    fn start_messages(&self, messages: Vec<Message>, cancel: CancellationToken, allow_steer: bool) -> Turn {
        let (tx, rx) = mpsc::unbounded_channel();
        let token = cancel.child_token();
        // Barge-in: reserve a still-running Codex turn for this follow-up before anything can
        // cancel it (e.g. the caller dropping the previous Turn), so it can be steered.
        if allow_steer && self.codex_enabled() {
            self.codex.claim_for_followup(&token);
        }
        let guard = token.clone().drop_guard();
        tokio::spawn(drive(self.clone(), messages, token.clone(), tx, allow_steer));
        Turn { rx, cancel: token, _guard: guard }
    }
}

fn build_body(route: &Route, cfg: &Config, messages: &[Message]) -> Value {
    let mut msgs = Vec::with_capacity(messages.len() + 1);
    if !cfg.system_prompt.is_empty() {
        msgs.push(json!({ "role": "system", "content": cfg.system_prompt }));
    }
    // "delta" context is for the persistent Codex thread only; stateless routes get the full context.
    msgs.extend(messages.iter().filter(|m| m.role != "delta").map(|m| json!({ "role": m.role, "content": m.content })));
    let mut body = json!({
        "model": route.model,
        "messages": msgs,
        "stream": true,
        "max_tokens": cfg.max_tokens,
        "temperature": cfg.temperature,
    });
    if let (Some(obj), Some(extra)) = (body.as_object_mut(), route.extra_body.as_object()) {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
    body
}

enum Outcome {
    Done,
    Cancelled,
    Failed(String),
}

#[derive(Clone)]
enum Cand {
    Codex,
    Http(Route),
}

impl Cand {
    fn engine(&self) -> &'static str {
        match self {
            Cand::Codex => "codex",
            Cand::Http(_) => "http",
        }
    }
}

/// Candidates + start schedule: stage k starts `delay_ms` after stage k-1, or at once on a failure
/// without a winner. A stage with `None` delay is failure-only: it starts only once everything
/// started so far has failed.
/// Dual mode with Luna degraded (Luna-first policy): HTTP leads at t=0, fastest recent route
/// first, and Luna is left out (the background probe decides when it comes back). Falls back to
/// the normal plan when no HTTP route is usable. While degraded Luna is kept as a failure-only last
/// resort, so a turn still gets an answer when every HTTP route fails.
/// (candidate indexes, delay before starting; None = failure-only)
type Stage = (Vec<usize>, Option<u64>);

fn plan(core: &Core, degraded: bool) -> (Vec<Cand>, Vec<Stage>) {
    let cfg = &core.config;
    let mut cands = Vec::new();
    let mut stages: Vec<Stage> = Vec::new();
    let mut routes = core.routes();
    let lead_http = degraded && core.config.mode == EngineMode::Dual && !routes.is_empty();
    if lead_http {
        let ids: Vec<String> = routes.iter().map(|r| r.id.clone()).collect();
        let order = core.health.rank(&ids);
        routes.sort_by_key(|r| order.iter().position(|id| id == &r.id).unwrap_or(usize::MAX));
    }
    let codex_first = core.codex_enabled() && !lead_http;
    if codex_first {
        cands.push(Cand::Codex);
        stages.push((vec![0], Some(0)));
    }
    if core.http_enabled() {
        let first_delay = if codex_first { cfg.codex.fallback_after_ms } else { 0 };
        let base = cands.len();
        cands.extend(routes.into_iter().map(Cand::Http));
        let idx: Vec<usize> = (base..cands.len()).collect();
        if idx.is_empty() {
        } else if cfg.http_race_at_start {
            stages.push((idx, Some(first_delay)));
        } else {
            for (k, i) in idx.into_iter().enumerate() {
                stages.push((vec![i], Some(if k == 0 { first_delay } else { cfg.first_token_timeout_ms })));
            }
        }
    }
    if lead_http && core.codex_enabled() {
        cands.push(Cand::Codex);
        stages.push((vec![cands.len() - 1], None));
    }
    (cands, stages)
}

async fn drive(core: Core, messages: Vec<Message>, cancel: CancellationToken, out: mpsc::UnboundedSender<Event>, allow_steer: bool) {
    let start = Instant::now();
    let ms = || start.elapsed().as_millis() as u64;
    let cfg = core.config.clone();
    let dual = cfg.mode == EngineMode::Dual;
    let degraded = if dual { core.health.luna_degraded() } else { None };
    if degraded.is_some() {
        core.ensure_probe();
    }
    let (cands, stages) = plan(&core, degraded.is_some());
    let mut t = Timings::default();
    if dual {
        t.luna = Some(match &degraded { Some(r) => format!("degraded: {r}"), None => "preferred".into() });
    }
    if cands.is_empty() {
        t.total_ms = ms();
        let msg = if core.codex_enabled() { "no engine available" } else { "no route has an API key (check .env / environment)" };
        let _ = out.send(Event::Error { message: msg.into(), timings: t });
        return;
    }
    let last_user = messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()).unwrap_or_default();
    // Per-turn context supplied by the caller as `system` messages (e.g. a brain's memory recall).
    // HTTP routes receive them as system messages after the configured prompt; the Codex thread
    // only takes one text input per turn, so the context is prefixed to the user text there.
    // A caller may add role "delta" messages: only what the thread has not seen yet. The Codex
    // engine sends them instead of the full context when its thread is not new.
    let codex_text = {
        let join = |role: &str| -> String {
            let context: Vec<&str> = messages.iter().filter(|m| m.role == role && !m.content.trim().is_empty()).map(|m| m.content.as_str()).collect();
            if context.is_empty() { last_user.clone() } else { format!("{}\n\n{}", context.join("\n"), last_user) }
        };
        let delta = messages.iter().any(|m| m.role == "delta").then(|| join("delta"));
        codex::TurnText { full: join("system"), delta }
    };
    let label = |c: &Cand| match c {
        Cand::Codex => core.codex.label(),
        Cand::Http(r) => r.id.clone(),
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<(usize, AttemptMsg)>();
    let mut attempts: Vec<(CancellationToken, JoinHandle<()>)> = Vec::new();
    let mut started = vec![false; cands.len()];
    let mut failed = vec![false; cands.len()];
    let mut tokens: Vec<Option<CancellationToken>> = vec![None; cands.len()];
    // Per-attempt bookkeeping for the latency log (ms since `respond`; u64::MAX = unset).
    let starts: Vec<AtomicU64> = (0..cands.len()).map(|_| AtomicU64::new(u64::MAX)).collect();
    let mut first_at: Vec<Option<u64>> = vec![None; cands.len()];
    let mut errs: Vec<Option<String>> = vec![None; cands.len()];
    let mut nets: Vec<Option<String>> = vec![None; cands.len()];
    let mut winner: Option<usize> = None;
    let mut splitter = SentenceSplitter::new(cfg.max_sentence_chars);
    let mut full = String::new();
    let mut sentences = 0usize;
    let req_timeout = Duration::from_millis(cfg.request_timeout_ms);

    let spawn = |i: usize, attempts: &mut Vec<(CancellationToken, JoinHandle<()>)>, t: &mut Timings, started: &mut Vec<bool>, tokens: &mut Vec<Option<CancellationToken>>| {
        let tok = cancel.child_token();
        started[i] = true;
        starts[i].store(start.elapsed().as_millis() as u64, Ordering::SeqCst);
        tokens[i] = Some(tok.clone());
        let h = match &cands[i] {
            Cand::Codex => {
                t.routes_started.push("codex".into());
                tokio::spawn(codex::attempt(core.codex.clone(), codex_text.clone(), tok.clone(), i, tx.clone(), allow_steer))
            }
            Cand::Http(r) => {
                t.routes_started.push(r.id.clone());
                let body = build_body(r, &cfg, &messages);
                let key = core.keys.get(&r.id).cloned();
                let (core, r, tok2, tx) = (core.clone(), r.clone(), tok.clone(), tx.clone());
                tokio::spawn(async move {
                    match core.http_client(&r).await {
                        Ok((c, net)) => {
                            let _ = tx.send((i, AttemptMsg::Network(net)));
                            client::attempt(c, r, key, body, req_timeout, tok2, i, tx).await
                        }
                        Err(e) => {
                            let _ = tx.send((i, AttemptMsg::Failed(e)));
                        }
                    }
                })
            }
        };
        attempts.push((tok, h));
    };

    let mut next_stage = 0usize;
    let start_stage = |k: usize, attempts: &mut Vec<(CancellationToken, JoinHandle<()>)>, t: &mut Timings, started: &mut Vec<bool>, tokens: &mut Vec<Option<CancellationToken>>| {
        for &i in &stages[k].0 {
            spawn(i, attempts, t, started, tokens);
        }
    };
    start_stage(0, &mut attempts, &mut t, &mut started, &mut tokens);
    next_stage = next_stage.max(1);
    let stage_deadline = |k: usize| stages.get(k).and_then(|s| s.1).map(|d| tokio::time::Instant::now() + Duration::from_millis(d));
    let mut deadline = stage_deadline(next_stage);

    let emit = |s: String, sentences: &mut usize, t: &mut Timings| {
        let now = ms();
        if t.first_sentence_ms.is_none() {
            t.first_sentence_ms = Some(now);
        }
        let _ = out.send(Event::Sentence { index: *sentences, text: s, ms: now });
        *sentences += 1;
    };

    let outcome = loop {
        let can_start_more = winner.is_none() && next_stage < stages.len();
        let sleep_until = deadline.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Outcome::Cancelled,
            msg = rx.recv() => {
                let Some((i, msg)) = msg else { break Outcome::Failed("internal: attempt channel closed".into()) };
                match msg {
                    AttemptMsg::Network(n) => nets[i] = Some(n),
                    AttemptMsg::Token(s) => {
                        if first_at[i].is_none() && has_visible(&s) {
                            first_at[i] = Some(ms());
                        }
                        if winner.is_none() && has_visible(&s) {
                            winner = Some(i);
                            for (j, tok) in tokens.iter().enumerate() {
                                if j != i { if let Some(tok) = tok { tok.cancel(); } }
                            }
                            let now = ms();
                            t.engine = Some(cands[i].engine().into());
                            t.route = Some(label(&cands[i]));
                            t.first_token_ms = Some(now);
                            t.network = nets[i].clone();
                            let _ = out.send(Event::FirstToken { route: label(&cands[i]), ms: now });
                        }
                        if winner == Some(i) {
                            full.push_str(&s);
                            for sentence in splitter.push(&s) {
                                emit(sentence, &mut sentences, &mut t);
                            }
                        }
                    }
                    AttemptMsg::Reasoning(n) => {
                        if winner.is_none() || winner == Some(i) { t.reasoning_chars += n; }
                    }
                    AttemptMsg::Superseded => {
                        if winner.is_none() || winner == Some(i) {
                            t.superseded = true;
                            break Outcome::Cancelled;
                        }
                    }
                    AttemptMsg::Finished | AttemptMsg::Failed(_) if winner == Some(i) => {
                        match msg {
                            AttemptMsg::Failed(e) => break Outcome::Failed(e),
                            _ => break Outcome::Done,
                        }
                    }
                    AttemptMsg::Finished | AttemptMsg::Failed(_) => {
                        if winner.is_some() || failed[i] { continue; } // a cancelled loser; ignore
                        let why = match msg { AttemptMsg::Failed(e) => e, _ => "empty response".into() };
                        errs[i] = Some(why.clone());
                        t.errors.push(format!("{}: {}", label(&cands[i]), why));
                        failed[i] = true;
                        let all_failed = (0..cands.len()).filter(|&j| started[j]).all(|j| failed[j]);
                        if next_stage < stages.len() && (stages[next_stage].1.is_some() || all_failed) {
                            // Immediate failover on error.
                            start_stage(next_stage, &mut attempts, &mut t, &mut started, &mut tokens);
                            next_stage += 1;
                            deadline = stage_deadline(next_stage);
                        } else if all_failed && next_stage >= stages.len() {
                            break Outcome::Failed(t.errors.join("; "));
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(sleep_until), if can_start_more && deadline.is_some() => {
                // Slow first token: start the next stage; whoever speaks first wins.
                start_stage(next_stage, &mut attempts, &mut t, &mut started, &mut tokens);
                next_stage += 1;
                deadline = stage_deadline(next_stage);
            }
        }
    };

    for (tok, _) in &attempts {
        tok.cancel();
    }
    for (_, h) in attempts {
        let _ = h.await;
    }
    // Latency log: one sample per started attempt that can be judged.
    let win_ms = winner.and_then(|w| first_at[w]);
    for i in 0..cands.len() {
        if !started[i] {
            continue;
        }
        let st = starts[i].load(Ordering::SeqCst);
        let st = if st == u64::MAX { 0 } else { st };
        let (outcome, ttft, elapsed) = if let Some(f) = first_at[i] {
            (Some(SampleOutcome::Ok), Some(f.saturating_sub(st)), f.saturating_sub(st))
        } else if let Some(e) = &errs[i] {
            (Some(if is_timeout(e) { SampleOutcome::Timeout } else { SampleOutcome::Failed }), None, ms().saturating_sub(st))
        } else if let Some(w) = win_ms {
            (Some(SampleOutcome::Lost), None, w.saturating_sub(st))
        } else {
            (None, None, ms().saturating_sub(st))
        };
        let (key, route) = match &cands[i] {
            Cand::Codex => (health::LUNA.to_owned(), label(&cands[i])),
            Cand::Http(r) => (r.id.clone(), r.id.clone()),
        };
        let error = errs[i].as_deref().map(|e| crate::sse::truncate(e, 200).to_owned());
        if let Some(o) = outcome {
            core.health.record(Sample { at_ms: health::now_ms(), key, route: route.clone(), outcome: o, ttft_ms: ttft, elapsed_ms: elapsed, probe: false, network: nets[i].clone(), error: error.clone() });
        }
        t.attempts.push(AttemptTiming { route, started_ms: st, ttft_ms: ttft, outcome, network: nets[i].clone(), error });
    }
    t.latency = core.health.snapshot().engines;
    t.total_ms = ms();
    let ev = match outcome {
        Outcome::Done => {
            if let Some(s) = splitter.flush() {
                emit(s, &mut sentences, &mut t);
            }
            t.total_ms = ms();
            Event::Done { text: full, timings: t }
        }
        Outcome::Cancelled => Event::Cancelled { text: full, timings: t },
        Outcome::Failed(e) => Event::Error { message: e, timings: t },
    };
    let _ = out.send(ev);
}

fn is_timeout(e: &str) -> bool {
    let e = e.to_ascii_lowercase();
    e.contains("timed out") || e.contains("timeout")
}

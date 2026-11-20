//! Codex engine against an in-process fake `codex app-server` over pipes (tokio duplex):
//! streaming, interrupt, approvals (accept inside workspace / decline outside), unknown server
//! requests, crash -> HTTP fallback, not-logged-in, missing model, dual-mode timing.
use fast_responder::codex::{BoxRead, BoxWrite, Connector};
use fast_responder::{BargeIn, CancellationToken, CodexConfig, Config, Core, EngineMode, Event, Route, Turn};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::sync::mpsc;

const ROOT: &str = "/tmp/xiaodai-fake-root";

#[derive(Default)]
struct FakeLog {
    methods: Mutex<Vec<String>>,
    /// Replies the client sent to our server-initiated requests: (our id, reply).
    replies: Mutex<Vec<(String, Value)>>,
    turn_inputs: Mutex<Vec<Value>>,
    spawns: AtomicUsize,
    /// Ordered protocol events: "start:<turn>", "completed:<turn>:<status>", "steer:<turn>".
    order: Mutex<Vec<String>>,
    steer_params: Mutex<Vec<Value>>,
    thread_params: Mutex<Vec<Value>>,
    /// Luna's health for every turn: 0 = normal, 1 = every turn fails, 2 = every turn is slow (3 s).
    luna: AtomicUsize,
}

#[derive(Clone, Copy)]
struct FakeOpts {
    logged_in: bool,
    has_luna: bool,
}

fn fake_connector(opts: FakeOpts, log: Arc<FakeLog>) -> Connector {
    Arc::new(move || {
        log.spawns.fetch_add(1, Ordering::SeqCst);
        let (client, server) = tokio::io::duplex(1 << 16);
        let (cr, cw) = tokio::io::split(client);
        tokio::spawn(fake_server(server, opts, log.clone()));
        Ok((Box::pin(cr) as BoxRead, Box::pin(cw) as BoxWrite))
    })
}

async fn fake_server(io: tokio::io::DuplexStream, opts: FakeOpts, log: Arc<FakeLog>) {
    let (r, mut w) = tokio::io::split(io);
    let mut lines = BufReader::new(r).lines();
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Value>();
    // Writer task; when the script asks to crash we drop everything.
    let crash = CancellationToken::new();
    let crash2 = crash.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = crash2.cancelled() => return,
                m = out_rx.recv() => {
                    let Some(m) = m else { return };
                    if w.write_all(format!("{m}\n").as_bytes()).await.is_err() { return; }
                }
            }
        }
    });
    let interrupts: Arc<Mutex<Vec<String>>> = Default::default();
    // turn id -> steer texts received; the running turn per thread; user texts per thread.
    let steers: Arc<Mutex<std::collections::HashMap<String, Vec<String>>>> = Default::default();
    let running: Arc<Mutex<std::collections::HashMap<String, String>>> = Default::default();
    let mut history: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut threads = 0;
    let mut turns = 0;
    loop {
        let line = tokio::select! {
            _ = crash.cancelled() => return,
            l = lines.next_line() => match l { Ok(Some(l)) => l, _ => return },
        };
        let v: Value = serde_json::from_str(&line).unwrap();
        let id = v["id"].clone();
        let method = v["method"].as_str().map(str::to_owned);
        let Some(method) = method else {
            // A reply to one of our server requests.
            log.replies.lock().unwrap().push((id.as_str().unwrap_or("").to_owned(), v.clone()));
            continue;
        };
        log.methods.lock().unwrap().push(method.clone());
        let reply = |result: Value| json!({ "id": id, "result": result });
        match method.as_str() {
            "initialize" => { out_tx.send(reply(json!({ "userAgent": "fake-codex/0.0" }))).ok(); }
            "initialized" => {}
            "account/read" => {
                let r = if opts.logged_in { json!({ "account": { "type": "chatgpt", "planType": "pro", "email": "x@example.com" }, "requiresOpenaiAuth": true }) } else { json!({ "account": null, "requiresOpenaiAuth": true }) };
                out_tx.send(reply(r)).ok();
            }
            "model/list" => {
                let mut data = vec![json!({ "id": "gpt-6-astra", "model": "gpt-6-astra", "displayName": "GPT-6-Astra", "description": "", "hidden": false, "isDefault": true, "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [{ "reasoningEffort": "medium", "description": "" }] })];
                if opts.has_luna {
                    data.push(json!({ "id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "GPT-6-Luna", "description": "", "hidden": false, "isDefault": false, "defaultReasoningEffort": "medium", "supportedReasoningEfforts": [{ "reasoningEffort": "medium", "description": "" }, { "reasoningEffort": "low", "description": "" }, { "reasoningEffort": "max", "description": "" }] }));
                }
                out_tx.send(reply(json!({ "data": data, "nextCursor": null }))).ok();
            }
            "thread/start" => {
                threads += 1;
                log.thread_params.lock().unwrap().push(v["params"].clone());
                assert_eq!(v["params"]["approvalPolicy"], "never");
                assert_eq!(v["params"]["sandbox"], "workspace-write");
                assert_eq!(v["params"]["cwd"], ROOT);
                out_tx.send(reply(json!({ "thread": { "id": format!("th-{threads}") }, "model": v["params"]["model"] }))).ok();
            }
            "turn/steer" => {
                log.steer_params.lock().unwrap().push(v["params"].clone());
                let tid = v["params"]["threadId"].as_str().unwrap_or("").to_owned();
                let want = v["params"]["expectedTurnId"].as_str().unwrap_or("").to_owned();
                let cur = running.lock().unwrap().get(&tid).cloned();
                if cur.as_deref() == Some(want.as_str()) {
                    let text = v["params"]["input"][0]["text"].as_str().unwrap_or("").to_owned();
                    log.order.lock().unwrap().push(format!("steer:{want}"));
                    steers.lock().unwrap().entry(want.clone()).or_default().push(text);
                    out_tx.send(reply(json!({ "turnId": want }))).ok();
                } else {
                    out_tx.send(json!({ "id": id, "error": { "code": -32600, "message": "expectedTurnId does not match the active turn" } })).ok();
                }
            }
            "turn/interrupt" => {
                interrupts.lock().unwrap().push(v["params"]["turnId"].as_str().unwrap_or("").to_owned());
                out_tx.send(reply(json!({}))).ok();
            }
            "turn/start" => {
                turns += 1;
                log.turn_inputs.lock().unwrap().push(v["params"].clone());
                let tid = v["params"]["threadId"].as_str().unwrap().to_owned();
                let turn = format!("tu-{turns}");
                let text = v["params"]["input"][0]["text"].as_str().unwrap_or("").to_owned();
                let earlier = history.entry(tid.clone()).or_default().clone();
                history.get_mut(&tid).unwrap().push(text.clone());
                log.order.lock().unwrap().push(format!("start:{turn}"));
                running.lock().unwrap().insert(tid.clone(), turn.clone());
                out_tx.send(reply(json!({ "turn": { "id": turn, "status": "inProgress", "items": [] } }))).ok();
                let out = out_tx.clone();
                let ints = interrupts.clone();
                let crash = crash.clone();
                let steers = steers.clone();
                let running = running.clone();
                let log2 = log.clone();
                tokio::spawn(async move {
                    let note = |m: &str, p: Value| { let mut p = p; p["threadId"] = json!(tid); p["turnId"] = json!(turn); json!({ "method": m, "params": p }) };
                    let delta = |d: &str| note("item/agentMessage/delta", json!({ "itemId": "m1", "delta": d }));
                    let done = |status: &str| {
                        log2.order.lock().unwrap().push(format!("completed:{turn}:{status}"));
                        let mut r = running.lock().unwrap();
                        if r.get(&tid) == Some(&turn) { r.remove(&tid); }
                        // The actual app-server terminal uses nested turn.id,
                        // not the top-level turnId carried by item events.
                        let mut event = note("turn/completed", json!({ "turn": { "id": turn, "status": status, "items": [] } }));
                        event["params"].as_object_mut().unwrap().remove("turnId");
                        event
                    };
                    out.send(note("turn/started", json!({}))).ok();
                    out.send(note("item/started", json!({ "item": { "type": "userMessage", "id": "u0" } }))).ok();
                    let luna = log2.luna.load(Ordering::SeqCst);
                    let text = match luna { 1 => "fail".to_string(), 2 => "slow".to_string(), _ => text };
                    match text.as_str() {
                        "steerable" | "steerable-final-only" => {
                            let mut old_text = String::new();
                            for i in 0..300 {
                                if ints.lock().unwrap().contains(&turn) { out.send(done("interrupted")).ok(); return; }
                                let s = steers.lock().unwrap().get(&turn).cloned().unwrap_or_default();
                                if !s.is_empty() {
                                    // The pre-steer answer runs on briefly, then the steer is consumed.
                                    out.send(delta("旧尾巴。")).ok();
                                    old_text.push_str("旧尾巴。");
                                    tokio::time::sleep(Duration::from_millis(10)).await;
                                    out.send(note("item/started", json!({ "item": { "type": "userMessage", "id": "u1" } }))).ok();
                                    let old_final = note("item/completed", json!({ "item": { "type": "agentMessage", "id": "m1", "text": old_text } }));
                                    out.send(old_final.clone()).ok();
                                    if text == "steerable-final-only" {
                                        out.send(note("item/started", json!({ "item": { "type": "agentMessage", "id": "m2", "text": "" } }))).ok();
                                        out.send(note("item/completed", json!({ "item": { "type": "agentMessage", "id": "m2", "text": format!("收到：{}。", s.join("+")) } }))).ok();
                                    } else {
                                        // A continuing item can expose only its new prefix as
                                        // deltas; completed must recover only the new tail.
                                        let next = format!("收到：{}", s.join("+"));
                                        out.send(delta(&next)).ok();
                                        out.send(old_final).ok(); // repeated old snapshot cannot fail the new owner
                                        let final_text = format!("{old_text}{next}。");
                                        let final_item = note("item/completed", json!({ "item": { "type": "agentMessage", "id": "m1", "text": final_text } }));
                                        out.send(final_item.clone()).ok();
                                        out.send(final_item).ok();
                                    }
                                    out.send(done("completed")).ok();
                                    return;
                                }
                                let next = format!("旧答案第{i}句。");
                                old_text.push_str(&next);
                                out.send(delta(&next)).ok();
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                            out.send(done("completed")).ok();
                        }
                        "我叫什么？" => {
                            let a = if earlier.iter().any(|t| t.contains("我叫小王")) { "你叫小王。" } else { "我不知道。" };
                            out.send(delta(a)).ok();
                            out.send(done("completed")).ok();
                        }
                        "stream" => {
                            out.send(note("item/reasoning/textDelta", json!({ "itemId": "r1", "delta": "思考中" }))).ok();
                            for d in ["你好", "！我是", "Codex。"] {
                                tokio::time::sleep(Duration::from_millis(20)).await;
                                out.send(delta(d)).ok();
                            }
                            out.send(done("completed")).ok();
                        }
                        "final-only" | "missing-tail" => {
                            out.send(note("item/started", json!({"item":{"type":"agentMessage","id":"m1","text":""}}))).ok();
                            if text == "missing-tail" { out.send(delta("你好！")).ok(); }
                            let final_item = note("item/completed", json!({"item":{"type":"agentMessage","id":"m1","text":"你好！完整的结尾。"}}));
                            out.send(final_item.clone()).ok();
                            out.send(final_item).ok(); // repeated snapshot must not duplicate speech
                            out.send(done("completed")).ok();
                        }
                        "stale-terminal" => {
                            out.send(json!({"method":"turn/completed","params":{"threadId":tid,"turn":{"id":"older-turn","status":"completed","items":[]}}})).ok();
                            out.send(delta("这是新问题的完整回答。")).ok();
                            out.send(done("completed")).ok();
                        }
                        "approval" => {
                            out.send(note("item/started", json!({ "item": { "type": "fileChange", "id": "fc-in", "changes": [{ "path": format!("{ROOT}/runtime/tmp/a.txt"), "kind": { "type": "add" } }] } }))).ok();
                            out.send(note("item/started", json!({ "item": { "type": "fileChange", "id": "fc-out", "changes": [{ "path": "/etc/hosts", "kind": { "type": "update" } }] } }))).ok();
                            let reqs = [
                                ("cmd-in", "item/commandExecution/requestApproval", json!({ "itemId": "c1", "startedAtMs": 0, "command": "ls -la runtime", "cwd": ROOT })),
                                ("cmd-out", "item/commandExecution/requestApproval", json!({ "itemId": "c2", "startedAtMs": 0, "command": "rm -rf /Users/someone", "cwd": ROOT })),
                                ("cmd-cwd-out", "item/commandExecution/requestApproval", json!({ "itemId": "c3", "startedAtMs": 0, "command": "ls", "cwd": "/etc" })),
                                ("fc-in", "item/fileChange/requestApproval", json!({ "itemId": "fc-in", "startedAtMs": 0 })),
                                ("fc-out", "item/fileChange/requestApproval", json!({ "itemId": "fc-out", "startedAtMs": 0 })),
                                ("time", "currentTime/read", json!({})),
                                ("weird", "attestation/generate", json!({})),
                            ];
                            for (rid, m, p) in reqs {
                                let mut p = p; p["threadId"] = json!(tid); p["turnId"] = json!(turn);
                                out.send(json!({ "id": rid, "method": m, "params": p })).ok();
                            }
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            out.send(delta("审批处理好了。")).ok();
                            out.send(done("completed")).ok();
                        }
                        "long" => {
                            for i in 0..500 {
                                if ints.lock().unwrap().contains(&turn) {
                                    tokio::time::sleep(Duration::from_millis(15)).await;
                                    out.send(done("interrupted")).ok();
                                    return;
                                }
                                out.send(delta(&format!("第{i}句。"))).ok();
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            out.send(done("completed")).ok();
                        }
                        "slow" => {
                            for _ in 0..300 {
                                if ints.lock().unwrap().contains(&turn) { out.send(done("interrupted")).ok(); return; }
                                tokio::time::sleep(Duration::from_millis(10)).await;
                            }
                            out.send(delta("太慢了。")).ok();
                            out.send(done("completed")).ok();
                        }
                        "crash" => {
                            tokio::time::sleep(Duration::from_millis(30)).await;
                            crash.cancel(); // child "dies": both pipes close
                        }
                        "fail" => {
                            out.send(note("error", json!({ "error": { "message": "usage limit reached" }, "willRetry": false }))).ok();
                            out.send(note("turn/completed", json!({ "turn": { "id": turn, "status": "failed", "items": [], "error": { "message": "usage limit reached" } } }))).ok();
                        }
                        _ => {
                            // priming turn ("你好") and anything else: quick answer.
                            out.send(delta("嗨。")).ok();
                            out.send(done("completed")).ok();
                        }
                    }
                });
            }
            other => {
                out_tx.send(json!({ "id": id, "error": { "code": -32601, "message": format!("fake: {other}") } })).ok();
            }
        }
    }
}

// ---- tiny HTTP SSE mock for the fallback engine ----
async fn spawn_http(delay_ms: u64, parts: &'static [&'static str]) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let h = h.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break p + 4; }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
                let len: usize = head.lines().find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))).unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                }
                h.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n").await;
                for p in parts {
                    let _ = sock.write_all(format!("data: {}\n\n", json!({"choices":[{"delta":{"content":p}}]})).as_bytes()).await;
                }
                let _ = sock.write_all(b"data: [DONE]\n\n").await;
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), hits)
}

fn http_route(base: &str) -> Route {
    Route { id: "http-a".into(), model: "mock".into(), base_url: base.into(), api_key_env: String::new(), extra_body: Value::Null, direct: false }
}

fn core(mode: EngineMode, opts: FakeOpts, http: Option<&str>, fallback_ms: u64) -> (Core, Arc<FakeLog>) {
    core_with(mode, opts, http, fallback_ms, |_| {})
}

fn core_with(mode: EngineMode, opts: FakeOpts, http: Option<&str>, fallback_ms: u64, f: impl FnOnce(&mut CodexConfig)) -> (Core, Arc<FakeLog>) {
    let log = Arc::new(FakeLog::default());
    let routes: Vec<Route> = http.map(|b| vec![http_route(b)]).unwrap_or_default();
    let order = routes.iter().map(|r| r.id.clone()).collect();
    let cfg = Config {
        routes,
        order,
        mode,
        workspace_root: ROOT.into(),
        codex: CodexConfig { fallback_after_ms: fallback_ms, respawn_backoff_ms: 60_000, ..CodexConfig::default() },
        health: quiet(), ..Config::default()
    };
    let mut cfg = cfg;
    f(&mut cfg.codex);
    (Core::with_codex_connector(cfg, fake_connector(opts, log.clone())).unwrap(), log)
}

const OK: FakeOpts = FakeOpts { logged_in: true, has_luna: true };

async fn collect(core: &Core, text: &str) -> Vec<Event> {
    let mut turn = core.respond(text, CancellationToken::new());
    let mut evs = Vec::new();
    while let Some(e) = turn.next().await {
        let end = e.is_terminal();
        evs.push(e);
        if end { break; }
    }
    evs
}

fn sentences(evs: &[Event]) -> Vec<String> {
    evs.iter().filter_map(|e| match e { Event::Sentence { text, .. } => Some(text.clone()), _ => None }).collect()
}

#[tokio::test]
async fn warmup_reports_model_effort_and_primes_a_thread() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    let w = c.warmup_codex().await;
    assert_eq!(w.error, None);
    assert_eq!(w.model.as_deref(), Some("gpt-6-luna"));
    assert_eq!(w.effort.as_deref(), Some("low"), "lowest supported effort");
    assert_eq!(w.account, "chatgpt pro", "no email in the report");
    assert!(w.prime_turn_ms.is_some());
    let m = log.methods.lock().unwrap().clone();
    assert_eq!(&m[..5], &["initialize", "initialized", "account/read", "model/list", "thread/start"]);
    assert_eq!(m[5], "turn/start");
}

#[tokio::test]
async fn codex_streams_sentences_on_the_warm_thread() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let evs = collect(&c, "stream").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "你好！我是Codex。");
    assert_eq!(sentences(&evs), vec!["你好！", "我是Codex。"]);
    assert_eq!(timings.engine.as_deref(), Some("codex"));
    assert_eq!(timings.route.as_deref(), Some("codex/gpt-6-luna"));
    assert!(timings.reasoning_chars > 0);
    let inputs = log.turn_inputs.lock().unwrap().clone();
    let last = inputs.last().unwrap();
    assert_eq!(last["threadId"], "th-1", "reuses the primed thread");
    assert_eq!(last["effort"], "low");
    assert_eq!(last["sandboxPolicy"]["type"], "workspaceWrite");
    assert_eq!(last["sandboxPolicy"]["writableRoots"][0], ROOT);
    assert_eq!(last["sandboxPolicy"]["networkAccess"], true);
    assert_eq!(log.methods.lock().unwrap().iter().filter(|m| *m == "thread/start").count(), 1);
}

#[tokio::test]
async fn completed_agent_item_recovers_only_the_missing_text() {
    let (c, _) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    for request in ["final-only", "missing-tail"] {
        let evs = collect(&c, request).await;
        let Some(Event::Done { text, .. }) = evs.last() else { panic!("{evs:?}") };
        assert_eq!(text, "你好！完整的结尾。", "{request}");
        assert_eq!(sentences(&evs), vec!["你好！", "完整的结尾。"]);
    }
}

#[tokio::test]
async fn old_nested_turn_completion_cannot_finish_the_current_question() {
    let (c, _) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let evs = collect(&c, "stale-terminal").await;
    assert!(matches!(evs.last(), Some(Event::Done { text, .. }) if text == "这是新问题的完整回答。"), "{evs:?}");
}

#[tokio::test]
async fn codex_interrupt_stops_output_and_sends_turn_interrupt() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let cancel = CancellationToken::new();
    let mut turn = c.respond("long", cancel.clone());
    let mut at = None;
    let mut after = 0;
    while let Some(e) = turn.next().await {
        match e {
            Event::Sentence { index, .. } => {
                if at.is_some() { after += 1; }
                if index == 3 && at.is_none() { cancel.cancel(); at = Some(Instant::now()); }
            }
            Event::Cancelled { .. } => {
                assert!(at.unwrap().elapsed() < Duration::from_millis(50), "{:?}", at.unwrap().elapsed());
                break;
            }
            e if e.is_terminal() => panic!("{e:?}"),
            _ => {}
        }
    }
    assert!(after <= 1);
    let s = c.codex().wait_interrupt_completed(Duration::from_secs(2)).await.expect("stats");
    assert_eq!(s.final_status.as_deref(), Some("interrupted"));
    assert!(s.cancel_to_completed_ms.unwrap() < 500.0);
    assert!(log.methods.lock().unwrap().contains(&"turn/interrupt".to_string()));
    // The thread is free again and the next turn works.
    let evs = collect(&c, "stream").await;
    assert!(matches!(evs.last(), Some(Event::Done { .. })), "{evs:?}");
}

#[tokio::test]
async fn approvals_accept_inside_workspace_and_decline_outside() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    let evs = collect(&c, "approval").await;
    assert!(matches!(evs.last(), Some(Event::Done { text, .. }) if text == "审批处理好了。"), "{evs:?}");
    let replies: std::collections::HashMap<String, Value> = log.replies.lock().unwrap().iter().cloned().collect();
    assert_eq!(replies["cmd-in"]["result"]["decision"], "accept");
    assert_eq!(replies["cmd-out"]["result"]["decision"], "decline");
    assert_eq!(replies["cmd-cwd-out"]["result"]["decision"], "decline");
    assert_eq!(replies["fc-in"]["result"]["decision"], "accept");
    assert_eq!(replies["fc-out"]["result"]["decision"], "decline");
    assert!(replies["time"]["result"]["currentTimeAt"].is_u64());
    assert_eq!(replies["weird"]["error"]["code"], -32601, "unknown requests get an error, never silence");
    assert!(c.codex().audit().len() >= 7);
}

#[tokio::test]
async fn child_crash_falls_back_to_http_and_marks_codex_unhealthy() {
    let (base, hits) = spawn_http(50, &["备用回答。"]).await;
    let (c, log) = core(EngineMode::Dual, OK, Some(&base), 5000);
    c.warmup_codex().await;
    let t = Instant::now();
    let evs = collect(&c, "crash").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "备用回答。");
    assert_eq!(timings.engine.as_deref(), Some("http"));
    assert_eq!(timings.routes_started, vec!["codex", "http-a"]);
    assert!(timings.errors.iter().any(|e| e.contains("exited")), "{:?}", timings.errors);
    assert!(t.elapsed() < Duration::from_millis(2000), "fallback must not wait for fallback_after_ms");
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    // Next turn: codex is unhealthy (within backoff) -> immediate fallback, no respawn.
    let evs = collect(&c, "stream").await;
    let Some(Event::Done { timings, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(timings.engine.as_deref(), Some("http"));
    assert!(timings.errors.iter().any(|e| e.contains("unhealthy")), "{:?}", timings.errors);
    assert_eq!(log.spawns.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn codex_turn_failure_falls_back_immediately() {
    let (base, _) = spawn_http(0, &["好的。"]).await;
    let (c, _) = core(EngineMode::Dual, OK, Some(&base), 5000);
    c.warmup_codex().await;
    let evs = collect(&c, "fail").await;
    let Some(Event::Done { timings, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(timings.engine.as_deref(), Some("http"));
    assert!(timings.errors[0].contains("usage limit reached"), "{:?}", timings.errors);
}

#[tokio::test]
async fn not_logged_in_is_unhealthy_and_dual_falls_back_at_once() {
    let (base, _) = spawn_http(0, &["没登录也能答。"]).await;
    let (c, _) = core(EngineMode::Dual, FakeOpts { logged_in: false, has_luna: true }, Some(&base), 5000);
    let w = c.warmup_codex().await;
    assert!(w.error.as_deref().unwrap().contains("not logged in"));
    let t = Instant::now();
    let evs = collect(&c, "stream").await;
    assert!(matches!(evs.last(), Some(Event::Done { timings, .. }) if timings.engine.as_deref() == Some("http")), "{evs:?}");
    assert!(t.elapsed() < Duration::from_millis(1000));
}

#[tokio::test]
async fn missing_model_is_reported_not_substituted() {
    let (c, log) = core(EngineMode::CodexOnly, FakeOpts { logged_in: true, has_luna: false }, None, 1000);
    let evs = collect(&c, "stream").await;
    let Some(Event::Error { message, .. }) = evs.last() else { panic!("{evs:?}") };
    assert!(message.contains("'gpt-6-luna' not found") && message.contains("gpt-6-astra"), "{message}");
    assert!(!log.methods.lock().unwrap().contains(&"thread/start".to_string()));
}

#[tokio::test]
async fn dual_slow_codex_lets_http_win_and_interrupts_codex() {
    let (base, _) = spawn_http(0, &["HTTP 先到。"]).await;
    let (c, log) = core(EngineMode::Dual, OK, Some(&base), 200);
    c.warmup_codex().await;
    let evs = collect(&c, "slow").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "HTTP 先到。");
    assert_eq!(timings.routes_started, vec!["codex", "http-a"]);
    let ft = timings.first_token_ms.unwrap();
    assert!((190..1500).contains(&ft), "{ft}");
    let t = Instant::now();
    while !log.methods.lock().unwrap().contains(&"turn/interrupt".to_string()) && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(log.methods.lock().unwrap().contains(&"turn/interrupt".to_string()), "codex loser must be interrupted");
}

#[tokio::test]
async fn dual_fast_codex_wins_and_http_never_starts() {
    let (base, hits) = spawn_http(0, &["不该出现。"]).await;
    let (c, _) = core(EngineMode::Dual, OK, Some(&base), 1500);
    c.warmup_codex().await;
    let evs = collect(&c, "stream").await;
    let Some(Event::Done { timings, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(timings.engine.as_deref(), Some("codex"));
    assert_eq!(timings.routes_started, vec!["codex"]);
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

// ---- one thread, steer and barge-in ----

async fn until_sentence(turn: &mut Turn) {
    loop {
        match turn.next().await {
            Some(Event::Sentence { .. }) => return,
            Some(e) if e.is_terminal() => panic!("ended early: {e:?}"),
            Some(_) => {}
            None => panic!("closed"),
        }
    }
}

async fn finish(turn: &mut Turn) -> Event {
    loop {
        match turn.next().await {
            Some(e) if e.is_terminal() => return e,
            Some(_) => {}
            None => panic!("closed without terminal event"),
        }
    }
}

fn count(log: &FakeLog, m: &str) -> usize {
    log.methods.lock().unwrap().iter().filter(|x| *x == m).count()
}

#[tokio::test]
async fn one_thread_keeps_context_across_turns_and_interrupts() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let evs = collect(&c, "我叫小王。").await;
    assert!(matches!(evs.last(), Some(Event::Done { .. })), "{evs:?}");
    // Interrupt a long turn, then ask: same thread, context intact.
    let cancel = CancellationToken::new();
    let mut t = c.respond("long", cancel.clone());
    until_sentence(&mut t).await;
    cancel.cancel();
    assert!(matches!(finish(&mut t).await, Event::Cancelled { .. }));
    drop(t);
    let evs = collect(&c, "我叫什么？").await;
    let Some(Event::Done { text, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "你叫小王。", "context must survive an interrupt (same thread)");
    let inputs = log.turn_inputs.lock().unwrap().clone();
    assert!(inputs.iter().all(|p| p["threadId"] == "th-1"), "every turn on th-1: {:?}", inputs.iter().map(|p| p["threadId"].clone()).collect::<Vec<_>>());
    assert_eq!(count(&log, "thread/start"), 1, "no spare / replacement threads");
    assert_eq!(c.codex().threads_started(), 1);
    assert_eq!(c.codex().thread_id().as_deref(), Some("th-1"));
    assert_eq!(count(&log, "turn/steer"), 0, "finished turns are never steered");
}

#[tokio::test]
async fn next_turn_waits_for_the_interrupted_turn_to_complete() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let cancel = CancellationToken::new();
    let mut t = c.respond("long", cancel.clone());
    until_sentence(&mut t).await;
    cancel.cancel();
    drop(t);
    // Ask immediately; the fake takes ~15 ms to confirm the interrupt.
    let evs = collect(&c, "stream").await;
    assert!(matches!(evs.last(), Some(Event::Done { .. })), "{evs:?}");
    let order = log.order.lock().unwrap().clone();
    let done_at = order.iter().position(|e| e.ends_with(":interrupted")).expect("interrupted turn completed");
    let next_start = order.iter().rposition(|e| e.starts_with("start:")).unwrap();
    assert!(next_start > done_at, "turn/start must wait for the interrupted turn: {order:?}");
    assert_eq!(count(&log, "thread/start"), 1);
}

#[tokio::test]
async fn fresh_question_cannot_steer_a_previous_speakers_turn() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let mut previous = c.respond("steerable", CancellationToken::new());
    until_sentence(&mut previous).await;
    let mut fresh = c.respond_fresh(vec![fast_responder::Message::user("新的问题")], CancellationToken::new());
    assert!(matches!(finish(&mut previous).await, Event::Cancelled { .. }));
    assert!(matches!(finish(&mut fresh).await, Event::Done { .. }));
    assert_eq!(count(&log, "turn/steer"), 0);
    assert_eq!(count(&log, "turn/interrupt"), 1);
}

#[tokio::test]
async fn followup_steers_the_running_turn_and_takes_over_its_output() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let mut t1 = c.respond("steerable", CancellationToken::new());
    until_sentence(&mut t1).await;
    let mut t2 = c.respond("改成讲狗", CancellationToken::new());
    // The old Turn ends as superseded, without an error.
    let Event::Cancelled { timings, .. } = finish(&mut t1).await else { panic!("t1 should end Cancelled") };
    assert!(timings.superseded);
    let Event::Done { text, timings } = finish(&mut t2).await else { panic!("t2 should finish") };
    assert_eq!(text, "收到：改成讲狗。", "recover the same item's new final tail without replaying an old snapshot");
    assert_eq!(timings.engine.as_deref(), Some("codex"));
    let sp = log.steer_params.lock().unwrap().clone();
    assert_eq!(sp.len(), 1);
    assert_eq!(sp[0]["threadId"], "th-1");
    let turn_ids: Vec<String> = log.turn_inputs.lock().unwrap().iter().map(|p| p["threadId"].to_string()).collect();
    let order = log.order.lock().unwrap().clone();
    let steered = order.iter().rev().find(|e| e.starts_with("start:")).unwrap().trim_start_matches("start:").to_owned();
    assert_eq!(sp[0]["expectedTurnId"], steered.as_str(), "expectedTurnId = the running turn");
    assert_eq!(count(&log, "turn/interrupt"), 0, "steer must not interrupt");
    assert_eq!(turn_ids.len(), 2, "prime + one user turn; the follow-up started no new turn");
    let s = c.codex().last_steer().expect("steer stats");
    assert!(s.ok && s.ack_ms < 500.0 && s.first_token_ms.is_some(), "{s:?}");
}

#[tokio::test]
async fn steered_final_only_item_does_not_replay_the_previous_item() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let mut previous = c.respond("steerable-final-only", CancellationToken::new());
    until_sentence(&mut previous).await;
    let mut next = c.respond("讲狗", CancellationToken::new());
    assert!(matches!(finish(&mut previous).await, Event::Cancelled { .. }));
    let Event::Done { text, .. } = finish(&mut next).await else { panic!("steered final-only answer should finish") };
    assert_eq!(text, "收到：讲狗。");
    assert_eq!(count(&log, "turn/steer"), 1);
    assert_eq!(count(&log, "turn/interrupt"), 0);
}

#[tokio::test]
async fn steer_survives_the_caller_dropping_the_previous_turn() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let mut turn = c.respond("steerable", CancellationToken::new());
    until_sentence(&mut turn).await;
    // Typical caller code: replace the Turn (drops and cancels the old one).
    turn = c.respond("讲短一点", CancellationToken::new());
    let Event::Done { text, .. } = finish(&mut turn).await else { panic!() };
    assert_eq!(text, "收到：讲短一点。");
    assert_eq!(count(&log, "turn/interrupt"), 0);
    assert_eq!(count(&log, "turn/steer"), 1);
}

#[tokio::test]
async fn interrupt_policy_stops_the_turn_and_starts_a_new_one_on_the_same_thread() {
    let (c, log) = core_with(EngineMode::CodexOnly, OK, None, 1000, |k| k.barge_in = BargeIn::Interrupt);
    c.warmup_codex().await;
    let mut t1 = c.respond("long", CancellationToken::new());
    until_sentence(&mut t1).await;
    let mut t2 = c.respond("stream", CancellationToken::new());
    let Event::Cancelled { timings, .. } = finish(&mut t1).await else { panic!() };
    assert!(timings.superseded);
    let Event::Done { text, .. } = finish(&mut t2).await else { panic!() };
    assert_eq!(text, "你好！我是Codex。");
    assert_eq!(count(&log, "turn/steer"), 0);
    assert_eq!(count(&log, "turn/interrupt"), 1);
    let order = log.order.lock().unwrap().clone();
    let done_at = order.iter().position(|e| e.ends_with(":interrupted")).unwrap();
    assert!(order.iter().rposition(|e| e.starts_with("start:")).unwrap() > done_at, "{order:?}");
    assert!(log.turn_inputs.lock().unwrap().iter().all(|p| p["threadId"] == "th-1"));
}

#[tokio::test]
async fn steer_rejected_falls_back_to_a_new_turn() {
    // The fake "stream" turn ignores steers: whether the steer is rejected (turn already over) or
    // accepted but never consumed, the follow-up is answered by a new turn on the same thread.
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let mut t1 = c.respond("stream", CancellationToken::new());
    let mut t2 = c.respond("stream", CancellationToken::new());
    let _ = finish(&mut t1).await;
    let Event::Done { text, .. } = finish(&mut t2).await else { panic!("t2 must still be answered") };
    assert_eq!(text, "你好！我是Codex。");
    assert!(c.codex().wait_idle(Duration::from_secs(2)).await);
    assert_eq!(count(&log, "thread/start"), 1);
}

/// Tests never write the real latency log.
fn quiet() -> fast_responder::HealthConfig {
    fast_responder::HealthConfig { log_path: None, ..Default::default() }
}

// ---- Luna-first policy: fallback + switch-back, and Grok Bot delegation config ----

fn named_route(id: &str, base: &str) -> Route {
    Route { id: id.into(), model: "mock".into(), base_url: base.into(), api_key_env: String::new(), extra_body: Value::Null, direct: false }
}

fn policy_core(routes: Vec<Route>, fallback_ms: u64, race_at_start: bool) -> (Core, Arc<FakeLog>) {
    let log = Arc::new(FakeLog::default());
    let order = routes.iter().map(|r| r.id.clone()).collect();
    let health = fast_responder::HealthConfig { log_path: None, probe_interval_ms: 50, probe_timeout_ms: 2000, ..Default::default() };
    let cfg = Config {
        routes,
        order,
        mode: EngineMode::Dual,
        http_race_at_start: race_at_start,
        first_token_timeout_ms: 2000,
        workspace_root: ROOT.into(),
        codex: CodexConfig { fallback_after_ms: fallback_ms, respawn_backoff_ms: 60_000, ..CodexConfig::default() },
        health,
        ..Config::default()
    };
    (Core::with_codex_connector(cfg, fake_connector(OK, log.clone())).unwrap(), log)
}

fn done_timings(evs: &[Event]) -> fast_responder::Timings {
    match evs.last() {
        Some(Event::Done { timings, .. }) => timings.clone(),
        other => panic!("{other:?}"),
    }
}

async fn wait_for(mut f: impl FnMut() -> bool, ms: u64) -> bool {
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(ms) {
        if f() { return true; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f()
}

#[tokio::test]
async fn healthy_luna_keeps_the_existing_dual_plan_and_logs_latency() {
    let (base, hits) = spawn_http(0, &["HTTP答。"]).await;
    let (c, _log) = policy_core(vec![named_route("http-a", &base)], 1000, true);
    c.warmup_codex().await;
    for _ in 0..4 {
        let t = done_timings(&collect(&c, "stream").await);
        assert_eq!(t.engine.as_deref(), Some("codex"));
        assert_eq!(t.routes_started, vec!["codex"], "HTTP still waits for fallback_after_ms");
        assert_eq!(t.luna.as_deref(), Some("preferred"));
        assert_eq!(t.attempts.len(), 1);
        assert_eq!(t.attempts[0].outcome, Some(fast_responder::SampleOutcome::Ok));
        assert!(t.attempts[0].ttft_ms.is_some());
        let luna = t.latency.iter().find(|e| e.key == "codex").expect("rolling stats exposed in Timings");
        assert!(luna.median_ttft_ms.is_some() && luna.failures == 0);
    }
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let h = c.health();
    assert_eq!(h.luna, fast_responder::LunaState::Preferred);
    let luna = h.engines.iter().find(|e| e.key == "codex").expect("luna stats");
    assert_eq!((luna.samples, luna.ok, luna.failures), (4, 4, 0));
    assert!(luna.median_ttft_ms.is_some());
}

#[tokio::test]
async fn luna_outage_switches_to_http_and_probe_switches_back() {
    let (base, _hits) = spawn_http(0, &["HTTP答。"]).await;
    let (c, log) = policy_core(vec![named_route("http-a", &base)], 1000, true);
    c.warmup_codex().await;
    log.luna.store(1, Ordering::SeqCst); // Luna starts failing
    for _ in 0..2 {
        let t = done_timings(&collect(&c, "stream").await);
        assert_eq!(t.engine.as_deref(), Some("http"), "immediate failover on a Luna error");
        assert_eq!(t.luna.as_deref(), Some("preferred"));
        assert_eq!(t.attempts[0].outcome, Some(fast_responder::SampleOutcome::Failed));
    }
    assert!(matches!(c.luna_state(), fast_responder::LunaState::Degraded { ref reason, .. } if reason.starts_with("unavailable")), "{:?}", c.luna_state());
    let turns_before = log.turn_inputs.lock().unwrap().iter().filter(|p| p["threadId"] == "th-1").count();
    let t = done_timings(&collect(&c, "stream").await);
    assert_eq!(t.routes_started, vec!["http-a"], "degraded: HTTP leads, Luna is not started");
    assert!(t.luna.as_deref().unwrap().starts_with("degraded"));
    assert!(t.first_token_ms.unwrap() < 500, "no fallback wait: {:?}", t.first_token_ms);
    // Probes run on their own thread while Luna is still failing; nothing reaches the conversation.
    assert!(wait_for(|| log.thread_params.lock().unwrap().len() >= 2, 2000).await, "probe thread created");
    assert_eq!(log.turn_inputs.lock().unwrap().iter().filter(|p| p["threadId"] == "th-1").count(), turns_before);
    assert!(c.luna_state() != fast_responder::LunaState::Preferred);
    // Luna recovers: two fast probes switch back.
    log.luna.store(0, Ordering::SeqCst);
    assert!(wait_for(|| c.luna_state() == fast_responder::LunaState::Preferred, 5000).await, "switched back: {:?}", c.health());
    let probe_inputs: Vec<Value> = log.turn_inputs.lock().unwrap().iter().filter(|p| p["threadId"] != "th-1").cloned().collect();
    assert!(probe_inputs.iter().any(|p| p["input"][0]["text"] == fast_responder::codex::PROBE_TEXT));
    let h = c.health();
    assert_eq!(h.switches.len(), 2, "{:?}", h.switches);
    assert_eq!(h.switches[1].to, "luna");
    let t = done_timings(&collect(&c, "stream").await);
    assert_eq!(t.engine.as_deref(), Some("codex"), "Luna leads again");
    assert_eq!(t.routes_started[0], "codex");
    assert_eq!(t.luna.as_deref(), Some("preferred"));
}

#[tokio::test]
async fn slow_luna_degrades_and_the_fastest_route_leads() {
    let (slow_base, _) = spawn_http(300, &["慢路线。"]).await;
    let (fast_base, _) = spawn_http(0, &["快路线。"]).await;
    // Configured order puts the slow route first.
    let (c, log) = policy_core(vec![named_route("slow-r", &slow_base), named_route("fast-r", &fast_base)], 100, true);
    c.warmup_codex().await;
    log.luna.store(2, Ordering::SeqCst); // Luna answers only after 3 s
    for _ in 0..3 {
        let t = done_timings(&collect(&c, "stream").await);
        assert_eq!(t.route.as_deref(), Some("fast-r"));
        let luna = t.attempts.iter().find(|a| a.route.starts_with("codex")).unwrap();
        assert_eq!(luna.outcome, Some(fast_responder::SampleOutcome::Lost));
        c.codex().wait_idle(Duration::from_secs(2)).await;
    }
    assert!(matches!(c.luna_state(), fast_responder::LunaState::Degraded { ref reason, .. } if reason.starts_with("slow")), "{:?}", c.luna_state());
    let t = done_timings(&collect(&c, "stream").await);
    assert_eq!(t.routes_started, vec!["fast-r", "slow-r"], "fastest recent route first, all at t=0, no Luna");
    assert_eq!(t.route.as_deref(), Some("fast-r"));
    assert!(t.first_token_ms.unwrap() < 100, "{:?}", t.first_token_ms);
}

#[tokio::test]
async fn degraded_luna_is_the_last_resort_when_every_http_route_fails() {
    let log = Arc::new(FakeLog::default());
    let cfg = Config {
        // Nothing listens on port 9: every HTTP attempt fails at connect.
        routes: vec![named_route("dead", "http://127.0.0.1:9/v1")],
        order: vec!["dead".into()],
        mode: EngineMode::Dual,
        workspace_root: ROOT.into(),
        codex: CodexConfig { fallback_after_ms: 1000, respawn_backoff_ms: 60_000, ..CodexConfig::default() },
        // No probe during the test: only the failure-only stage can bring Luna in.
        health: fast_responder::HealthConfig { log_path: None, probe_interval_ms: 600_000, ..Default::default() },
        ..Config::default()
    };
    let c = Core::with_codex_connector(cfg, fake_connector(OK, log.clone())).unwrap();
    c.warmup_codex().await;
    log.luna.store(1, Ordering::SeqCst);
    for _ in 0..2 {
        let evs = collect(&c, "stream").await;
        assert!(matches!(evs.last(), Some(Event::Error { .. })), "Luna and HTTP both fail: {evs:?}");
    }
    assert!(matches!(c.luna_state(), fast_responder::LunaState::Degraded { .. }));
    log.luna.store(0, Ordering::SeqCst); // Luna works again, but the policy has not noticed yet
    let t = done_timings(&collect(&c, "stream").await);
    assert!(t.luna.as_deref().unwrap().starts_with("degraded"));
    assert_eq!(t.routes_started, vec!["dead", "codex"], "HTTP leads; Luna only after every HTTP route failed");
    assert_eq!(t.engine.as_deref(), Some("codex"));
    let dead = t.attempts.iter().find(|a| a.route == "dead").unwrap();
    assert_eq!(dead.outcome, Some(fast_responder::SampleOutcome::Failed));
    assert!(c.luna_state() != fast_responder::LunaState::Preferred, "only probes switch back");
}

#[tokio::test]
async fn degraded_luna_does_not_start_while_an_http_route_is_still_running() {
    let (slow_base, _) = spawn_http(400, &["慢但成功。"]).await;
    let log = Arc::new(FakeLog::default());
    let cfg = Config {
        routes: vec![named_route("dead", "http://127.0.0.1:9/v1"), named_route("slow-ok", &slow_base)],
        order: vec!["dead".into(), "slow-ok".into()],
        mode: EngineMode::Dual,
        workspace_root: ROOT.into(),
        codex: CodexConfig { fallback_after_ms: 1000, respawn_backoff_ms: 60_000, ..CodexConfig::default() },
        health: fast_responder::HealthConfig { log_path: None, probe_interval_ms: 600_000, ..Default::default() },
        ..Config::default()
    };
    let c = Core::with_codex_connector(cfg, fake_connector(OK, log.clone())).unwrap();
    c.warmup_codex().await;
    log.luna.store(1, Ordering::SeqCst);
    for _ in 0..2 {
        let _ = collect(&c, "stream").await;
    }
    assert!(matches!(c.luna_state(), fast_responder::LunaState::Degraded { .. }));
    let t = done_timings(&collect(&c, "stream").await);
    assert_eq!(t.route.as_deref(), Some("slow-ok"));
    assert!(!t.routes_started.iter().any(|r| r == "codex"), "one HTTP failure is not enough: {:?}", t.routes_started);
}

#[tokio::test]
async fn disabled_policy_never_leaves_luna() {
    let (base, _) = spawn_http(0, &["HTTP答。"]).await;
    let log = Arc::new(FakeLog::default());
    let cfg = Config {
        routes: vec![named_route("http-a", &base)],
        order: vec!["http-a".into()],
        mode: EngineMode::Dual,
        workspace_root: ROOT.into(),
        codex: CodexConfig { fallback_after_ms: 1000, respawn_backoff_ms: 60_000, ..CodexConfig::default() },
        health: fast_responder::HealthConfig { enabled: false, log_path: None, ..Default::default() },
        ..Config::default()
    };
    let c = Core::with_codex_connector(cfg, fake_connector(OK, log.clone())).unwrap();
    c.warmup_codex().await;
    log.luna.store(1, Ordering::SeqCst);
    for _ in 0..4 {
        let t = done_timings(&collect(&c, "stream").await);
        assert_eq!(t.routes_started[0], "codex");
    }
    assert_eq!(c.luna_state(), fast_responder::LunaState::Preferred);
}

#[test]
fn grok_bot_delegate_builds_codex_overrides() {
    use fast_responder::GrokBotDelegate;
    let d = GrokBotDelegate { enabled: true, node: Some("/usr/local/bin/node".into()), mcp_server: "/x/mcp/server.mjs".into(), cli: "/x/cli/grokbot".into(), ..GrokBotDelegate::default() };
    let o = d.codex_overrides();
    assert!(o.chunks(2).all(|p| p[0] == "-c"), "{o:?}");
    let vals: Vec<&str> = o.chunks(2).map(|p| p[1].as_str()).collect();
    assert!(vals.contains(&r#"mcp_servers.xiaodai_grok_bot.command="/usr/local/bin/node""#), "{vals:?}");
    assert!(vals.contains(&r#"mcp_servers.xiaodai_grok_bot.args=["/x/mcp/server.mjs"]"#));
    assert!(vals.contains(&"mcp_servers.xiaodai_grok_bot.enabled=true"));
    assert!(vals.contains(&"mcp_servers.xiaodai_grok_bot.tool_timeout_sec=330"));
    for tool in ["ask_grok_bot", "get_grok_bot_result", "ask_grok_bot_and_wait"] {
        assert!(vals.contains(&format!(r#"mcp_servers.xiaodai_grok_bot.tools.{tool}.approval_mode="approve""#).as_str()));
    }
    let text = d.instructions_text();
    assert!(text.contains("ask_grok_bot") && text.contains("/x/cli/grokbot --no-wait"), "{text}");
    // Only the core's child gets them, after the lean set.
    let cfg = CodexConfig { lean: false, grok_bot: d.clone(), ..CodexConfig::default() };
    assert_eq!(fast_responder::codex::child_overrides(&cfg), o);
    let off = GrokBotDelegate { enabled: false, ..d };
    assert!(off.codex_overrides().is_empty());
    assert!(off.instructions_text().is_empty());
}

#[tokio::test]
async fn delegation_note_reaches_thread_instructions_only_when_enabled() {
    use fast_responder::GrokBotDelegate;
    for enabled in [true, false] {
        let (c, log) = core_with(EngineMode::CodexOnly, OK, None, 1000, |cc| {
            cc.grok_bot = GrokBotDelegate { enabled, cli: "/x/cli/grokbot".into(), ..GrokBotDelegate::default() };
        });
        c.warmup_codex().await;
        let p = log.thread_params.lock().unwrap()[0].clone();
        let di = p["developerInstructions"].as_str().unwrap();
        assert!(di.starts_with(fast_responder::config::DEFAULT_CODEX_INSTRUCTIONS));
        assert_eq!(di.contains("ask_grok_bot"), enabled, "{di}");
        assert_eq!(di.contains("/x/cli/grokbot"), enabled);
    }
}

#[tokio::test]
async fn caller_context_messages_prefix_the_codex_turn_text() {
    use fast_responder::Message;
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let msgs = vec![Message { role: "system".into(), content: "家庭记忆：小王喜欢猫。".into() }, Message::user("stream")];
    let mut turn = c.respond_messages(msgs, CancellationToken::new());
    while let Some(e) = turn.next().await { if e.is_terminal() { break; } }
    let inputs = log.turn_inputs.lock().unwrap().clone();
    let text = inputs.last().unwrap()["input"][0]["text"].as_str().unwrap().to_owned();
    assert!(text.starts_with("家庭记忆：小王喜欢猫。"), "{text}");
    assert!(text.ends_with("stream"), "{text}");
    // Without context messages the text is unchanged.
    collect(&c, "stream").await;
    let inputs = log.turn_inputs.lock().unwrap().clone();
    assert_eq!(inputs.last().unwrap()["input"][0]["text"], "stream");
}

#[tokio::test]
async fn reset_thread_starts_a_new_conversation_thread() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    collect(&c, "stream").await;
    let first = c.codex().thread_id().unwrap();
    c.codex().reset_thread(false).await;
    assert_eq!(c.codex().thread_id(), None);
    collect(&c, "stream").await;
    let second = c.codex().thread_id().unwrap();
    assert_ne!(first, second);
    assert_eq!(log.methods.lock().unwrap().iter().filter(|m| *m == "thread/start").count(), 2);
    // Primed reset prepares the next thread in the background.
    c.codex().reset_thread(true).await;
    assert!(wait_for(|| c.codex().thread_id().is_some_and(|t| t != second), 3000).await);
}

#[tokio::test]
async fn shutdown_stops_codex_until_reopen() {
    let (a, _) = spawn_http(0, &["HTTP在。"]).await;
    let (c, log) = core(EngineMode::Dual, OK, Some(&a), 1000);
    c.warmup_codex().await;
    assert_eq!(log.spawns.load(Ordering::SeqCst), 1);
    c.shutdown().await;
    let evs = collect(&c, "stream").await;
    let Some(Event::Done { timings, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(timings.engine.as_deref(), Some("http"), "HTTP answers while Codex is shut down");
    assert_eq!(log.spawns.load(Ordering::SeqCst), 1, "no respawn while shut down");
    c.reopen();
    c.warmup_codex().await;
    assert_eq!(log.spawns.load(Ordering::SeqCst), 2);
    let evs = collect(&c, "stream").await;
    let Some(Event::Done { timings, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(timings.engine.as_deref(), Some("codex"));
}

async fn collect_messages(core: &Core, messages: Vec<fast_responder::Message>) -> Vec<Event> {
    let mut turn = core.respond_fresh(messages, CancellationToken::new());
    let mut evs = Vec::new();
    while let Some(e) = turn.next().await {
        let end = e.is_terminal();
        evs.push(e);
        if end { break; }
    }
    evs
}

#[tokio::test]
async fn a_known_thread_gets_the_delta_and_a_new_thread_the_full_context() {
    let (c, log) = core(EngineMode::CodexOnly, OK, None, 1000);
    c.warmup_codex().await;
    let ask = |q: &str| vec![
        fast_responder::Message { role: "system".into(), content: "CTX-FULL".into() },
        fast_responder::Message { role: "delta".into(), content: "CTX-DELTA".into() },
        fast_responder::Message::user(q),
    ];
    let text = |i: usize| log.turn_inputs.lock().unwrap()[i]["input"][0]["text"].as_str().unwrap_or("").to_owned();
    let base = log.turn_inputs.lock().unwrap().len();
    collect_messages(&c, ask("一")).await;
    assert!(text(base).starts_with("CTX-FULL") && text(base).ends_with("一"), "primed thread is new: {}", text(base));
    assert!(!c.codex().input_stats().last_delta);
    collect_messages(&c, ask("二")).await;
    assert!(text(base + 1).starts_with("CTX-DELTA") && !text(base + 1).contains("CTX-FULL"), "{}", text(base + 1));
    assert!(c.codex().input_stats().last_delta);
    c.codex().reset_thread(false).await;
    collect_messages(&c, ask("三")).await;
    assert!(text(base + 2).starts_with("CTX-FULL"), "a replacement thread needs everything: {}", text(base + 2));
    assert_eq!(c.codex().input_stats().submitted, 3);
}

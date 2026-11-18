//! Race / failover / cancellation against a local mock SSE server (tokio TcpListener, HTTP/1.1).
use fast_responder::{CancellationToken, Config, Core, EngineMode, Event, Route};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
struct Mock {
    status: u16,
    delay_ms: u64,
    chunks: Vec<String>,
    chunk_delay_ms: u64,
}

#[derive(Default)]
struct Stats {
    requests: AtomicUsize,
    client_gone: AtomicBool,
}

fn content(s: &str) -> String {
    format!("data: {}\n\n", serde_json::json!({"choices":[{"delta":{"content":s}}]}))
}
fn reasoning(s: &str) -> String {
    format!("data: {}\n\n", serde_json::json!({"choices":[{"delta":{"reasoning_content":s}}]}))
}
fn answer(parts: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = parts.iter().map(|p| content(p)).collect();
    v.push("data: [DONE]\n\n".into());
    v
}

async fn spawn_mock(m: Mock) -> (String, Arc<Stats>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stats = Arc::new(Stats::default());
    let st = stats.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let m = m.clone();
            let st = st.clone();
            tokio::spawn(async move {
                // Read headers + body.
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let header_end = loop {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") { break p + 4; }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
                    .unwrap_or(0);
                while buf.len() < header_end + len {
                    let n = sock.read(&mut tmp).await.unwrap_or(0);
                    if n == 0 { return; }
                    buf.extend_from_slice(&tmp[..n]);
                }
                st.requests.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(m.delay_ms)).await;
                let reason = if m.status == 200 { "OK" } else { "Error" };
                let ctype = if m.status == 200 { "text/event-stream" } else { "application/json" };
                let head = format!("HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n\r\n", m.status, reason, ctype);
                if sock.write_all(head.as_bytes()).await.is_err() { st.client_gone.store(true, Ordering::SeqCst); return; }
                if m.status != 200 {
                    let _ = sock.write_all(br#"{"error":{"message":"mock failure"}}"#).await;
                    return;
                }
                for c in &m.chunks {
                    if sock.write_all(c.as_bytes()).await.is_err() || sock.flush().await.is_err() {
                        st.client_gone.store(true, Ordering::SeqCst);
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(m.chunk_delay_ms)).await;
                }
            });
        }
    });
    (format!("http://127.0.0.1:{port}/v1"), stats)
}

fn route(id: &str, base: &str) -> Route {
    Route { id: id.into(), model: "mock".into(), base_url: base.into(), api_key_env: String::new(), extra_body: serde_json::Value::Null, direct: false }
}

fn core(routes: Vec<Route>, timeout_ms: u64) -> Core {
    let order = routes.iter().map(|r| r.id.clone()).collect();
    // Phase-1 semantics: HTTP engine only, routes staggered by `first_token_timeout_ms`.
    Core::new(Config { routes, order, first_token_timeout_ms: timeout_ms, http_race_at_start: false, mode: EngineMode::HttpOnly, health: quiet(), ..Config::default() }).unwrap()
}

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

fn fast(parts: &[&str]) -> Mock {
    Mock { status: 200, delay_ms: 0, chunks: answer(parts), chunk_delay_ms: 5 }
}

#[tokio::test]
async fn first_route_wins_when_fast_and_second_never_starts() {
    let (a, _) = spawn_mock(fast(&["你好！", "我是A。"])).await;
    let (b, sb) = spawn_mock(fast(&["我是B。"])).await;
    let evs = collect(&core(vec![route("a", &a), route("b", &b)], 1000), "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "你好！我是A。");
    assert_eq!(timings.route.as_deref(), Some("a"));
    assert_eq!(timings.routes_started, vec!["a"]);
    let sentences: Vec<_> = evs.iter().filter_map(|e| match e { Event::Sentence { text, .. } => Some(text.clone()), _ => None }).collect();
    assert_eq!(sentences, vec!["你好！", "我是A。"]);
    assert!(timings.first_token_ms.unwrap() <= timings.first_sentence_ms.unwrap());
    assert_eq!(sb.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn fails_over_to_second_route_when_first_is_slow() {
    let (a, sa) = spawn_mock(Mock { delay_ms: 3000, ..fast(&["慢。"]) }).await;
    let (b, _) = spawn_mock(fast(&["快。"])).await;
    let evs = collect(&core(vec![route("a", &a), route("b", &b)], 200), "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "快。");
    assert_eq!(timings.route.as_deref(), Some("b"));
    assert_eq!(timings.routes_started, vec!["a", "b"]);
    let ft = timings.first_token_ms.unwrap();
    assert!((190..1500).contains(&ft), "first token at {ft}ms");
    assert!(timings.total_ms < 1500, "loser must not delay Done: {}", timings.total_ms);
    assert_eq!(sa.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn thinking_only_route_counts_as_slow() {
    let mut chunks: Vec<String> = (0..100).map(|_| reasoning("嗯")).collect();
    chunks.extend(answer(&["想好了。"]));
    let (a, _) = spawn_mock(Mock { status: 200, delay_ms: 0, chunks, chunk_delay_ms: 20 }).await;
    let (b, _) = spawn_mock(fast(&["直接答。"])).await;
    let evs = collect(&core(vec![route("a", &a), route("b", &b)], 200), "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "直接答。");
    assert!(timings.reasoning_chars > 0);
}

#[tokio::test]
async fn immediate_failover_on_http_error() {
    let (a, _) = spawn_mock(Mock { status: 500, ..fast(&[]) }).await;
    let (b, _) = spawn_mock(fast(&["备用。"])).await;
    let evs = collect(&core(vec![route("a", &a), route("b", &b)], 5000), "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "备用。");
    assert!(timings.first_token_ms.unwrap() < 1000, "should not wait for the 5s timeout");
    assert!(timings.errors[0].starts_with("a: http 500"), "{:?}", timings.errors);
}

#[tokio::test]
async fn transport_error_fails_over() {
    // Port with nothing listening.
    let dead = { let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap(); format!("http://127.0.0.1:{}/v1", l.local_addr().unwrap().port()) };
    let (b, _) = spawn_mock(fast(&["好。"])).await;
    let evs = collect(&core(vec![route("a", &dead), route("b", &b)], 5000), "hi").await;
    assert!(matches!(evs.last(), Some(Event::Done { text, .. }) if text == "好。"), "{evs:?}");
}

#[tokio::test]
async fn all_routes_failing_reports_error() {
    let (a, _) = spawn_mock(Mock { status: 500, ..fast(&[]) }).await;
    let (b, _) = spawn_mock(Mock { status: 429, ..fast(&[]) }).await;
    let evs = collect(&core(vec![route("a", &a), route("b", &b)], 5000), "hi").await;
    let Some(Event::Error { message, .. }) = evs.last() else { panic!("{evs:?}") };
    assert!(message.contains("http 500") && message.contains("http 429"), "{message}");
}

#[tokio::test]
async fn cancel_stops_stream_promptly_and_closes_connection() {
    let chunks: Vec<String> = (0..500).map(|i| content(&format!("第{i}句。"))).collect();
    let (a, sa) = spawn_mock(Mock { status: 200, delay_ms: 0, chunks, chunk_delay_ms: 10 }).await;
    let c = core(vec![route("a", &a)], 1000);
    let cancel = CancellationToken::new();
    let mut turn = c.respond("hi", cancel.clone());
    let mut cancelled_at = None;
    let mut sentences_after_cancel = 0;
    while let Some(e) = turn.next().await {
        match e {
            Event::Sentence { index, .. } => {
                if cancelled_at.is_some() { sentences_after_cancel += 1; }
                if index == 3 && cancelled_at.is_none() { cancel.cancel(); cancelled_at = Some(Instant::now()); }
            }
            Event::Cancelled { text, .. } => {
                let stop = cancelled_at.unwrap().elapsed();
                assert!(stop < Duration::from_millis(50), "cancel took {stop:?}");
                assert!(text.starts_with("第0句。"));
                break;
            }
            other if other.is_terminal() => panic!("unexpected {other:?}"),
            _ => {}
        }
    }
    assert!(sentences_after_cancel <= 1, "late sentences leaked: {sentences_after_cancel}");
    // The server notices the client went away (body aborted, not drained).
    let t = Instant::now();
    while !sa.client_gone.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sa.client_gone.load(Ordering::SeqCst), "server kept streaming after cancel");
}

#[tokio::test]
async fn dropping_the_turn_cancels_it() {
    let chunks: Vec<String> = (0..500).map(|i| content(&format!("{i}。"))).collect();
    let (a, sa) = spawn_mock(Mock { status: 200, delay_ms: 0, chunks, chunk_delay_ms: 10 }).await;
    let c = core(vec![route("a", &a)], 1000);
    let mut turn = c.respond("hi", CancellationToken::new());
    while let Some(e) = turn.next().await { if matches!(e, Event::Sentence { .. }) { break; } }
    drop(turn);
    let t = Instant::now();
    while !sa.client_gone.load(Ordering::SeqCst) && t.elapsed() < Duration::from_secs(2) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(sa.client_gone.load(Ordering::SeqCst));
}

#[tokio::test]
async fn request_body_has_stream_and_extra_fields() {
    let r = Route::glm_coding_plan();
    assert_eq!(r.extra_body["reasoning_effort"], "low"); // GLM-5.3 rejects thinking=disabled
    assert_eq!(Route::glm_coding_plan_no_think().extra_body["thinking"]["type"], "disabled");
    assert_eq!(Config::default().order, vec!["glm-coding-plan", "deepseek-flash"]);
    assert_eq!(Route::deepseek_flash().extra_body["thinking"]["type"], "disabled");
    assert!(Config::default().http_race_at_start);
    assert_eq!(Config::default().mode, EngineMode::Dual);
    assert_eq!(Config::default().codex.model, "gpt-6-luna");
}

#[tokio::test]
async fn race_at_start_starts_every_route_at_t0() {
    let (a, sa) = spawn_mock(Mock { delay_ms: 400, ..fast(&["慢。"]) }).await;
    let (b, sb) = spawn_mock(Mock { delay_ms: 50, ..fast(&["快。"]) }).await;
    let routes = vec![route("a", &a), route("b", &b)];
    let order = routes.iter().map(|r| r.id.clone()).collect();
    let c = Core::new(Config { routes, order, first_token_timeout_ms: 5000, mode: EngineMode::HttpOnly, health: quiet(), ..Config::default() }).unwrap();
    let evs = collect(&c, "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "快。");
    assert_eq!(timings.engine.as_deref(), Some("http"));
    assert_eq!(timings.routes_started, vec!["a", "b"]);
    assert!(timings.first_token_ms.unwrap() < 400, "b must not wait for a stagger");
    assert_eq!((sa.requests.load(Ordering::SeqCst), sb.requests.load(Ordering::SeqCst)), (1, 1));
}

#[tokio::test]
async fn zero_width_tokens_do_not_win_the_race() {
    let mut chunks = vec![content("\u{200D}\u{200B}\u{200B}")];
    chunks.extend((0..20).map(|_| content("\u{200B}")));
    chunks.push("data: [DONE]\n\n".into());
    let (a, _) = spawn_mock(Mock { status: 200, delay_ms: 0, chunks, chunk_delay_ms: 20 }).await;
    let (b, _) = spawn_mock(Mock { delay_ms: 100, ..fast(&["真的回答。"]) }).await;
    let routes = vec![route("a", &a), route("b", &b)];
    let order = routes.iter().map(|r| r.id.clone()).collect();
    let c = Core::new(Config { routes, order, mode: EngineMode::HttpOnly, health: quiet(), ..Config::default() }).unwrap();
    let evs = collect(&c, "hi").await;
    let Some(Event::Done { text, timings }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "真的回答。");
    assert_eq!(timings.route.as_deref(), Some("b"));
}

/// Tests never write the real latency log.
fn quiet() -> fast_responder::HealthConfig {
    fast_responder::HealthConfig { log_path: None, ..Default::default() }
}

#[tokio::test]
async fn caller_supplied_keys_enable_a_route_without_the_environment() {
    let (a, sa) = spawn_mock(fast(&["有钥匙。"])).await;
    let mut r = route("keyed", &a);
    r.api_key_env = "XIAODAI_TEST_KEY_NEVER_SET_7F3A".into();
    let c = core(vec![r], 1000);
    assert!(c.available_routes().is_empty(), "no env key -> route unavailable");
    let evs = collect(&c, "hi").await;
    assert!(matches!(evs.last(), Some(Event::Error { .. })), "{evs:?}");
    let keyed = c.with_keys([("XIAODAI_TEST_KEY_NEVER_SET_7F3A", "test-value")]);
    assert_eq!(keyed.available_routes(), vec!["keyed"]);
    let evs = collect(&keyed, "hi").await;
    let Some(Event::Done { text, .. }) = evs.last() else { panic!("{evs:?}") };
    assert_eq!(text, "有钥匙。");
    assert_eq!(sa.requests.load(Ordering::SeqCst), 1);
    assert!(!format!("{evs:?}").contains("test-value"), "key never appears in events");
    let removed = keyed.with_keys([("XIAODAI_TEST_KEY_NEVER_SET_7F3A", "")]);
    assert!(removed.available_routes().is_empty());
}

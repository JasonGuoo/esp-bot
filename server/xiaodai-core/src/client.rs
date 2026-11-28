//! Shared HTTP client + one streaming chat attempt.
use crate::config::Route;
use crate::sse::{parse_chunk, truncate, Chunk, SseParser};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// API key wrapper that never prints its value.
#[derive(Clone)]
pub(crate) struct Secret(String);
impl Secret {
    pub(crate) fn new(s: String) -> Self {
        Self(s)
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(***)")
    }
}

pub(crate) fn build_client() -> reqwest::Result<Client> {
    Client::builder()
        .use_rustls_tls()
        .tcp_nodelay(true)
        .pool_idle_timeout(Duration::from_secs(90))
        .pool_max_idle_per_host(4)
        .connect_timeout(Duration::from_secs(3))
        .http2_keep_alive_interval(Duration::from_secs(20))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
}

#[derive(Debug)]
pub(crate) enum AttemptMsg {
    Token(String),
    Reasoning(usize),
    Finished,
    Failed(String),
    /// Codex only: a later utterance took over this turn (steer or interrupt barge-in).
    Superseded,
    /// HTTP only: the network path this attempt uses ("en0" = direct interface, "system").
    Network(String),
}

/// Open DNS/TCP/TLS (+h2) to the route so the first real turn skips the handshake.
pub(crate) async fn warmup(client: &Client, route: &Route) -> Result<u64, String> {
    let t = Instant::now();
    client
        .head(route.base_url.trim_end_matches('/'))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| format!("{}", e.without_url()))?;
    Ok(t.elapsed().as_millis() as u64)
}

/// Runs one attempt; sends tokens to `tx` tagged with `idx`. Returns promptly when `cancel` fires
/// (the response is dropped, which aborts the body / resets the h2 stream).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn attempt(
    client: Client,
    route: Route,
    key: Option<Secret>,
    body: Value,
    timeout: Duration,
    cancel: CancellationToken,
    idx: usize,
    tx: UnboundedSender<(usize, AttemptMsg)>,
) {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {}
        r = run(&client, &route, key, &body, timeout, idx, &tx) => {
            let msg = match r { Ok(()) => AttemptMsg::Finished, Err(e) => AttemptMsg::Failed(e) };
            let _ = tx.send((idx, msg));
        }
    }
}

async fn run(
    client: &Client,
    route: &Route,
    key: Option<Secret>,
    body: &Value,
    timeout: Duration,
    idx: usize,
    tx: &UnboundedSender<(usize, AttemptMsg)>,
) -> Result<(), String> {
    let url = format!("{}/chat/completions", route.base_url.trim_end_matches('/'));
    let mut req = client.post(url).timeout(timeout).header("Accept", "text/event-stream").json(body);
    if let Some(k) = &key {
        req = req.bearer_auth(&k.0);
    }
    let resp = req.send().await.map_err(|e| format!("transport: {}", e.without_url()))?;
    let status = resp.status();
    if !status.is_success() {
        let mut stream=resp.bytes_stream();let mut bytes=Vec::new();
        while bytes.len()<1024 {match stream.next().await {
            Some(Ok(chunk))=>bytes.extend_from_slice(&chunk[..chunk.len().min(1024-bytes.len())]),
            _=>break,
        }}
        let text=String::from_utf8_lossy(&bytes);
        return Err(format!("http {}: {}", status.as_u16(), truncate(text.trim(), 200)));
    }
    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    let mut events=0usize;
    let mut handle = |data: String| -> Option<Result<(), String>> {
        events+=1;
        if events>2048 {return Some(Err("provider event budget exceeded".into()));}
        match parse_chunk(&data) {
            Chunk::Content(s) => {
                if tx.send((idx, AttemptMsg::Token(s))).is_err() {
                    return Some(Ok(()));
                }
            }
            Chunk::Reasoning(n) => {
                let _ = tx.send((idx, AttemptMsg::Reasoning(n)));
            }
            Chunk::Finished { reason, content } => {
                if reason != "stop" { return Some(Err(format!("provider: incomplete response ({})", truncate(&reason, 80)))); }
                if let Some(s) = content { let _ = tx.send((idx, AttemptMsg::Token(s))); }
                return Some(Ok(()));
            }
            Chunk::Done => return Some(Ok(())),
            Chunk::Error(m) => return Some(Err(format!("provider: {m}"))),
            Chunk::Ignore => {}
        }
        None
    };
    let mut received=0usize;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("stream: {}", e.without_url()))?;
        received=received.saturating_add(chunk.len());
        if received>2*1024*1024 {return Err("provider stream byte budget exceeded".into());}
        for data in parser.feed(&chunk) {
            if let Some(r) = handle(data) {
                return r;
            }
        }
    }
    if let Some(data) = parser.finish() {
        if let Some(r) = handle(data) {
            return r;
        }
    }
    // A clean TCP EOF can still cut off an SSE answer. Require a semantic
    // finish (`stop`) or the provider's explicit [DONE] marker.
    Err("stream ended without completion".into())
}

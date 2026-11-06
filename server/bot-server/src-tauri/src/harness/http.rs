//! Small native OpenCode v2 HTTP/SSE adapter. No model API is implemented here.

use std::time::Duration;

use reqwest::{Client, Method, Response};
use serde_json::Value;

const MAX_JSON_BYTES: usize = 2 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Clone)]
pub(super) struct Api {
    client: Client,
    base: String,
    password: String,
    timeout: Duration,
}

#[derive(Debug)]
pub(super) struct ApiError {
    pub status: Option<u16>,
    pub body: Value,
    pub detail: &'static str,
}

impl ApiError {
    pub fn public_message(&self) -> String {
        self.status.map_or_else(
            || format!("opencode_api: {}", self.detail),
            |s| format!("opencode_api: {} (HTTP {s})", self.detail),
        )
    }
}

impl Api {
    pub fn new(client: Client, base: &str, password: String, timeout_ms: u64) -> Self {
        Self { client, base: base.trim_end_matches('/').to_owned(), password, timeout: Duration::from_millis(timeout_ms) }
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, format!("{}{}", self.base, path))
            .basic_auth("opencode", Some(&self.password))
            .header("Accept", "application/json")
    }

    pub async fn json(&self, method: Method, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
        let mut request = self.request(method, path).timeout(self.timeout);
        if let Some(body) = body { request = request.json(body); }
        let mut response = request.send().await.map_err(|_| ApiError {
            status: None, body: Value::Null, detail: "request failed or timed out",
        })?;
        let status = response.status();
        let bytes = read_bounded(&mut response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(ApiError {
                status: Some(status.as_u16()),
                body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                detail: "server rejected the request",
            });
        }
        if bytes.is_empty() { return Ok(Value::Null); }
        serde_json::from_slice(&bytes).map_err(|_| ApiError {
            status: Some(status.as_u16()), body: Value::Null, detail: "server returned invalid JSON",
        })
    }

    pub async fn events(&self) -> Result<Events, ApiError> {
        // Do not apply a whole-response HTTP timeout to an endless event stream.
        let response = tokio::time::timeout(self.timeout,
            self.request(Method::GET, "/api/event").header("Accept", "text/event-stream").send(),
        ).await.map_err(|_| ApiError { status: None, body: Value::Null, detail: "event subscription timed out" })?
            .map_err(|_| ApiError { status: None, body: Value::Null, detail: "event subscription failed" })?;
        if !response.status().is_success() {
            return Err(ApiError { status: Some(response.status().as_u16()), body: Value::Null, detail: "event subscription rejected" });
        }
        let content_type = response.headers().get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()).unwrap_or("");
        if !content_type.starts_with("text/event-stream") {
            return Err(ApiError { status: None, body: Value::Null, detail: "server did not return an SSE stream" });
        }
        Ok(Events { response, buffer: Vec::new() })
    }
}

async fn read_bounded(response: &mut Response, limit: usize) -> Result<Vec<u8>, ApiError> {
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(ApiError { status: None, body: Value::Null, detail: "response exceeds the bounded JSON budget" });
    }
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ApiError {
        status: None, body: Value::Null, detail: "response stream interrupted",
    })? {
        if data.len().saturating_add(chunk.len()) > limit {
            return Err(ApiError { status: None, body: Value::Null, detail: "response exceeds the bounded JSON budget" });
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}

pub(super) struct Events {
    response: Response,
    buffer: Vec<u8>,
}

impl Events {
    pub async fn next(&mut self) -> Result<Value, &'static str> {
        loop {
            if let Some((end, delimiter)) = boundary(&self.buffer) {
                let frame = self.buffer.drain(..end + delimiter).collect::<Vec<_>>();
                let text = std::str::from_utf8(&frame[..end]).map_err(|_| "invalid UTF-8 in event frame")?;
                let payload = text.lines().filter_map(|line| line.strip_prefix("data:")
                    .map(|value| value.strip_prefix(' ').unwrap_or(value))).collect::<Vec<_>>().join("\n");
                if payload.is_empty() { continue; } // comments and heartbeats
                return serde_json::from_str(&payload).map_err(|_| "invalid JSON in event frame");
            }
            let Some(chunk) = self.response.chunk().await.map_err(|_| "event stream interrupted")? else {
                return Err("event stream closed");
            };
            if self.buffer.len().saturating_add(chunk.len()) > MAX_EVENT_BYTES {
                return Err("event frame exceeds the bounded SSE budget");
            }
            self.buffer.extend_from_slice(&chunk);
        }
    }
}

fn boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index..].starts_with(b"\n\n") { return Some((index, 2)); }
        if bytes[index..].starts_with(b"\r\n\r\n") { return Some((index, 4)); }
        if bytes[index..].starts_with(b"\r\r") { return Some((index, 2)); }
    }
    None
}

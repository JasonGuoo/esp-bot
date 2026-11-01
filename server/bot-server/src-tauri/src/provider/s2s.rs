//! HF Speech-to-Speech sidecar adapter (Realtime-style WebSocket).
//!
//! EXPERIMENTAL and text-only: this round speaks text in / text out over
//! the sidecar's Realtime-compatible socket; audio frames, VAD and barge-in
//! arrive with the device audio integration (docs/local-first-hybrid-brain.md
//! §6). The endpoint shape of local HF speech-to-speech builds is still a
//! moving target, so everything protocol-specific lives in the pure frame
//! helpers below and the URL is fully configurable.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

use super::{Health, Provider, ProviderError};
use crate::config::S2sConfig;

const CHAT_TIMEOUT: Duration = Duration::from_secs(45);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);

pub struct S2sProvider {
    url: String,
}

impl S2sProvider {
    pub fn from_config(config: &S2sConfig) -> Self {
        Self { url: config.url.clone().unwrap_or_default() }
    }

    fn configured(&self) -> bool {
        !self.url.is_empty()
    }

    /// A selected provider without a URL stays unconfigured so the console
    /// shows the honest "endpoint not set" state.
    pub fn is_configured(&self) -> bool {
        self.configured()
    }
}

/// Pure helpers so the protocol mapping is unit-testable without a live
/// sidecar process.
pub fn conversation_item_frame(text: &str) -> String {
    json!({
        "type": "conversation.item.create",
        "item": {
            "type": "message",
            "role": "user",
            "content": [ { "type": "input_text", "text": text } ]
        }
    })
    .to_string()
}

pub fn response_create_frame() -> String {
    json!({ "type": "response.create" }).to_string()
}

/// Picks the assistant text out of the collected server events, preferring
/// the final `response.done` output over interim audio transcripts.
pub fn extract_reply(events: &[Value]) -> Option<String> {
    let mut transcript = None;
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some("response.audio_transcript.done") => {
                transcript = event.get("transcript").and_then(Value::as_str).map(str::to_owned);
            }
            Some("response.done") => {
                let texts: Vec<&str> = event
                    .pointer("/response/output")
                    .and_then(Value::as_array)
                    .map(|outputs| {
                        outputs
                            .iter()
                            .flat_map(|output| output.get("content").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]))
                            .filter_map(|content| {
                                (content.get("type").and_then(Value::as_str) == Some("text"))
                                    .then(|| content.get("text").and_then(Value::as_str))
                                    .flatten()
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(text) = texts.first() {
                    return Some((*text).to_owned());
                }
            }
            _ => {}
        }
    }
    transcript
}

/// `ws://host[:port]/…` → (host, port) with scheme defaults. Pure.
pub fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = match url.strip_prefix("ws://") {
        Some(rest) => ("ws", rest),
        None => match url.strip_prefix("wss://") {
            Some(rest) => ("wss", rest),
            None => ("ws", url),
        },
    };
    let authority = rest.split(['/', '?']).next()?;
    let default_port = if scheme == "wss" { 443 } else { 80 };
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => port.parse().ok().map(|port| (host.to_owned(), port)),
        _ => Some((authority.to_owned(), default_port)),
    }
}

#[async_trait]
impl Provider for S2sProvider {
    fn name(&self) -> &'static str {
        "s2s"
    }

    async fn health(&self) -> Health {
        if !self.configured() {
            return Health { ok: false, detail: "url not set".to_owned() };
        }
        let Some((host, port)) = host_port(&self.url) else {
            return Health { ok: false, detail: format!("cannot parse {}", self.url) };
        };
        match tokio::time::timeout(HEALTH_TIMEOUT, tokio::net::TcpStream::connect((host.as_str(), port))).await {
            Ok(Ok(_)) => Health { ok: true, detail: format!("tcp {host}:{port} reachable") },
            Ok(Err(e)) => Health { ok: false, detail: e.to_string() },
            Err(_) => Health { ok: false, detail: format!("tcp {host}:{port} timed out") },
        }
    }

    async fn chat(&self, message: &str) -> Result<String, ProviderError> {
        if !self.configured() {
            return Err(ProviderError::NotConfigured);
        }
        if self.url.starts_with("wss://") {
            // Honest refusal instead of a confusing TLS handshake failure:
            // the local sidecar deployment we target is plain ws://.
            return Err(ProviderError::Upstream(
                "wss:// is not supported by the s2s adapter yet (use ws:// localhost)".to_owned(),
            ));
        }
        let outcome = tokio::time::timeout(CHAT_TIMEOUT, async {
            let (mut socket, _response) = tokio_tungstenite::connect_async(self.url.clone())
                .await
                .map_err(|e| ProviderError::Unreachable(e.to_string()))?;
            socket
                .send(Message::Text(conversation_item_frame(message)))
                .await
                .map_err(|e| ProviderError::Upstream(format!("send item: {e}")))?;
            socket
                .send(Message::Text(response_create_frame()))
                .await
                .map_err(|e| ProviderError::Upstream(format!("send response.create: {e}")))?;

            let mut events = Vec::new();
            loop {
                match socket.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(value) = serde_json::from_str::<Value>(&text) else {
                            continue; // ignore non-JSON keepalives
                        };
                        let done = value.get("type").and_then(Value::as_str) == Some("response.done");
                        events.push(value);
                        if done {
                            break;
                        }
                    }
                    Some(Ok(_)) => continue, // ping/binary frames: tungstenite auto-handles pings
                    None => {
                        return Err(ProviderError::Upstream("socket closed before response.done".into()))
                    }
                    Some(Err(e)) => return Err(ProviderError::Upstream(format!("socket error: {e}"))),
                }
            }
            extract_reply(&events).ok_or_else(|| {
                ProviderError::Upstream("response.done carried no assistant text".to_owned())
            })
        })
        .await;
        match outcome {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Unreachable(format!(
                "no complete response within {} s",
                CHAT_TIMEOUT.as_secs()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::S2sConfig;

    #[test]
    fn frames_follow_realtime_shapes() {
        let item: Value = serde_json::from_str(&conversation_item_frame("你好")).unwrap();
        assert_eq!(item["type"], "conversation.item.create");
        assert_eq!(item["item"]["content"][0]["type"], "input_text");
        assert_eq!(item["item"]["content"][0]["text"], "你好");
        let create: Value = serde_json::from_str(&response_create_frame()).unwrap();
        assert_eq!(create["type"], "response.create");
    }

    #[test]
    fn extracts_reply_preferring_response_done() {
        let events = vec![
            json!({ "type": "response.audio_transcript.done", "transcript": "partial" }),
            json!({ "type": "response.done", "response": { "output": [
                { "content": [ { "type": "text", "text": "final answer" } ] }
            ] } }),
        ];
        assert_eq!(extract_reply(&events).as_deref(), Some("final answer"));

        let transcript_only = vec![json!({ "type": "response.audio_transcript.done", "transcript": "spoken" })];
        assert_eq!(extract_reply(&transcript_only).as_deref(), Some("spoken"));

        let empty = vec![json!({ "type": "response.done", "response": { "output": [] } })];
        assert_eq!(extract_reply(&empty), None);
    }

    #[test]
    fn parses_host_port_with_defaults() {
        assert_eq!(host_port("ws://127.0.0.1:7860/v1/realtime"), Some(("127.0.0.1".into(), 7860)));
        assert_eq!(host_port("ws://localhost/api"), Some(("localhost".into(), 80)));
        assert_eq!(host_port("wss://example.com/api"), Some(("example.com".into(), 443)));
        assert_eq!(host_port("not a url"), Some(("not a url".into(), 80)));
    }

    #[test]
    fn unconfigured_provider_is_not_configured() {
        let provider = S2sProvider::from_config(&S2sConfig::default());
        assert!(!provider.configured());
    }
}

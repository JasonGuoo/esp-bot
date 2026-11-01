//! OpenAI-compatible localhost adapter (mlx-lm serve, LM Studio, …).
//!
//! Ports the legacy Python Brain adapter (server/espbot/llm.py) one-to-one:
//! non-streaming `/chat/completions`, 30 s timeout, 512 KB response cap,
//! identical system prompt, optional bearer key.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

use super::{Health, Provider, ProviderError};
use crate::config::OpenAiConfig;

/// Kept verbatim from the Python prototype so model behaviour does not
/// drift between the legacy and Rust brains.
const SYSTEM_PROMPT: &str =
    "You are the speech brain of a small friendly robot. Be concise, clear, and truthful. Prefer short spoken replies.";

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_RESPONSE_BYTES: usize = 512 * 1024;

pub struct OpenAiProvider {
    base_url: String,
    api_key: Option<String>,
    model: String,
    client: reqwest::Client,
}

impl OpenAiProvider {
    pub fn from_config(config: &OpenAiConfig) -> Self {
        Self {
            base_url: config
                .base_url
                .as_deref()
                .unwrap_or("")
                .trim_end_matches('/')
                .to_owned(),
            api_key: config.api_key.clone().filter(|key| !key.is_empty()),
            model: config.model.clone().unwrap_or_else(|| "default".to_owned()),
            client: reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().unwrap_or_default(),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    fn configured(&self) -> bool {
        !self.base_url.is_empty()
    }

    /// A selected provider without a base_url stays unconfigured so the
    /// console shows the honest "endpoint not set" state.
    pub fn is_configured(&self) -> bool {
        self.configured()
    }

    pub fn request_body(&self, message: &str) -> Value {
        json!({
            "model": self.model,
            "stream": false,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": message }
            ]
        })
    }

    /// Extracts `choices[0].message.content` (pure; unit-tested).
    pub fn parse_reply(body: &Value) -> Result<String, ProviderError> {
        body.pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                ProviderError::Upstream("response missing choices[0].message.content".to_owned())
            })
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn name(&self) -> &'static str {
        "openai"
    }

    async fn health(&self) -> Health {
        if !self.configured() {
            return Health { ok: false, detail: "base_url not set".to_owned() };
        }
        let request = self
            .client
            .get(format!("{}/models", self.base_url))
            .timeout(HEALTH_TIMEOUT)
            .build();
        let request = match request {
            Ok(request) => request,
            Err(e) => return Health { ok: false, detail: e.to_string() },
        };
        // Any HTTP answer counts as reachable; only connect failures mean
        // the sidecar is down.
        match self.client.execute(request).await {
            Ok(response) => Health { ok: true, detail: format!("HTTP {}", response.status()) },
            Err(e) => Health { ok: false, detail: e.to_string() },
        }
    }

    async fn chat(&self, message: &str) -> Result<String, ProviderError> {
        if !self.configured() {
            return Err(ProviderError::NotConfigured);
        }
        let mut request = self.client.post(self.chat_url()).json(&self.request_body(message));
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let mut response = request.send().await.map_err(|e| ProviderError::Unreachable(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let excerpt: String = response.text().await.unwrap_or_default().chars().take(200).collect();
            return Err(ProviderError::Upstream(format!("HTTP {status}: {excerpt}")));
        }
        // Cap the download like the Python adapter instead of trusting the
        // sidecar to be well-behaved.
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| ProviderError::Upstream(e.to_string()))? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_RESPONSE_BYTES {
                return Err(ProviderError::Upstream(format!(
                    "response exceeds {} byte cap",
                    MAX_RESPONSE_BYTES
                )));
            }
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|e| ProviderError::Upstream(format!("invalid JSON: {e}")))?;
        Self::parse_reply(&value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::PROVIDER_OPENAI;
    use serde_json::json;

    #[test]
    fn request_body_matches_python_prototype() {
        let provider = OpenAiProvider::from_config(&OpenAiConfig {
            base_url: Some("http://127.0.0.1:8080/v1".into()),
            api_key: None,
            model: Some("qwen".into()),
        });
        let body = provider.request_body("你好");
        assert_eq!(body["model"], "qwen");
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], SYSTEM_PROMPT);
        assert_eq!(body["messages"][1]["content"], "你好");
    }

    #[test]
    fn parses_reply_and_rejects_missing_content() {
        let good = json!({ "choices": [ { "message": { "role": "assistant", "content": "hi there" } } ] });
        assert_eq!(OpenAiProvider::parse_reply(&good).unwrap(), "hi there");
        let bad = json!({ "choices": [] });
        assert!(matches!(OpenAiProvider::parse_reply(&bad), Err(ProviderError::Upstream(_))));
    }

    #[test]
    fn unconfigured_provider_reports_not_configured() {
        let provider = OpenAiProvider::from_config(&OpenAiConfig::default());
        assert!(!provider.configured());
        assert_eq!(provider.name(), PROVIDER_OPENAI);
    }

    /// Real-network roundtrip through the actual adapter (reqwest, timeouts,
    /// size cap, response parsing) against a live OpenAI-compatible server.
    /// Not part of the default suite — run it explicitly, e.g. against the
    /// repo mock or a real local model:
    ///
    ///   node scripts/mock_llm_server.mjs &
    ///   ESPBOT_TEST_OPENAI_BASE=http://127.0.0.1:8765/v1 \
    ///     cargo test --manifest-path src-tauri/Cargo.toml -- --ignored real_http
    #[tokio::test]
    #[ignore = "needs a live server; set ESPBOT_TEST_OPENAI_BASE"]
    async fn real_http_roundtrip_against_live_server() {
        let base = std::env::var("ESPBOT_TEST_OPENAI_BASE").expect("set ESPBOT_TEST_OPENAI_BASE");
        let provider = OpenAiProvider::from_config(&OpenAiConfig {
            base_url: Some(base),
            api_key: None,
            model: Some("mock-model".to_owned()),
        });
        let health = provider.health().await;
        assert!(health.ok, "health probe failed: {}", health.detail);
        let reply = provider.chat("你好小呆").await.expect("real chat roundtrip");
        assert_eq!(reply, "Mock reply to: 你好小呆");
    }
}

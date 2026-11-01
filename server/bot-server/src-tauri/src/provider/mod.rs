//! Local-first AI providers.
//!
//! Rust adapts; it does not reimplement inference. Providers are localhost
//! sidecars (MLX/LM Studio OpenAI-compatible HTTP, HF Speech-to-Speech
//! Realtime-style WS) selected in config.json. When no provider is
//! configured, chat fails honestly with `llm_not_configured` — never a
//! fake answer (docs/local-first-hybrid-brain.md §3).

mod openai;
mod s2s;

use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use serde_json::Value;

use crate::config::{self, BotServerConfig};
use crate::lan::{EventSink, LanEvent};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    NotConfigured,
    /// Connection/timeout problems (Python prototype: network failures).
    Unreachable(String),
    /// The provider answered with an error or unusable payload (llm_error).
    Upstream(String),
}

impl ProviderError {
    pub fn code(&self) -> &'static str {
        match self {
            ProviderError::NotConfigured => "llm_not_configured",
            ProviderError::Unreachable(_) => "llm_unreachable",
            ProviderError::Upstream(_) => "llm_error",
        }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderError::NotConfigured => write!(f, "no LLM provider is configured"),
            ProviderError::Unreachable(detail) => write!(f, "provider unreachable: {detail}"),
            ProviderError::Upstream(detail) => write!(f, "provider error: {detail}"),
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Health {
    pub ok: bool,
    pub detail: String,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    async fn health(&self) -> Health;
    /// One-shot text reply. Message length is bounded by the caller.
    async fn chat(&self, message: &str) -> Result<String, ProviderError>;
}

/// Holds the selected provider built from config.json; reloadable at
/// runtime from the console. Config problems surface as console events.
pub struct ProviderRegistry {
    config_dir: PathBuf,
    sink: EventSink,
    inner: RwLock<Inner>,
}

struct Inner {
    config: BotServerConfig,
    provider: Option<Arc<dyn Provider>>,
}

impl ProviderRegistry {
    pub fn new(config_dir: PathBuf, sink: EventSink) -> Arc<Self> {
        let registry = Arc::new(Self {
            config_dir,
            sink,
            inner: RwLock::new(Inner { config: BotServerConfig::default(), provider: None }),
        });
        registry.reload();
        registry
    }

    /// Re-reads config.json and rebuilds the provider. A bad file keeps the
    /// previous selection and reports the exact reason.
    pub fn reload(&self) {
        match config::load_or_create(&self.config_dir) {
            Ok(config) => {
                let provider = build_provider(&config);
                let detail = match (&config.provider, &provider) {
                    (_, Some(provider)) => format!("provider {} selected", provider.name()),
                    (selection, None) if selection != config::PROVIDER_NONE => {
                        format!("provider {selection} selected but its endpoint is not configured")
                    }
                    _ => "no provider selected (chat disabled)".to_owned(),
                };
                if let Ok(mut inner) = self.inner.write() {
                    inner.config = config;
                    inner.provider = provider;
                }
                (self.sink)(LanEvent::new("provider_loaded", None, &detail, None));
            }
            Err(message) => {
                // Keep whatever was loaded before; do not crash on a typo.
                (self.sink)(LanEvent::new("config_error", None, &message, None));
            }
        }
    }

    pub fn provider(&self) -> Option<Arc<dyn Provider>> {
        self.inner.read().ok().and_then(|inner| inner.provider.clone())
    }

    /// UI status; never exposes the API key.
    pub fn status(&self) -> Value {
        let inner = self.inner.read().ok();
        let config = inner.as_ref().map(|i| i.config.clone());
        match config {
            Some(config) => serde_json::json!({
                "provider": config.provider,
                "configured": inner.as_ref().is_some_and(|i| i.provider.is_some()),
                "openai_base_url": config.openai.base_url,
                "s2s_url": config.s2s.url,
            }),
            None => serde_json::json!({"provider": "unknown", "configured": false}),
        }
    }

    pub async fn chat(&self, message: &str) -> Result<String, ProviderError> {
        let provider = self.provider().ok_or(ProviderError::NotConfigured)?;
        provider.chat(message).await
    }
}

fn build_provider(config: &BotServerConfig) -> Option<Arc<dyn Provider>> {
    match config.provider.as_str() {
        config::PROVIDER_OPENAI => {
            let provider = openai::OpenAiProvider::from_config(&config.openai);
            provider.is_configured().then(|| Arc::new(provider) as Arc<dyn Provider>)
        }
        config::PROVIDER_S2S => {
            let provider = s2s::S2sProvider::from_config(&config.s2s);
            provider.is_configured().then(|| Arc::new(provider) as Arc<dyn Provider>)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn sink() -> EventSink {
        static EVENTS: AtomicUsize = AtomicUsize::new(0);
        Arc::new(|_event| {
            EVENTS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn temp_config_dir() -> PathBuf {
        std::env::temp_dir().join(format!("espbot-registry-{}", uuid::Uuid::new_v4().simple()))
    }

    #[test]
    fn none_config_disables_chat_honestly() {
        let dir = temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(config::config_path(&dir), r#"{"provider":"none"}"#).unwrap();
        let registry = ProviderRegistry::new(dir.clone(), sink());
        assert!(registry.provider().is_none());
        assert_eq!(registry.status()["configured"], serde_json::json!(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn openai_selection_builds_provider() {
        let dir = temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            config::config_path(&dir),
            r#"{"provider":"openai","openai":{"base_url":"http://127.0.0.1:9/v1","model":"m"}}"#,
        )
        .unwrap();
        let registry = ProviderRegistry::new(dir.clone(), sink());
        assert!(registry.provider().is_some());
        assert_eq!(registry.status()["configured"], serde_json::json!(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn openai_without_endpoint_stays_unconfigured() {
        let dir = temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(config::config_path(&dir), r#"{"provider":"openai"}"#).unwrap();
        let registry = ProviderRegistry::new(dir.clone(), sink());
        assert!(registry.provider().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bad_config_keeps_previous_selection() {
        let dir = temp_config_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            config::config_path(&dir),
            r#"{"provider":"openai","openai":{"base_url":"http://127.0.0.1:9/v1","model":"m"}}"#,
        )
        .unwrap();
        let registry = ProviderRegistry::new(dir.clone(), sink());
        assert!(registry.provider().is_some());

        std::fs::write(config::config_path(&dir), "{ not json").unwrap();
        registry.reload();
        assert!(registry.provider().is_some(), "reload on a broken file must keep the working provider");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

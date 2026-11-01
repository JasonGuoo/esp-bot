//! Console configuration (`config.json` in the app config dir).
//!
//! Load-or-default: a missing file is written with the honest `none`
//! provider selection; a malformed file never kills the app — the caller
//! falls back to no provider and surfaces the parse error in the console.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

pub const PROVIDER_NONE: &str = "none";
pub const PROVIDER_OPENAI: &str = "openai";
pub const PROVIDER_S2S: &str = "s2s";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BotServerConfig {
    /// `none` | `openai` | `s2s`. `none` keeps chat honestly disabled.
    pub provider: String,
    pub openai: OpenAiConfig,
    pub s2s: S2sConfig,
}

impl Default for BotServerConfig {
    fn default() -> Self {
        Self {
            provider: PROVIDER_NONE.to_owned(),
            openai: OpenAiConfig::default(),
            s2s: S2sConfig::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiConfig {
    /// OpenAI-compatible base URL, e.g. mlx-lm serve or LM Studio on localhost.
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub model: Option<String>,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self { base_url: None, api_key: None, model: None }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct S2sConfig {
    /// Realtime-style WebSocket URL of a local HF Speech-to-Speech sidecar.
    pub url: Option<String>,
}

impl Default for S2sConfig {
    fn default() -> Self {
        Self { url: None }
    }
}

pub fn config_path(dir: &std::path::Path) -> PathBuf {
    dir.join("config.json")
}

/// Loads config.json, creating a commented-in-spirit default on first run.
/// Returns `Err` with the exact parse problem so the console can show it.
pub fn load_or_create(dir: &std::path::Path) -> Result<BotServerConfig, String> {
    let path = config_path(dir);
    if !path.exists() {
        let config = BotServerConfig::default();
        // Hand-written template so the file teaches valid values instead of
        // showing bare nulls.
        let template = "{\n  \"provider\": \"none\",\n  \"openai\": {\n    \"base_url\": \"http://127.0.0.1:8080/v1\",\n    \"api_key\": null,\n    \"model\": \"default\"\n  },\n  \"s2s\": {\n    \"url\": \"ws://127.0.0.1:7860/v1/realtime\"\n  }\n}\n";
        if let Err(e) = fs::create_dir_all(dir) {
            return Err(format!("cannot create config dir {}: {e}", dir.display()));
        }
        if let Err(e) = fs::write(&path, template) {
            return Err(format!("cannot write {}: {e}", path.display()));
        }
        return Ok(config);
    }
    let text = fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("espbot-config-{}", Uuid::new_v4().simple()))
    }

    #[test]
    fn first_run_creates_none_provider_default() {
        let dir = temp_dir();
        let config = load_or_create(&dir).unwrap();
        assert_eq!(config.provider, PROVIDER_NONE);
        // Second load returns the same template file, still valid.
        assert!(load_or_create(&dir).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_full_config() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            config_path(&dir),
            r#"{"provider":"openai","openai":{"base_url":"http://127.0.0.1:1234/v1","api_key":"k","model":"qwen"},"s2s":{"url":"ws://127.0.0.1:9000/v1/realtime"}}"#,
        )
        .unwrap();
        let config = load_or_create(&dir).unwrap();
        assert_eq!(config.provider, PROVIDER_OPENAI);
        assert_eq!(config.openai.base_url.as_deref(), Some("http://127.0.0.1:1234/v1"));
        assert_eq!(config.s2s.url.as_deref(), Some("ws://127.0.0.1:9000/v1/realtime"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_config_reports_exact_error() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(config_path(&dir), "{ not json").unwrap();
        let err = load_or_create(&dir).unwrap_err();
        assert!(err.contains("invalid config"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_sections_default_to_none() {
        let config: BotServerConfig = serde_json::from_str(r#"{"provider":"s2s"}"#).unwrap();
        assert_eq!(config.provider, PROVIDER_S2S);
        assert_eq!(config.s2s, S2sConfig::default());
    }
}

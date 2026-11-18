use std::{fs, path::{Path, PathBuf}};
use serde::{Deserialize, Serialize};
use super::worker::ModelWorkerConfig;
use crate::{harness::HarnessConfig, memory::MemoryConfig};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrainConfig {
    pub schema_version: u32,
    pub auto_start: bool,
    pub harness: HarnessConfig,
    pub models: ModelWorkerConfig,
    pub memory: MemoryConfig,
    pub gateway: GatewayConfig,
    pub conversation: ConversationConfig,
    pub tasks: super::tasks::TaskConfig,
    pub presentation: super::presentation::PresentationConfig,
    /// Default conversational brain (Xiaodai core); OpenCode keeps tools and tasks.
    pub brain_core: super::fast_core::FastCoreConfig,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self { schema_version: 1, auto_start: false, harness: HarnessConfig::default(),
            models: ModelWorkerConfig::default(), memory: MemoryConfig::default(),
            gateway: GatewayConfig::default(), conversation: ConversationConfig::default(),
            tasks: Default::default(), presentation: Default::default(), brain_core: Default::default() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub bind: String,
    pub certificate_pem: Option<PathBuf>,
    pub private_key_pem: Option<PathBuf>,
    pub pairing_ca_pem: Option<PathBuf>,
    pub max_connections: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self { enabled: false, bind: "0.0.0.0:7443".into(), certificate_pem: None,
            private_key_pem: None, pairing_ca_pem: None, max_connections: 4 }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConversationConfig {
    pub language: String,
    pub default_speak: bool,
    pub max_reply_characters: usize,
    pub idle_session_seconds: u64,
    /// Only approved HTTPS origins may appear as images or web frames.
    pub media_hosts: Vec<String>,
}

impl Default for ConversationConfig {
    fn default() -> Self {
        Self { language: "zh-CN".into(), default_speak: true, max_reply_characters: 1200,
            idle_session_seconds: 300, media_hosts: Vec::new() }
    }
}

pub fn load_or_create(dir: &Path, bundled_worker: Option<PathBuf>) -> Result<BrainConfig, String> {
    private_dir(dir)?;
    let path = dir.join("brain.json");
    let source = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("brain_config_read: {e}")),
    };
    let mut config = if let Some(text) = &source {
        serde_json::from_str(text).map_err(|e| format!("brain_config_invalid: {e}"))?
    } else {
        let mut config = BrainConfig::default();
        config.models.script = bundled_worker.clone();
        config.models.model_dir = Some(dir.join("models/local"));
        save(dir, &config)?;
        config
    };
    // The worker is a bundled source resource; models remain an explicit install.
    // Installations migrated by our helper use this known default; prefer the
    // current signed bundle on updates. User-selected custom workers are kept.
    if config.models.script.as_deref() == Some(dir.join("engines/local-models/worker.py").as_path()) {
        if let Some(resource) = bundled_worker.clone() { config.models.script = Some(resource); }
    }
    if config.models.model_dir.is_none() { config.models.model_dir = Some(dir.join("models/local")); }
    if config.tasks.media_script.is_none() { config.tasks.media_script = bundled_worker.as_ref().or(config.models.script.as_ref()).and_then(|p|p.parent()).map(|p|p.join("media_prepare.py")).filter(|p|p.is_file()); }
    // Media helpers use Pillow from the explicitly configured local-model
    // environment. Keep an operator-selected separate interpreter intact.
    if config.tasks.python==super::tasks::TaskConfig::default().python {config.tasks.python=config.models.python.clone();}
    validate(&config)?;
    if let Some(text) = source.filter(|_| config.harness.route_order_version < HarnessConfig::ROUTE_ORDER_VERSION) {
        let previous = config.harness.routes.clone();
        config.harness.upgrade_route_defaults();
        // Patch the stored document so unrelated missing fields still inherit
        // defaults, including the worker paths resolved above for this run.
        let mut stored: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let harness = &mut stored["harness"];
        harness["route_order_version"] = config.harness.route_order_version.into();
        if previous != config.harness.routes {
            harness["routes"] = serde_json::to_value(&config.harness.routes).map_err(|e| e.to_string())?;
        }
        private_write(&path, &serde_json::to_vec_pretty(&stored).map_err(|e| e.to_string())?)?;
    }
    Ok(config)
}

pub fn validate(config: &BrainConfig) -> Result<(), String> {
    if config.schema_version != 1 { return Err("unsupported_brain_config_version".into()); }
    config.harness.validate()?;
    config.memory.validate()?;
    config.tasks.validate()?;
    config.presentation.validate()?;
    config.brain_core.validate()?;
    if config.models.speaker_python.as_ref().is_some_and(|p|!p.is_absolute()){return Err("speaker_python_path_must_be_absolute".into());}
    if !matches!(config.models.tts_backend.as_str(),"auto"|"kokoro"|"say"){return Err("tts_backend_must_be_auto_kokoro_or_say".into());}
    if [&config.models.kokoro_model_dir,&config.models.tts_python].iter().any(|p|p.as_ref().is_some_and(|p|!p.is_absolute())){return Err("kokoro_paths_must_be_absolute".into());}
    if !(1..=16).contains(&config.models.kokoro_threads){return Err("kokoro_threads_must_be_1_to_16".into());}
    if config.models.diarization_model.as_ref().is_some_and(|p|!p.is_absolute()){return Err("diarization_path_must_be_absolute".into());}
    if !(100..=4000).contains(&config.conversation.max_reply_characters) { return Err("reply_limit_must_be_100_to_4000".into()); }
    if !(30..=1800).contains(&config.conversation.idle_session_seconds) { return Err("session_idle_must_be_30_to_1800_seconds".into()); }
    if config.conversation.media_hosts.len() > 32 || config.conversation.media_hosts.iter().any(|host|
        host.is_empty() || host.len() > 253 || host.contains('/') || host.contains(':') || host.contains('*') || host.chars().any(char::is_whitespace)) {
        return Err("media_hosts_require_exact_dns_names".into());
    }
    if config.models.python.trim().is_empty() { return Err("python_executable_required".into()); }
    if config.models.script.as_ref().is_some_and(|p| !p.is_absolute()) || config.models.model_dir.as_ref().is_some_and(|p| !p.is_absolute()) {
        return Err("model_paths_must_be_absolute".into());
    }
    if !matches!(config.models.asr_backend.as_str(), "whisper" | "rma_qwen") {
        return Err("asr_backend_invalid".into());
    }
    let rma_paths = [&config.models.rma_qwen_python, &config.models.rma_qwen_worker, &config.models.rma_qwen_model];
    if rma_paths.iter().any(|path| path.as_ref().is_some_and(|p| !p.is_absolute())) {
        return Err("rma_qwen_paths_must_be_absolute".into());
    }
    if config.models.asr_backend == "rma_qwen" && rma_paths.iter().any(|path| path.is_none()) {
        return Err("rma_qwen_paths_required".into());
    }
    if config.gateway.enabled {
        config.gateway.bind.parse::<std::net::SocketAddr>().map_err(|_| "gateway_bind_invalid")?;
        if config.gateway.certificate_pem.is_none() || config.gateway.private_key_pem.is_none() {
            return Err("gateway_requires_tls_certificate_and_key".into());
        }
    }
    Ok(())
}

pub fn save(dir: &Path, config: &BrainConfig) -> Result<(), String> {
    validate(config)?;
    let mut config = config.clone();
    // Explicit saves express the caller's chosen order, including pasted older
    // JSON. Record that choice without applying the startup default migration.
    config.harness.route_order_version = config.harness.route_order_version.max(HarnessConfig::ROUTE_ORDER_VERSION);
    let bytes = serde_json::to_vec_pretty(&config).map_err(|e| e.to_string())?;
    private_write(&dir.join("brain.json"), &bytes)
}

pub fn private_dir(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|e| format!("private_dir_create: {e}"))?;
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn private_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("private_file_parent_missing")?;
    private_dir(parent)?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|e| e.to_string())?;
    let result = file.write_all(bytes).and_then(|_| file.sync_all()).and_then(|_| fs::rename(&temporary, path));
    if result.is_err() { let _ = fs::remove_file(&temporary); }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrading_routes_preserves_omitted_settings_on_disk() {
        let mut routes = HarnessConfig::default().routes;
        routes[0].base_url = Some("https://api.z.ai/api/coding/paas/v4".into());
        let mut expected = serde_json::json!({
            "harness": {"route_order_version": 1, "routes": routes},
            "memory": {"history_enabled": false},
        });
        let dir = std::env::temp_dir().join(format!("espbot-route-upgrade-{}", uuid::Uuid::new_v4()));
        let path = dir.join("brain.json");
        private_write(&path, &serde_json::to_vec(&expected).unwrap()).unwrap();
        let config = load_or_create(&dir, None).unwrap();
        assert_eq!(config.harness.routes, HarnessConfig::default().routes);
        expected["harness"]["route_order_version"] = HarnessConfig::ROUTE_ORDER_VERSION.into();
        expected["harness"]["routes"][0]["base_url"] = serde_json::json!(config.harness.routes[0].base_url);
        let actual: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn old_model_configuration_remains_valid_and_rma_requires_absolute_paths() {
        let mut config: BrainConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(config.models.asr_backend, "whisper");
        assert!(validate(&config).is_ok());
        config.models.asr_backend = "rma_qwen".into();
        assert_eq!(validate(&config).unwrap_err(), "rma_qwen_paths_required");
        config.models.rma_qwen_python = Some("/tmp/rma/python".into());
        config.models.rma_qwen_worker = Some("relative/worker.py".into());
        config.models.rma_qwen_model = Some("/tmp/rma/model".into());
        assert_eq!(validate(&config).unwrap_err(), "rma_qwen_paths_must_be_absolute");
        config.models.rma_qwen_worker = Some("/tmp/rma/worker.py".into());
        assert!(validate(&config).is_ok());
    }
}

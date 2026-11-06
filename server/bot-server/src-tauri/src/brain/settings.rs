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
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self { schema_version: 1, auto_start: false, harness: HarnessConfig::default(),
            models: ModelWorkerConfig::default(), memory: MemoryConfig::default(),
            gateway: GatewayConfig::default(), conversation: ConversationConfig::default() }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub bind: String,
    pub certificate_pem: Option<PathBuf>,
    pub private_key_pem: Option<PathBuf>,
    pub max_connections: usize,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self { enabled: false, bind: "0.0.0.0:7443".into(), certificate_pem: None,
            private_key_pem: None, max_connections: 4 }
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
    let mut config = if path.exists() {
        let text = fs::read_to_string(&path).map_err(|e| format!("brain_config_read: {e}"))?;
        serde_json::from_str(&text).map_err(|e| format!("brain_config_invalid: {e}"))?
    } else {
        let mut config = BrainConfig::default();
        config.models.script = bundled_worker;
        config.models.model_dir = Some(dir.join("models"));
        save(dir, &config)?;
        config
    };
    // The worker is a bundled source resource; models remain an explicit install.
    if config.models.model_dir.is_none() { config.models.model_dir = Some(dir.join("models")); }
    validate(&config)?;
    Ok(config)
}

pub fn validate(config: &BrainConfig) -> Result<(), String> {
    if config.schema_version != 1 { return Err("unsupported_brain_config_version".into()); }
    config.harness.validate()?;
    config.memory.validate()?;
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
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
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

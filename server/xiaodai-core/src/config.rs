use crate::health::HealthConfig;
use crate::net::HttpNetwork;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

/// The bot-server folder both engines may fully read/write. Override with `XIAODAI_WORKSPACE_ROOT`.
pub const DEFAULT_WORKSPACE_ROOT: &str = "/Users/jason/Xiaodai_Server";

pub fn default_workspace_root() -> PathBuf {
    std::env::var_os("XIAODAI_WORKSPACE_ROOT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WORKSPACE_ROOT))
}

pub const DEFAULT_SYSTEM_PROMPT: &str = "你是一个语音助手，你的回答会被直接朗读出来。请用简体中文口语回答，只说1到3句短句，简洁自然。不要使用Markdown、列表、标题、表情符号、代码或网址。";

/// One OpenAI-compatible `/chat/completions` provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Route {
    pub id: String,
    pub model: String,
    /// Base URL without `/chat/completions`.
    pub base_url: String,
    /// Environment variable holding the API key. Empty = no auth (local/mock servers).
    pub api_key_env: String,
    /// Extra JSON fields merged into the request body (e.g. disable thinking).
    #[serde(default)]
    pub extra_body: Value,
    /// Eligible for the direct (active physical interface) path, see `Config::http_network`.
    /// True for the built-in GLM / DeepSeek / Qwen routes; custom routes default to false.
    #[serde(default)]
    pub direct: bool,
}

impl Route {
    /// GLM-5.3 on the coding plan. z.ai docs: GLM-5.3 uses *forced* thinking and returns an
    /// error if `thinking.type` is `disabled`, so the fastest legal setting is the lowest effort
    /// (coding plan maps none/minimal/low -> low). Thinking deltas are ignored by the core.
    pub fn glm_coding_plan() -> Self {
        Self {
            id: "glm-coding-plan".into(),
            model: "glm-5.3".into(),
            base_url: "https://api.z.ai/api/coding/paas/v4".into(),
            api_key_env: "GLM_CODING_PLAN_API_KEY".into(),
            extra_body: json!({ "thinking": { "type": "enabled" }, "reasoning_effort": "low" }),
            direct: true,
        }
    }

    /// Same model on Zhipu's mainland endpoint (`open.bigmodel.cn`), which esp-bot switched to on
    /// 2026-11-18. Same key variable; whether a given key works on both sites depends on where it
    /// was issued. Not in the default order.
    pub fn glm_coding_plan_cn() -> Self {
        Self { id: "glm-coding-plan-cn".into(), base_url: "https://open.bigmodel.cn/api/coding/paas/v4".into(), ..Self::glm_coding_plan() }
    }

    /// GLM-5.2 with thinking truly disabled (docs: only GLM-5.2 and older allow it).
    /// Not in the default order; availability on the coding plan is unverified.
    pub fn glm_coding_plan_no_think() -> Self {
        Self {
            id: "glm-5.2-no-think".into(),
            model: "glm-5.2".into(),
            base_url: "https://api.z.ai/api/coding/paas/v4".into(),
            api_key_env: "GLM_CODING_PLAN_API_KEY".into(),
            extra_body: json!({ "thinking": { "type": "disabled" } }),
            direct: true,
        }
    }

    /// DeepSeek Flash with thinking disabled. With thinking on it spent the whole 200-token
    /// budget on hidden reasoning and returned an empty answer; disabled: TTFT ~0.6 s.
    pub fn deepseek_flash() -> Self {
        Self {
            id: "deepseek-flash".into(),
            model: "deepseek-flash".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            extra_body: json!({ "thinking": { "type": "disabled" } }),
            direct: true,
        }
    }

    /// Supported, not in the default order.
    pub fn qwen_coding_plan() -> Self {
        Self {
            id: "qwen-coding-plan".into(),
            model: "qwen3.7-plus".into(),
            base_url: "https://coding.dashscope.aliyuncs.com/v1".into(),
            api_key_env: "QWEN_CODING_PLAN_API_KEY".into(),
            extra_body: json!({ "enable_thinking": false }),
            direct: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub routes: Vec<Route>,
    /// Route ids to try, in order. Routes without a key are skipped.
    pub order: Vec<String>,
    /// Start the next route if the current one has produced no text token by then.
    pub first_token_timeout_ms: u64,
    /// Whole-request ceiling per attempt.
    pub request_timeout_ms: u64,
    pub max_tokens: u32,
    pub temperature: f32,
    pub system_prompt: String,
    /// Force a sentence break after this many chars without punctuation.
    pub max_sentence_chars: usize,
    /// HTTP engine: start every route at t=0 (true, default) or stagger them by
    /// `first_token_timeout_ms` (false, the phase-1 behaviour).
    pub http_race_at_start: bool,
    /// Which engines to use.
    pub mode: EngineMode,
    /// Codex app-server engine settings.
    pub codex: CodexConfig,
    /// Folder both engines get full access to (Codex: cwd + writable root; HTTP: exposed, no tools yet).
    pub workspace_root: PathBuf,
    /// How routes with `direct: true` reach the network: `auto` (default: the active Wi-Fi/Ethernet
    /// interface, bypassing a VPN, else the system path), `active-interface` (fail without one) or
    /// `system`. Override with `XIAODAI_HTTP_NETWORK`.
    pub http_network: HttpNetwork,
    /// Rolling latency log + Luna-first switching.
    pub health: HealthConfig,
}

pub fn default_http_network() -> HttpNetwork {
    std::env::var("XIAODAI_HTTP_NETWORK").ok().and_then(|v| v.parse().ok()).unwrap_or(HttpNetwork::Auto)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineMode {
    /// Codex first; HTTP race joins after `codex.fallback_after_ms` without visible text,
    /// or immediately if Codex errors / is unhealthy. First visible text wins.
    Dual,
    CodexOnly,
    HttpOnly,
}

impl std::str::FromStr for EngineMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "dual" => Ok(Self::Dual),
            "codex-only" | "codex" => Ok(Self::CodexOnly),
            "http-only" | "http" => Ok(Self::HttpOnly),
            _ => Err(format!("unknown mode '{s}' (dual | codex-only | http-only)")),
        }
    }
}

/// Barge-in policy for a follow-up utterance during an active Codex turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BargeIn {
    /// `turn/steer` (with `expectedTurnId`) into the running turn; no reconnect penalty.
    Steer,
    /// `turn/interrupt` the running turn, then a new turn on the same thread.
    Interrupt,
}

impl std::str::FromStr for BargeIn {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "steer" => Ok(Self::Steer),
            "interrupt" => Ok(Self::Interrupt),
            _ => Err(format!("unknown barge-in policy '{s}' (steer | interrupt)")),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    /// Executable to run as `<binary> app-server --stdio`. None = auto: `XIAODAI_CODEX_BIN`, else
    /// /usr/local/bin/codex (0.170), else the ChatGPT app's bundled codex (0.169), else
    /// ~/working/codex-cli's codex, else `codex` on PATH. gpt-6-luna needs codex 0.169+.
    pub binary: Option<PathBuf>,
    /// Requested model; matched exactly (case/space-insensitive) against `model/list`. Never substituted.
    pub model: String,
    /// Reasoning effort. None = the lowest effort the model advertises.
    pub effort: Option<String>,
    /// e.g. "priority". None = the account/thread default.
    pub service_tier: Option<String>,
    /// Developer instructions for every thread (spoken style).
    pub instructions: String,
    /// Dual mode: start the HTTP race if Codex has shown no visible text by then.
    pub fallback_after_ms: u64,
    /// Whole-turn ceiling for a Codex turn (it may run tools).
    pub turn_timeout_ms: u64,
    /// Ceiling for each JSON-RPC request (initialize, model/list, thread/start, ...).
    pub request_timeout_ms: u64,
    /// Run a tiny throwaway turn when the conversation thread is created so the first real turn
    /// skips the ~7-11 s per-thread session setup (MCP/tools/skills).
    pub prime_threads: bool,
    /// Start a fresh thread after this many turns. 0 (default) = never: the whole conversation
    /// stays in ONE thread, including after interrupts.
    pub max_turns_per_thread: u32,
    /// What a new utterance does while a Codex turn is still producing output:
    /// `steer` (default) adds it to the running turn with `turn/steer`; `interrupt` stops the
    /// turn and starts a new one (same thread).
    pub barge_in: BargeIn,
    /// Pass the binary's bundled model catalog (`codex debug models --bundled`, written to a temp
    /// file) as `-c model_catalog_json=...`, so the child never re-fetches the remote model list.
    /// That fetch times out after 5 s on this network and was retried on every turn.
    pub static_model_catalog: bool,
    /// Use this catalog file instead of the bundled one (implies a static catalog).
    pub model_catalog_json: Option<PathBuf>,
    /// Lean child: disable user MCP servers, plugins and apps for this child only
    /// (`-c` overrides; ~/.codex/config.toml is never modified). Built-in shell/file tools stay.
    pub lean: bool,
    /// Sandbox network access inside the workspace.
    pub network_access: bool,
    /// After the child dies / init fails, wait this long before trying to respawn.
    pub respawn_backoff_ms: u64,
    /// Let Luna hand heavier work to Grok Bot (MCP server + CLI in ~/grok-bot).
    pub grok_bot: GrokBotDelegate,
}

/// Grok Bot as a *delegate* of Luna (not a racing engine). When enabled, the core's own codex
/// child gets the Grok Bot MCP server via `-c mcp_servers.xiaodai_grok_bot.*` overrides (never
/// written to ~/.codex/config.toml) and the developer instructions gain a short note on when and
/// how to delegate (MCP tools first, the CLI as a fallback).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GrokBotDelegate {
    /// Default: on when the MCP server script exists (`XIAODAI_GROK_BOT=0` turns it off).
    pub enabled: bool,
    /// Node used to run the MCP server. None = /usr/local/bin/node, /opt/homebrew/bin/node, else `node`.
    pub node: Option<PathBuf>,
    pub mcp_server: PathBuf,
    pub cli: PathBuf,
    /// Codex's per-tool-call ceiling for the MCP server (ask_grok_bot_and_wait may wait minutes).
    pub tool_timeout_sec: u64,
    pub startup_timeout_sec: u64,
    /// Appended to `CodexConfig::instructions` when enabled.
    pub instructions: String,
}

pub const GROK_BOT_MCP_NAME: &str = "xiaodai_grok_bot";

pub const DEFAULT_GROK_BOT_INSTRUCTIONS: &str = "你可以把较重或需要联网的只读任务交给 Grok Bot（云端助手，能上网查资料、查天气、查地点和营业时间、看 X 上的公开帖子、总结网页、翻译），通常要1到3分钟才有结果。简单问题自己直接回答，不要委托。需要委托时：用 xiaodai_grok_bot 的 ask_grok_bot 工具提交一个清楚完整的任务，立刻用一句话告诉用户已经交给 Grok Bot、稍后告诉他；之后用户问起时，用 get_grok_bot_result 和 request_id 查看结果。只有用户愿意等的时候才用 ask_grok_bot_and_wait。没有 MCP 工具时，可以在终端运行 Grok Bot 命令行：`{cli} --no-wait \"任务\"` 提交，`{cli} --result <request_id>` 查看。Grok Bot 不发消息、不下单、不预订、不填表、不改文件或账户，不要让它做这些事。";

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/Users/jason"))
}

impl Default for GrokBotDelegate {
    fn default() -> Self {
        let mcp_server = home().join("grok-bot/mcp/server.mjs");
        let off = std::env::var("XIAODAI_GROK_BOT").is_ok_and(|v| v == "0" || v.eq_ignore_ascii_case("off") || v.eq_ignore_ascii_case("false"));
        Self {
            enabled: !off && mcp_server.is_file(),
            node: None,
            mcp_server,
            cli: home().join("grok-bot/cli/grokbot"),
            tool_timeout_sec: 330,
            startup_timeout_sec: 20,
            instructions: DEFAULT_GROK_BOT_INSTRUCTIONS.into(),
        }
    }
}

impl GrokBotDelegate {
    pub fn node_binary(&self) -> PathBuf {
        if let Some(n) = &self.node {
            return n.clone();
        }
        ["/usr/local/bin/node", "/opt/homebrew/bin/node"].iter().map(PathBuf::from).find(|p| p.is_file()).unwrap_or_else(|| PathBuf::from("node"))
    }

    /// `-c` overrides for the core's codex child only. Empty when disabled.
    pub fn codex_overrides(&self) -> Vec<String> {
        if !self.enabled {
            return Vec::new();
        }
        let q = |s: &str| serde_json::to_string(s).unwrap_or_default(); // JSON string == TOML basic string
        let n = GROK_BOT_MCP_NAME;
        let mut kv = vec![
            format!("mcp_servers.{n}.command={}", q(&self.node_binary().display().to_string())),
            format!("mcp_servers.{n}.args=[{}]", q(&self.mcp_server.display().to_string())),
            format!("mcp_servers.{n}.enabled=true"),
            format!("mcp_servers.{n}.startup_timeout_sec={}", self.startup_timeout_sec),
            format!("mcp_servers.{n}.tool_timeout_sec={}", self.tool_timeout_sec),
            format!("mcp_servers.{n}.default_tools_approval_mode=\"approve\""),
        ];
        for tool in ["ask_grok_bot", "get_grok_bot_result", "ask_grok_bot_and_wait"] {
            kv.push(format!("mcp_servers.{n}.tools.{tool}.approval_mode=\"approve\""));
        }
        kv.into_iter().flat_map(|v| ["-c".to_owned(), v]).collect()
    }

    /// The developer-instructions addition (empty when disabled).
    pub fn instructions_text(&self) -> String {
        if !self.enabled {
            return String::new();
        }
        self.instructions.replace("{cli}", &self.cli.display().to_string())
    }
}

impl CodexConfig {
    /// `instructions` plus the Grok Bot delegation note when enabled.
    pub fn effective_instructions(&self) -> String {
        let extra = self.grok_bot.instructions_text();
        if extra.is_empty() {
            self.instructions.clone()
        } else {
            format!("{}\n\n{}", self.instructions, extra)
        }
    }
}

pub const DEFAULT_CODEX_INSTRUCTIONS: &str = "你是机器人小呆的大脑，你的回答会被直接朗读出来。请用简体中文口语回答，通常只说1到3句短句，简洁自然。不要使用Markdown、列表、标题、表情符号、代码块或网址。只有在确实需要查看或修改小呆服务器文件夹时才调用工具。";

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            binary: None,
            model: "gpt-6-luna".into(),
            effort: None,
            service_tier: None,
            instructions: DEFAULT_CODEX_INSTRUCTIONS.into(),
            fallback_after_ms: 2000,
            turn_timeout_ms: 120_000,
            request_timeout_ms: 30_000,
            prime_threads: true,
            max_turns_per_thread: 0,
            barge_in: BargeIn::Steer,
            static_model_catalog: true,
            model_catalog_json: None,
            lean: true,
            network_access: true,
            respawn_backoff_ms: 10_000,
            grok_bot: GrokBotDelegate::default(),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            routes: vec![Route::glm_coding_plan(), Route::deepseek_flash(), Route::qwen_coding_plan(), Route::glm_coding_plan_no_think(), Route::glm_coding_plan_cn()],
            order: vec!["glm-coding-plan".into(), "deepseek-flash".into()],
            first_token_timeout_ms: 1200,
            request_timeout_ms: 30_000,
            max_tokens: 200,
            temperature: 0.7,
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            max_sentence_chars: 60,
            http_race_at_start: true,
            mode: EngineMode::Dual,
            codex: CodexConfig::default(),
            workspace_root: default_workspace_root(),
            http_network: default_http_network(),
            health: HealthConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_workspace_uses_english_root_and_preserves_overrides() {
        const CHILD: &str = "ESPBOT_CORE_ROOT_TEST_EXPECTED";
        if let Some(expected) = std::env::var_os(CHILD) {
            let expected = PathBuf::from(expected);
            assert_eq!(Config::default().workspace_root, expected);
            let from_json: Config = serde_json::from_value(json!({})).unwrap();
            assert_eq!(from_json.workspace_root, expected);
            let explicit: Config = serde_json::from_value(json!({"workspace_root": "/tmp/custom-bot"})).unwrap();
            assert_eq!(explicit.workspace_root, PathBuf::from("/tmp/custom-bot"));
            return;
        }
        // Isolate launch environments instead of mutating the test runner's
        // process-wide environment while other core tests are running.
        for (setting, expected) in [
            (None, "/Users/jason/Xiaodai_Server"),
            (Some(""), "/Users/jason/Xiaodai_Server"),
            (Some("/tmp/custom bot"), "/tmp/custom bot"),
        ] {
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "config::tests::default_workspace_uses_english_root_and_preserves_overrides"])
                .env(CHILD, expected).env_remove("XIAODAI_WORKSPACE_ROOT");
            if let Some(value) = setting { child.env("XIAODAI_WORKSPACE_ROOT", value); }
            let result = child.output().unwrap();
            assert!(result.status.success(), "{}{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
        }
    }
}

//! Conservative routing: a 429 is not a subscription balance, and an
//! interrupted tool-using turn is never replayed on a cheaper model.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::config::{HarnessConfig, ModelRoute};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    QuotaExhausted,
    RateLimited,
    Authentication,
    Configuration,
    Transport,
    Upstream,
    Cancelled,
}

impl FailureKind {
    pub fn code(self) -> &'static str {
        match self {
            Self::QuotaExhausted => "quota_exhausted",
            Self::RateLimited => "rate_limited",
            Self::Authentication => "provider_authentication",
            Self::Configuration => "provider_configuration",
            Self::Transport => "opencode_transport",
            Self::Upstream => "provider_error",
            Self::Cancelled => "turn_cancelled",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Failure {
    pub kind: FailureKind,
    pub status: Option<u16>,
    pub retry_after_ms: Option<u64>,
    /// Only a code-owned diagnostic, never an upstream response or credential.
    pub detail: &'static str,
}

impl Failure {
    pub fn transport(detail: &'static str) -> Self {
        Self { kind: FailureKind::Transport, status: None, retry_after_ms: None, detail }
    }

    pub fn public_message(&self) -> String {
        match self.status {
            Some(status) => format!("{}: {} (HTTP {status})", self.kind.code(), self.detail),
            None => format!("{}: {}", self.kind.code(), self.detail),
        }
    }

    pub fn permits_route_change(&self, config: &HarnessConfig) -> bool {
        self.kind == FailureKind::QuotaExhausted
            || (self.kind == FailureKind::RateLimited && config.fallback_on_rate_limit)
    }
}

/// Native v2 Session.StructuredError carries `type`, `message`, optional
/// `status`, `response.body`, and `retryAfterMs`. Match known quota semantics
/// before classifying a generic HTTP status; never infer balance from tokens.
pub(super) fn classify(error: &Value, route: &ModelRoute, http_status: Option<u16>) -> Failure {
    let status = error.get("status").and_then(Value::as_u64)
        .and_then(|v| u16::try_from(v).ok()).or(http_status);
    let response_body = error.pointer("/response/body").and_then(Value::as_str).unwrap_or("");
    let body: Value = serde_json::from_str(response_body).unwrap_or(Value::Null);
    let code = body.pointer("/error/code").or_else(|| body.get("code"))
        .or_else(|| error.pointer("/error/code")).or_else(|| error.get("code"))
        .and_then(|v| v.as_str().map(str::to_owned).or_else(|| v.as_i64().map(|n| n.to_string())))
        .unwrap_or_default().to_ascii_lowercase();
    // Bound classification work. This text is never returned, logged or persisted.
    let text = format!("{} {} {} {}",
        error.get("type").and_then(Value::as_str).unwrap_or(""),
        error.get("message").and_then(Value::as_str).unwrap_or(""),
        body.pointer("/error/message").or_else(|| body.get("message")).and_then(Value::as_str).unwrap_or(""),
        response_body.chars().take(16_384).collect::<String>(),
    ).to_lowercase();
    let temporary = text.contains("concurrency allocated quota exceeded")
        || text.contains("usage allocated quota exceeded");
    let quota = !temporary && (
        status == Some(402)
        || matches!(code.as_str(), "insufficient_quota" | "quota_exhausted" | "insufficient_balance")
        || route.quota_codes.iter().any(|known| !known.is_empty() && known.eq_ignore_ascii_case(&code))
        || ["hour allocated quota exceeded", "week allocated quota exceeded",
            "month allocated quota exceeded", "insufficient balance", "quota exhausted",
            "insufficient quota", "weekly usage limit reached", "monthly usage limit reached",
            "余额不足", "额度已用完", "套餐额度已用尽"]
            .iter().any(|phrase| text.contains(phrase))
    );
    let kind = if quota {
        FailureKind::QuotaExhausted
    } else if temporary || status == Some(429) {
        FailureKind::RateLimited
    } else if matches!(status, Some(401 | 403)) {
        FailureKind::Authentication
    } else if matches!(status, Some(400 | 404 | 422))
        || text.contains("modelnotfound") || text.contains("providernotfound")
    {
        FailureKind::Configuration
    } else {
        FailureKind::Upstream
    };
    Failure {
        kind, status,
        retry_after_ms: error.get("retryAfterMs").and_then(Value::as_u64)
            .map(|value| value.clamp(1_000, 604_800_000)),
        detail: match kind {
            FailureKind::QuotaExhausted => "provider reported exhausted quota or balance",
            FailureKind::RateLimited => "provider reported a temporary rate or concurrency limit",
            FailureKind::Authentication => "check the selected provider's credential and subscription endpoint",
            FailureKind::Configuration => "check provider, model, endpoint and request configuration",
            _ => "OpenCode reported an upstream failure; inspect its local diagnostics",
        },
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(super) struct RoutingState {
    pub routes: BTreeMap<String, RouteHealth>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RouteHealth {
    pub last_failure: FailureKind,
    pub failed_at_ms: u64,
    pub retry_at_ms: u64,
    /// A local backoff estimate; not an assertion that the plan resets here.
    pub reset_is_provider_hint: bool,
}

impl RoutingState {
    pub fn record(&mut self, id: &str, error: &Failure, config: &HarnessConfig) {
        let default_ms = match error.kind {
            FailureKind::QuotaExhausted => config.quota_cooldown_secs.saturating_mul(1_000),
            FailureKind::RateLimited => config.rate_limit_cooldown_secs.saturating_mul(1_000),
            _ => 0,
        };
        let at = super::now_ms();
        self.routes.insert(id.to_owned(), RouteHealth {
            last_failure: error.kind, failed_at_ms: at,
            retry_at_ms: at.saturating_add(error.retry_after_ms.unwrap_or(default_ms)),
            reset_is_provider_hint: error.retry_after_ms.is_some(),
        });
    }
}

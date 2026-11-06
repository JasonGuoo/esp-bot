//! Local data contracts. None of these types confers device or parent authority.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub max_profiles: usize,
    pub max_memories: usize,
    pub max_memory_chars: usize,
    pub context_max_chars: usize,
    pub context_max_items: usize,
    /// Transcript persistence is opt-in; conversation ownership metadata is kept
    /// separately so forgetting a person can also identify external sessions.
    pub history_enabled: bool,
    pub history_ttl_secs: u64,
    pub history_max_turns: usize,
    pub history_max_chars: usize,
    pub max_conversations: usize,
    pub min_enrollment_samples: usize,
    pub max_enrollment_samples: usize,
    /// Provisional cosine thresholds, NOT probabilities or a calibrated claim.
    pub face_threshold: f32,
    pub speaker_threshold: f32,
    pub match_margin: f32,
    pub face_enrollment_consistency: f32,
    pub speaker_enrollment_consistency: f32,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_profiles: 32,
            max_memories: 2_000,
            max_memory_chars: 2_000,
            context_max_chars: 3_000,
            context_max_items: 12,
            history_enabled: false,
            history_ttl_secs: 86_400,
            history_max_turns: 24,
            history_max_chars: 4_000,
            max_conversations: 4_096,
            min_enrollment_samples: 3,
            max_enrollment_samples: 12,
            face_threshold: 0.50,
            speaker_threshold: 0.70,
            match_margin: 0.08,
            face_enrollment_consistency: 0.30,
            speaker_enrollment_consistency: 0.50,
        }
    }
}

impl MemoryConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !(1..=256).contains(&self.max_profiles)
            || !(1..=20_000).contains(&self.max_memories)
            || !(32..=8_000).contains(&self.max_memory_chars)
            || !(256..=16_000).contains(&self.context_max_chars)
            || !(1..=64).contains(&self.context_max_items)
            || !(1..=256).contains(&self.history_max_turns)
            || !(32..=16_000).contains(&self.history_max_chars)
            || !(1..=50_000).contains(&self.max_conversations)
            || !(60..=31_536_000).contains(&self.history_ttl_secs)
            || !(3..=20).contains(&self.min_enrollment_samples)
            || self.max_enrollment_samples < self.min_enrollment_samples
            || self.max_enrollment_samples > 32
        {
            return Err("invalid memory capacity or retention limits".into());
        }
        for value in [
            self.face_threshold,
            self.speaker_threshold,
            self.face_enrollment_consistency,
            self.speaker_enrollment_consistency,
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err("identity cosine thresholds must be finite and between 0 and 1".into());
            }
        }
        if !self.match_margin.is_finite() || !(0.01..=0.5).contains(&self.match_margin) {
            return Err("identity match_margin must be between 0.01 and 0.5".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewProfile {
    pub display_name: String,
    #[serde(default = "yes")]
    pub is_child: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    pub display_name: String,
    pub is_child: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    Household,
    Profile,
}

impl MemoryScope {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Household => "household", Self::Profile => "profile" }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySource {
    GuardianNote,
    UserStatement,
    AssistantSuggestion,
}

impl MemorySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::GuardianNote => "guardian_note",
            Self::UserStatement => "user_statement",
            Self::AssistantSuggestion => "assistant_suggestion",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewMemory {
    #[serde(default)]
    pub id: Option<String>,
    pub scope: MemoryScope,
    #[serde(default)]
    pub profile_id: Option<String>,
    pub content: String,
    pub category: String,
    pub source: MemorySource,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    #[serde(default)]
    pub allow_cloud: bool,
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    pub profile_id: Option<String>,
    pub content: String,
    pub category: String,
    pub source: MemorySource,
    pub source_ref: Option<String>,
    pub confirmed: bool,
    pub allow_cloud: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub expires_at_ms: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextDestination { Local, Cloud }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContextRequest {
    #[serde(default)]
    pub profile_id: Option<String>,
    /// Supplied only by a trusted local profile selection, never by matching.
    #[serde(default)]
    pub profile_confirmed: bool,
    pub destination: ContextDestination,
    pub query: String,
    #[serde(default = "yes")]
    pub include_household: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemoryContext {
    pub text: String,
    pub memory_ids: Vec<String>,
    pub profile_id: Option<String>,
    pub requires_profile_confirmation: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole { User, Assistant }

impl TurnRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::User => "user", Self::Assistant => "assistant" }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TurnRecord {
    pub conversation_id: String,
    #[serde(default)]
    pub profile_id: Option<String>,
    pub role: TurnRole,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredTurn {
    pub id: i64,
    pub conversation_id: String,
    pub profile_id: Option<String>,
    pub role: TurnRole,
    pub content: String,
    pub at_ms: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityKind { Face, Speaker }

impl IdentityKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self { Self::Face => "face", Self::Speaker => "speaker" }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbeddingSample {
    pub embedding: Vec<f32>,
    /// Worker quality gates must pass; this is not a user-set confidence.
    #[serde(default)]
    pub quality_ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    pub profile_id: String,
    pub kind: IdentityKind,
    pub model_id: String,
    pub samples: Vec<EmbeddingSample>,
    /// Trusted parent-console confirmation of enrollment, not model output.
    #[serde(default)]
    pub guardian_confirmed: bool,
    #[serde(default)]
    pub replace_existing: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Enrollment {
    pub id: String,
    pub profile_id: String,
    pub kind: IdentityKind,
    pub model_id: String,
    pub dimension: usize,
    pub sample_count: usize,
    pub consent_at_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityRequest {
    pub kind: IdentityKind,
    pub model_id: String,
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub quality_ok: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus { Matched, Unknown, Ambiguous, LowQuality, NoEnrollment }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchResult {
    pub status: MatchStatus,
    /// A suggestion for the local console, never an authorization credential.
    pub profile_id: Option<String>,
    pub best_score: Option<f32>,
    pub runner_up_score: Option<f32>,
    pub threshold: f32,
    pub required_margin: f32,
    pub reason: String,
    pub authorizes_profile_memory: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FusedIdentity {
    /// `agreement`, `single_candidate`, `conflict`, or `unknown`.
    pub status: String,
    pub profile_id: Option<String>,
    pub reason: String,
    pub authorizes_profile_memory: bool,
}

fn yes() -> bool { true }

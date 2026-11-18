//! Source and projection contracts. Identity observations never grant access.
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GraphitiConfig {
    pub enabled: bool,
    pub python: Option<PathBuf>,
    pub script: Option<PathBuf>,
    /// A local, user-managed JSON configuration. It contains environment key
    /// names, not credentials; see graphiti_worker.py for the bounded schema.
    pub config_file: Option<PathBuf>,
    /// Remote extraction/embedding is additionally gated per source.
    pub allow_cloud: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalInput {
    pub turn_id: String,
    pub activation_id: String,
    #[serde(default)] pub utterance_id: Option<String>,
    /// Empty is permitted only for received/ASR-failed input metadata.
    #[serde(default)] pub text: String,
    pub occurred_at_ms: i64,
    pub input_kind: String,
    #[serde(default)] pub speaker_person_id: Option<String>,
    #[serde(default)] pub private_profile_id: Option<String>,
    #[serde(default)] pub allow_cloud: bool,
    #[serde(default)] pub metadata: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalReceipt {
    pub household_id: String,
    pub daily_conversation_id: String,
    pub episode_id: String,
    pub conversation_turn_id: String,
    pub utterance_id: String,
    pub source_revision: u64,
    pub retained: bool,
    pub policy_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HouseholdJob {
    pub id: String,
    pub lease: String,
    pub kind: String,
    pub household_id: String,
    pub episode_id: String,
    pub input_version: String,
    pub payload: Value,
    pub allow_cloud: bool,
}

/// Every derived item must cite exact text from one or more input sources.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceQuote {
    pub conversation_turn_id: String,
    pub source_revision: u64,
    pub quote: String,
    #[serde(default)] pub speaker_segment_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LearningEvidence {
    pub knowledge_point: String,
    pub question: String,
    pub answer: String,
    /// none / provided / unknown, never an inferred mastery percentage.
    pub hint_level: String,
    pub question_source_id: String,
    pub answer_source_id: String,
    #[serde(default)] pub question_receipt_key: Option<String>,
    #[serde(default)] pub hint_source_id: Option<String>,
    #[serde(default)] pub hint_quote: Option<String>,
    #[serde(default)] pub prior_answer_source_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HouseholdItem {
    /// event / topic / learning / fact. All are observations, not instructions.
    pub kind: String,
    pub content: String,
    #[serde(default)] pub subject_entity_ids: Vec<String>,
    #[serde(default)] pub topic_key: Option<String>,
    #[serde(default)] pub evidence: Option<String>,
    #[serde(default)] pub assertion_kind: Option<String>,
    #[serde(default)] pub learning: Option<LearningEvidence>,
    /// Optional date of an event/fact: `YYYY-MM-DD`, or `--MM-DD` for a yearly date
    /// such as a birthday. Anything else is ignored.
    #[serde(default)] pub when: Option<String>,
    pub sources: Vec<SourceQuote>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HouseholdExtraction {
    #[serde(default)] pub items: Vec<HouseholdItem>,
}

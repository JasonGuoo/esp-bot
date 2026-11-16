//! Host IDs and versions are distinct from the ESP runtime's numeric turn token.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CONTRACT_VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub household_id: String,
    pub daily_conversation_id: String,
    pub episode_id: String,
    pub conversation_turn_id: String,
    pub utterance_id: String,
    pub source_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeBinding {
    pub activation_id: String,
    pub conversation_turn_id: String,
    pub runtime: Value,
    pub output_lease_id: String,
}

pub fn capabilities() -> Value {
    json!({"contract_version":CONTRACT_VERSION,"household_journal":1,"source_revisions":1,
        "parallel_asr_identity":1,"tasks":1,"presentation":1,"segment_receipts":1,
        "diarization":"optional_local_model","graphiti":"optional_local_adapter",
        "aec":"device_negotiated","side_effects_require_receipts":true})
}

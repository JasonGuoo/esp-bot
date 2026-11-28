//! Host authority is separate from model-supplied business parameters. These
//! envelopes are internal; they do not widen a device's WSS runtime token.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use super::contracts::SourceRef;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionProposal {
    pub capability_id: String,
    pub parameters: Value,
    #[serde(default)] pub target_handle: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ForegroundRef {
    pub connection_epoch: String, pub activation_id: String, pub runtime_token: Value,
    pub turn_id: String, pub generation: u64, pub input_revision: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct OperationRef { pub operation_id: String, pub operation_revision: u64, pub task_id: Option<String> }
#[derive(Clone, Debug, Serialize)]
pub struct TrustedActionContext {
    pub request_id: String, pub service_epoch: String, pub source: SourceRef,
    /// Private scope is supplied only by the trusted console; a speaker label
    /// or a self-introduction never selects it.
    pub private_profile_id: Option<String>, pub policy_epoch: u64,
    pub snapshot_version: u64, pub catalog_version: u32,
    pub foreground: Option<ForegroundRef>, pub operation: Option<OperationRef>,
    pub output_lease_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all="snake_case")]
pub enum ReceiptStage { ReadResult, AcceptedDurable, Applied, Failed, Unknown }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionReceipt {
    pub capability_id: String, pub stage: ReceiptStage, pub operation_id: Option<String>,
    pub result: Value, pub as_of_ms: i64,
}

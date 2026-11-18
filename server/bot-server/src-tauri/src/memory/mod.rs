//! 小呆 local memory and enrolled identity store.
//!
//! The Tauri parent console is the writer. The harness receives only bounded,
//! explicitly confirmed text through `Store::context`; it never gets templates.
//! Matching supplies a candidate and cannot authorize profile memory or wake.

mod identity;
mod voice_album;
pub(crate) use voice_album::VoiceSample;
mod store;
mod types;
mod household_types;
mod household;
mod household_jobs;
mod profile_merge;
mod provenance;
pub mod graphiti;

pub use identity::fuse_identity;
pub use store::{Store, ExtractedFact};
pub use types::*;
pub use household_types::*;
pub use provenance::household_source_ref;
pub use household::TurnSpeaker;

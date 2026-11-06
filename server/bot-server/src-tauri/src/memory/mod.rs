//! 小呆 local memory and enrolled identity store.
//!
//! The Tauri parent console is the writer. The harness receives only bounded,
//! explicitly confirmed text through `Store::context`; it never gets templates.
//! Matching supplies a candidate and cannot authorize profile memory or wake.

mod identity;
mod store;
mod types;

pub use identity::fuse_identity;
pub use store::Store;
pub use types::*;

//! Fast responder core: text in, streamed spoken-length sentences out.
//!
//! Minimal API:
//! ```ignore
//! let core = Core::new(Config::default())?;
//! core.warmup().await;
//! let cancel = CancellationToken::new();
//! let mut turn = core.respond("你好", cancel.clone());
//! while let Some(ev) = turn.next().await { /* FirstToken / Sentence / Done / Cancelled / Error */ }
//! ```
//! No memory, ASR, TTS, tools or device protocol here: a brain wraps this.

mod client;
pub mod codex;
pub mod config;
mod core;
pub mod health;
pub mod net;
pub mod sse;
pub mod splitter;

pub use crate::codex::{CodexEngine, CodexWarmup, InputStats, InterruptStats, SteerStats, TurnText};
pub use crate::config::{BargeIn, CodexConfig, Config, EngineMode, GrokBotDelegate, Route, DEFAULT_WORKSPACE_ROOT};
pub use crate::health::{EngineStats, HealthConfig, HealthSnapshot, LunaState, Sample, SampleOutcome};
pub use crate::net::{ActiveInterface, HttpNetwork};
pub use crate::core::{AttemptTiming, Core, CoreError, Event, Message, Timings, Turn, WarmupResult};
pub use tokio_util::sync::CancellationToken;

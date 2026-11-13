//! Monotonic, content-free timing around the existing foreground operations.
//! These are Mac processing durations, not device speech-to-visible latency.

use std::{collections::BTreeMap, future::Future, time::Instant};

use serde::Serialize;
use serde_json::json;

use super::Brain;

#[derive(Clone, Debug, Default, Serialize)]
pub struct TurnTimings {
    pub total_ms: u64,
    pub stages_ms: BTreeMap<String, u64>,
    pub skipped: BTreeMap<String, String>,
    pub input_audio_ms: Option<u64>,
}

struct StageTimer<'a> {
    brain: &'a Brain,
    turn_id: &'a str,
    stage: &'static str,
    started: Instant,
    finished: bool,
}

impl StageTimer<'_> {
    fn finish(&mut self, status: &str) -> u64 {
        let elapsed_ms = self.started.elapsed().as_millis() as u64;
        self.finished = true;
        self.brain.emit("turn.stage_completed", Some(self.turn_id), json!({
            "stage":self.stage, "elapsed_ms":elapsed_ms, "status":status
        }));
        elapsed_ms
    }
}

impl Drop for StageTimer<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Cancellation drops the awaited future. Still identify the stage
            // that was interrupted, without retaining input or model output.
            self.finish("interrupted");
        }
    }
}

impl Brain {
    pub(super) async fn timed_stage<T>(
        &self, turn_id: &str, stage: &'static str, timings: &mut TurnTimings,
        work: impl Future<Output = Result<T, String>>,
    ) -> Result<T, String> {
        let mut timer = StageTimer { brain:self, turn_id, stage, started:Instant::now(), finished:false };
        self.emit("turn.stage_started", Some(turn_id), json!({"stage":stage}));
        let result = work.await;
        let elapsed_ms = timer.finish(if result.is_ok() { "completed" } else { "failed" });
        timings.stages_ms.insert(stage.to_owned(), elapsed_ms);
        result
    }

    pub(super) fn skip_stage(&self, turn_id: &str, stage: &'static str, reason: &'static str, timings: &mut TurnTimings) {
        timings.skipped.insert(stage.to_owned(), reason.to_owned());
        self.emit("turn.stage_skipped", Some(turn_id), json!({"stage":stage,"reason":reason}));
    }
}

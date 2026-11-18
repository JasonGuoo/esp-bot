//! Rolling per-stage latency (p50/p95) against the responsiveness budgets of
//! docs/review-ux-2026-11-18.md §2. Content-free; device figures are measured
//! from the device's own end-of-speech (`vad_end_ms`) to its first codec write.

use std::{collections::{BTreeMap, VecDeque}, sync::Mutex};
use serde_json::{json, Value};

const WINDOW: usize = 200;

/// (metric, label, p50 budget, p95 budget). `None`: tracked without a budget.
pub const METRICS: &[(&str, &str, Option<u64>, Option<u64>)] = &[
    ("first_sound_ms", "end of speech → first sound (ack or answer)", Some(700), None),
    ("first_word_ms", "end of speech → first real word", Some(1500), Some(2500)),
    ("asr_ms", "speech recognition", None, None),
    ("memory_context_ms", "memory context ready", None, None),
    ("first_sentence_ms", "turn start → core first sentence", None, None),
    ("first_tts_audio_ms", "turn start → first synthesized sentence", None, None),
];

#[derive(Default)]
pub struct Latency { samples: Mutex<BTreeMap<&'static str, VecDeque<u64>>> }

fn percentile(sorted: &[u64], p: f64) -> u64 {
    // Nearest-rank, so p95 of 20 samples is the 19th.
    let rank = ((p / 100.0) * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

impl Latency {
    pub fn record(&self, metric: &str, ms: u64) {
        let Some((name, ..)) = METRICS.iter().find(|m| m.0 == metric) else { return };
        if ms > 600_000 { return; }
        if let Ok(mut samples) = self.samples.lock() {
            let window = samples.entry(name).or_default();
            if window.len() == WINDOW { window.pop_front(); }
            window.push_back(ms);
        }
    }

    pub fn snapshot(&self) -> Value {
        let samples = self.samples.lock().map(|s| s.clone()).unwrap_or_default();
        let metrics: Vec<Value> = METRICS.iter().map(|(name, label, p50_budget, p95_budget)| {
            let mut sorted: Vec<u64> = samples.get(name).map(|w| w.iter().copied().collect()).unwrap_or_default();
            sorted.sort_unstable();
            let (p50, p95) = if sorted.is_empty() { (None, None) } else { (Some(percentile(&sorted, 50.0)), Some(percentile(&sorted, 95.0))) };
            let within = match (p50, p95) {
                (Some(a), Some(b)) => Some(p50_budget.is_none_or(|x| a <= x) && p95_budget.is_none_or(|x| b <= x)),
                _ => None,
            };
            json!({"metric":name,"label":label,"samples":sorted.len(),"p50_ms":p50,"p95_ms":p95,
                "budget_p50_ms":p50_budget,"budget_p95_ms":p95_budget,"within_budget":if p50_budget.is_some()||p95_budget.is_some(){within}else{None}})
        }).collect();
        json!({"window":WINDOW,"metrics":metrics,
            "not_measured":["first visual reaction (< 150 ms budget) is local to the device and not reported to the Mac"]})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_and_budgets_over_a_rolling_window() {
        let latency = Latency::default();
        for ms in 1..=20 { latency.record("first_word_ms", ms * 100); }
        latency.record("unknown_metric", 5);
        let snapshot = latency.snapshot();
        let word = snapshot["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "first_word_ms").unwrap().clone();
        assert_eq!((word["samples"].as_u64(), word["p50_ms"].as_u64(), word["p95_ms"].as_u64()), (Some(20), Some(1000), Some(1900)));
        assert_eq!(word["within_budget"], true);
        latency.record("first_word_ms", 9000);
        for _ in 0..10 { latency.record("first_word_ms", 3000); }
        let snapshot = latency.snapshot();
        let word = snapshot["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "first_word_ms").unwrap().clone();
        assert_eq!(word["within_budget"], false, "p95 over 2.5 s");
        for _ in 0..(WINDOW + 5) { latency.record("asr_ms", 1); }
        let asr = latency.snapshot()["metrics"].as_array().unwrap().iter().find(|m| m["metric"] == "asr_ms").unwrap().clone();
        assert_eq!(asr["samples"].as_u64(), Some(WINDOW as u64));
        assert!(asr["within_budget"].is_null());
    }
}

//! Per-engine rolling latency log + the Luna-first policy.
//!
//! Every attempt the core starts ends as one `Sample` (time to first visible text, or why not),
//! kept in a rolling window per engine key ("codex" for Luna, else the HTTP route id) and appended
//! as JSON lines to a log file (default `<crate>/runtime/latency.jsonl`).
//!
//! Luna-first (dual mode only): Luna leads while healthy, exactly as before. Luna becomes
//! *degraded* when it has been
//! - unavailable: `fail_threshold` failures/timeouts in a row, or still failing
//!   `unavailable_for_ms` after the first failure of the streak; or
//! - slow: over the last `window` samples (at least `min_samples`), more than half took longer
//!   than `slow_ttft_ms` to speak or lost the race (HTTP spoke first).
//!
//! While degraded the HTTP routes lead at t=0, fastest recent route first, and Luna is not used
//! for user turns. A background probe (one tiny turn on a separate throwaway Codex thread every
//! `probe_interval_ms`) checks Luna; `recover_probes` fast probes in a row switch back.
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const LUNA: &str = "codex";

pub fn default_log_path() -> Option<PathBuf> {
    match std::env::var_os("XIAODAI_LATENCY_LOG") {
        Some(v) if v.is_empty() => None,
        Some(v) => Some(PathBuf::from(v)),
        None => Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runtime").join("latency.jsonl")),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HealthConfig {
    /// Luna-first switching (dual mode). The latency log is kept either way.
    pub enabled: bool,
    /// Rolling window per engine.
    pub window: usize,
    /// Luna samples needed before "slow" can trigger.
    pub min_samples: usize,
    /// Luna counts as slow above this time to first text (ms from its start).
    pub slow_ttft_ms: u64,
    /// Consecutive Luna failures/timeouts that make it unavailable.
    pub fail_threshold: u32,
    /// ...or a failure streak that has lasted this long.
    pub unavailable_for_ms: u64,
    /// While degraded: probe Luna this often.
    pub probe_interval_ms: u64,
    /// Ceiling for one probe turn.
    pub probe_timeout_ms: u64,
    /// Fast probes in a row needed to switch back to Luna.
    pub recover_probes: u32,
    /// Ignore samples older than this (also when loading the log at startup).
    pub max_sample_age_ms: u64,
    /// JSON-lines log. None = memory only. Default `<crate>/runtime/latency.jsonl`
    /// (override with `XIAODAI_LATENCY_LOG`, empty = off).
    pub log_path: Option<PathBuf>,
    /// Rotate the log to `<name>.1` beyond this size.
    pub log_max_bytes: u64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            window: 6,
            min_samples: 3,
            slow_ttft_ms: 3000,
            fail_threshold: 2,
            unavailable_for_ms: 30_000,
            probe_interval_ms: 20_000,
            probe_timeout_ms: 20_000,
            recover_probes: 2,
            max_sample_age_ms: 600_000,
            log_path: default_log_path(),
            log_max_bytes: 5 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleOutcome {
    /// Spoke; `ttft_ms` set.
    Ok,
    /// Cancelled before speaking because another engine spoke first (`elapsed_ms` is a lower bound).
    Lost,
    Failed,
    Timeout,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    /// Unix ms.
    pub at_ms: u64,
    /// "codex" for Luna, else the HTTP route id.
    pub key: String,
    /// Display route ("codex/gpt-6-luna", "deepseek-flash").
    pub route: String,
    pub outcome: SampleOutcome,
    /// Time to first visible text, from this engine's own start.
    pub ttft_ms: Option<u64>,
    /// From this engine's start to its end/cancel.
    pub elapsed_ms: u64,
    #[serde(default)]
    pub probe: bool,
    /// "en0" (direct) / "system" for HTTP routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Sample {
    pub fn is_failure(&self) -> bool {
        matches!(self.outcome, SampleOutcome::Failed | SampleOutcome::Timeout)
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum LunaState {
    Preferred,
    Degraded { reason: String, since_ms: u64 },
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineStats {
    pub key: String,
    pub route: String,
    pub samples: usize,
    pub ok: usize,
    pub lost: usize,
    pub failures: usize,
    pub median_ttft_ms: Option<u64>,
    pub last_ttft_ms: Option<u64>,
    pub last_outcome: Option<SampleOutcome>,
    pub consecutive_failures: u32,
    /// Ranking score used to pick the leading HTTP route (lower = faster). None = no data.
    pub score_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Switch {
    pub at_ms: u64,
    pub to: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HealthSnapshot {
    pub luna: LunaState,
    pub probing: bool,
    pub probe_ok_streak: u32,
    pub engines: Vec<EngineStats>,
    pub switches: Vec<Switch>,
    pub log_path: Option<PathBuf>,
}

#[derive(Default)]
struct State {
    windows: HashMap<String, VecDeque<Sample>>,
    fail_streak: HashMap<String, (u32, u64)>,
    luna: Option<(String, u64)>,
    probe_ok: u32,
    switches: VecDeque<Switch>,
}

pub struct Health {
    cfg: HealthConfig,
    state: Mutex<State>,
    pub(crate) probing: AtomicBool,
}

const FAIL_PENALTY_MS: u64 = 30_000;
/// A lost race only bounds the time from below (racers start together), so it ranks a bit behind
/// the winner's time.
const LOST_PENALTY_MS: u64 = 50;

impl Health {
    pub fn new(cfg: HealthConfig) -> Self {
        let h = Self { cfg, state: Mutex::new(State::default()), probing: AtomicBool::new(false) };
        h.load();
        h
    }

    pub fn config(&self) -> &HealthConfig {
        &self.cfg
    }

    /// Seed the windows from the tail of the log (recent samples only).
    fn load(&self) {
        let Some(p) = &self.cfg.log_path else { return };
        let Ok(text) = std::fs::read_to_string(p) else { return };
        let cutoff = now_ms().saturating_sub(self.cfg.max_sample_age_ms);
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(2000);
        for l in &lines[start..] {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(l) else { continue };
            if v["type"] != "sample" {
                continue;
            }
            let Ok(s) = serde_json::from_value::<Sample>(v["sample"].clone()) else { continue };
            if s.at_ms >= cutoff {
                self.apply(s, false);
            }
        }
    }

    fn append(&self, v: serde_json::Value) {
        let Some(p) = &self.cfg.log_path else { return };
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if std::fs::metadata(p).map(|m| m.len() > self.cfg.log_max_bytes).unwrap_or(false) {
            let mut old = p.clone().into_os_string();
            old.push(".1");
            let _ = std::fs::rename(p, old);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{v}");
        }
    }

    /// Record one finished attempt (or probe) and re-evaluate the Luna policy.
    pub fn record(&self, s: Sample) {
        self.append(serde_json::json!({ "type": "sample", "sample": s }));
        self.apply(s, true);
    }

    fn apply(&self, s: Sample, log_switch: bool) {
        let mut switched: Option<Switch> = None;
        {
            let mut st = self.state.lock().unwrap();
            let key = s.key.clone();
            // Failure streaks.
            let e = st.fail_streak.entry(key.clone()).or_insert((0, 0));
            if s.is_failure() {
                if e.0 == 0 {
                    e.1 = s.at_ms;
                }
                e.0 += 1;
            } else if s.outcome == SampleOutcome::Ok {
                *e = (0, 0);
            }
            if !s.probe {
                let w = st.windows.entry(key.clone()).or_default();
                w.push_back(s.clone());
                while w.len() > self.cfg.window.max(1) {
                    w.pop_front();
                }
            }
            if key == LUNA {
                if s.probe {
                    let fast = s.outcome == SampleOutcome::Ok && s.ttft_ms.is_some_and(|t| t <= self.cfg.slow_ttft_ms);
                    st.probe_ok = if fast { st.probe_ok + 1 } else { 0 };
                    if st.luna.is_some() && st.probe_ok >= self.cfg.recover_probes.max(1) {
                        st.luna = None;
                        st.probe_ok = 0;
                        st.windows.remove(LUNA);
                        st.fail_streak.remove(LUNA);
                        switched = Some(Switch { at_ms: s.at_ms, to: "luna".into(), reason: format!("probe ok ({} ms)", s.ttft_ms.unwrap_or(0)) });
                    }
                } else if st.luna.is_none() {
                    if let Some(reason) = self.degrade_reason(&st, s.at_ms) {
                        st.luna = Some((reason.clone(), s.at_ms));
                        st.probe_ok = 0;
                        switched = Some(Switch { at_ms: s.at_ms, to: "fastest".into(), reason });
                    }
                }
            }
            if let Some(sw) = &switched {
                st.switches.push_back(sw.clone());
                while st.switches.len() > 20 {
                    st.switches.pop_front();
                }
            }
        }
        if let (Some(sw), true) = (switched, log_switch) {
            self.append(serde_json::json!({ "type": "switch", "switch": sw }));
        }
    }

    fn degrade_reason(&self, st: &State, now: u64) -> Option<String> {
        let c = &self.cfg;
        if let Some(&(n, since)) = st.fail_streak.get(LUNA) {
            if n >= c.fail_threshold.max(1) {
                return Some(format!("unavailable: {n} failures in a row"));
            }
            if n >= 1 && now.saturating_sub(since) >= c.unavailable_for_ms {
                return Some(format!("unavailable: failing for {} ms", now - since));
            }
        }
        let cutoff = now.saturating_sub(c.max_sample_age_ms);
        let w: Vec<&Sample> = st.windows.get(LUNA)?.iter().filter(|s| s.at_ms >= cutoff && !s.is_failure()).collect();
        if w.len() >= c.min_samples.max(1) {
            let slow = w.iter().filter(|s| s.outcome == SampleOutcome::Lost || s.ttft_ms.is_some_and(|t| t > c.slow_ttft_ms)).count();
            if slow * 2 > w.len() {
                return Some(format!("slow: {slow}/{} recent turns over {} ms or beaten by HTTP", w.len(), c.slow_ttft_ms));
            }
        }
        None
    }

    /// Some(reason) while Luna is degraded (and the policy is enabled).
    pub fn luna_degraded(&self) -> Option<String> {
        if !self.cfg.enabled {
            return None;
        }
        self.state.lock().unwrap().luna.as_ref().map(|(r, _)| r.clone())
    }

    pub fn luna_state(&self) -> LunaState {
        match (self.cfg.enabled, &self.state.lock().unwrap().luna) {
            (true, Some((reason, since))) => LunaState::Degraded { reason: reason.clone(), since_ms: *since },
            _ => LunaState::Preferred,
        }
    }

    fn score(&self, w: &VecDeque<Sample>, now: u64) -> Option<u64> {
        let cutoff = now.saturating_sub(self.cfg.max_sample_age_ms);
        let mut v: Vec<u64> = w
            .iter()
            .filter(|s| s.at_ms >= cutoff)
            .map(|s| match s.outcome {
                SampleOutcome::Ok => s.ttft_ms.unwrap_or(s.elapsed_ms),
                SampleOutcome::Lost => s.elapsed_ms + LOST_PENALTY_MS,
                _ => FAIL_PENALTY_MS,
            })
            .collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        Some(v[v.len() / 2])
    }

    /// `ids` sorted fastest-recent first (median of the window; failures count as 30 s, a lost race
    /// counts as its lower bound + 50 ms). Routes without recent data keep their configured order, after
    /// routes with data.
    pub fn rank(&self, ids: &[String]) -> Vec<String> {
        let st = self.state.lock().unwrap();
        let now = now_ms();
        let mut v: Vec<(usize, Option<u64>, String)> =
            ids.iter().enumerate().map(|(i, id)| (i, st.windows.get(id).and_then(|w| self.score(w, now)), id.clone())).collect();
        v.sort_by_key(|(i, s, _)| (s.is_none(), s.unwrap_or(0), *i));
        v.into_iter().map(|(_, _, id)| id).collect()
    }

    pub fn snapshot(&self) -> HealthSnapshot {
        let luna = self.luna_state();
        let st = self.state.lock().unwrap();
        let now = now_ms();
        let mut engines: Vec<EngineStats> = st
            .windows
            .iter()
            .map(|(k, w)| {
                let mut ttfts: Vec<u64> = w.iter().filter_map(|s| s.ttft_ms).collect();
                ttfts.sort_unstable();
                EngineStats {
                    key: k.clone(),
                    route: w.back().map(|s| s.route.clone()).unwrap_or_default(),
                    samples: w.len(),
                    ok: w.iter().filter(|s| s.outcome == SampleOutcome::Ok).count(),
                    lost: w.iter().filter(|s| s.outcome == SampleOutcome::Lost).count(),
                    failures: w.iter().filter(|s| s.is_failure()).count(),
                    median_ttft_ms: (!ttfts.is_empty()).then(|| ttfts[ttfts.len() / 2]),
                    last_ttft_ms: w.back().and_then(|s| s.ttft_ms),
                    last_outcome: w.back().map(|s| s.outcome),
                    consecutive_failures: st.fail_streak.get(k).map(|x| x.0).unwrap_or(0),
                    score_ms: self.score(w, now),
                }
            })
            .collect();
        engines.sort_by(|a, b| (a.key != LUNA, &a.key).cmp(&(b.key != LUNA, &b.key)));
        HealthSnapshot {
            luna,
            probing: self.probing.load(Ordering::SeqCst),
            probe_ok_streak: st.probe_ok,
            engines,
            switches: st.switches.iter().cloned().collect(),
            log_path: self.cfg.log_path.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HealthConfig {
        HealthConfig { log_path: None, ..HealthConfig::default() }
    }
    fn s(key: &str, outcome: SampleOutcome, ttft: Option<u64>, elapsed: u64, probe: bool) -> Sample {
        Sample { at_ms: now_ms(), key: key.into(), route: key.into(), outcome, ttft_ms: ttft, elapsed_ms: elapsed, probe, network: None, error: None }
    }

    #[test]
    fn two_failures_degrade_and_two_fast_probes_recover() {
        let h = Health::new(cfg());
        h.record(s(LUNA, SampleOutcome::Failed, None, 5, false));
        assert!(h.luna_degraded().is_none());
        h.record(s(LUNA, SampleOutcome::Failed, None, 5, false));
        assert!(h.luna_degraded().unwrap().starts_with("unavailable"));
        h.record(s(LUNA, SampleOutcome::Ok, Some(900), 900, true));
        assert!(h.luna_degraded().is_some(), "one probe is not enough");
        h.record(s(LUNA, SampleOutcome::Ok, Some(5000), 5000, true));
        h.record(s(LUNA, SampleOutcome::Ok, Some(900), 900, true));
        assert!(h.luna_degraded().is_some(), "a slow probe resets the streak");
        h.record(s(LUNA, SampleOutcome::Ok, Some(800), 800, true));
        assert!(h.luna_degraded().is_none());
        assert_eq!(h.snapshot().switches.len(), 2);
    }

    #[test]
    fn a_long_failure_streak_degrades_before_the_count_threshold() {
        let h = Health::new(HealthConfig { fail_threshold: 5, unavailable_for_ms: 1000, ..cfg() });
        let mut first = s(LUNA, SampleOutcome::Timeout, None, 20_000, false);
        first.at_ms -= 2000;
        h.record(first);
        assert!(h.luna_degraded().is_none(), "the streak is only measured from the next sample");
        h.record(s(LUNA, SampleOutcome::Failed, None, 5, false));
        assert!(h.luna_degraded().unwrap().starts_with("unavailable: failing for"));
    }

    #[test]
    fn mostly_slow_or_lost_turns_degrade() {
        let h = Health::new(cfg());
        h.record(s(LUNA, SampleOutcome::Ok, Some(1500), 1500, false));
        h.record(s(LUNA, SampleOutcome::Lost, None, 2600, false));
        assert!(h.luna_degraded().is_none(), "below min_samples");
        h.record(s(LUNA, SampleOutcome::Ok, Some(1400), 1400, false));
        assert!(h.luna_degraded().is_none(), "1 of 3 slow");
        h.record(s(LUNA, SampleOutcome::Ok, Some(4000), 4000, false));
        assert!(h.luna_degraded().is_none(), "2 of 4 is not more than half");
        h.record(s(LUNA, SampleOutcome::Lost, None, 2500, false));
        assert!(h.luna_degraded().unwrap().starts_with("slow"));
    }

    #[test]
    fn disabled_policy_never_degrades() {
        let h = Health::new(HealthConfig { enabled: false, ..cfg() });
        for _ in 0..5 {
            h.record(s(LUNA, SampleOutcome::Timeout, None, 120_000, false));
        }
        assert!(h.luna_degraded().is_none());
        assert_eq!(h.luna_state(), LunaState::Preferred);
    }

    #[test]
    fn ranking_prefers_fast_routes_and_keeps_order_without_data() {
        let h = Health::new(cfg());
        h.record(s("glm", SampleOutcome::Lost, None, 700, false));
        h.record(s("ds", SampleOutcome::Ok, Some(600), 600, false));
        h.record(s("qwen", SampleOutcome::Failed, None, 10, false));
        let ids: Vec<String> = ["glm", "ds", "new", "qwen"].iter().map(|x| x.to_string()).collect();
        assert_eq!(h.rank(&ids), vec!["ds", "glm", "qwen", "new"]);
    }

    #[test]
    fn log_round_trips_and_seeds_the_window() {
        // Test writes go only under the agreed test folder.
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let dir = home.join("小呆 Server/runtime/tmp/xiaodai-core-test").join(format!("health-unit-{}", std::process::id()));
        let p = dir.join("latency.jsonl");
        let _ = std::fs::remove_file(&p);
        let c = HealthConfig { log_path: Some(p.clone()), ..HealthConfig::default() };
        let h = Health::new(c.clone());
        h.record(s(LUNA, SampleOutcome::Failed, None, 5, false));
        h.record(s(LUNA, SampleOutcome::Timeout, None, 5, false));
        h.record(s("ds", SampleOutcome::Ok, Some(500), 500, false));
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().filter(|l| l.contains("\"type\":\"sample\"")).count(), 3);
        assert_eq!(text.lines().filter(|l| l.contains("\"type\":\"switch\"")).count(), 1);
        let h2 = Health::new(c);
        assert!(h2.luna_degraded().is_some(), "state rebuilt from the log");
        assert_eq!(h2.snapshot().engines.iter().find(|e| e.key == "ds").unwrap().median_ttft_ms, Some(500));
        let _ = std::fs::remove_dir_all(dir);
    }
}

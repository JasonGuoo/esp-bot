//! Short in-character lines 小呆 says instead of a technical error. The exact
//! error code stays in the console journal and the device log.

const RETRY: &[(&str, &str)] = &[
    ("我没听清，再说一遍好吗？", "confused"),
    ("咦，刚才好像没听到，再说一次吧。", "curious"),
    ("嗯？声音有点小，再说一遍呀。", "curious"),
    ("我刚刚走神了，你再说一次好吗？", "shy"),
];
const OOPS: &[(&str, &str)] = &[
    ("哎呀，我脑袋卡住了，再说一遍好吗？", "confused"),
    ("糟糕，我刚才没想出来，再问我一次吧。", "shy"),
    ("咦，我的大脑打了个结，我们再试一次。", "confused"),
    ("不好意思，我刚才没接上，再来一次吧。", "shy"),
];
/// Said after part of an answer was already spoken and the rest failed.
const PARTIAL: &[&str] = &[
    "哎呀，后面的我一下子想不起来了，等会儿再讲给你听好吗？",
    "咦，我脑袋卡住了，先说到这里吧。",
    "糟糕，我忘词了，你可以再问我一次哦。",
];
const PARTIAL_HANDOFF: &str = "后面的要请更厉害的助手帮忙，你可以再问我一次哦。";

fn pick<T: Copy>(pool: &[T], seed: &str) -> T {
    // FNV-1a: stable per turn, varied across turns, no RNG dependency.
    let hash = seed.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3));
    pool[(hash % pool.len() as u64) as usize]
}

/// The line for a failed device turn, or `None` when the failure should stay a
/// caption (cancellation, busy, permissions, configuration, stale tokens).
pub fn failure_line(error: &str, seed: &str) -> Option<(&'static str, &'static str)> {
    let code = error.split(':').next().unwrap_or("").trim();
    if matches!(code, "asr_no_speech" | "no_speech" | "audio_too_quiet" | "audio_too_short") {
        return Some(pick(RETRY, seed));
    }
    // Repeating the question cannot fix missing credentials or unavailable
    // providers. Do not imply that we heard nothing or merely forgot the answer.
    if code == "all_routes_unavailable" {
        return Some(("我需要的服务现在不可用，请在 Mac 上检查模型设置。", "confused"));
    }
    let transient = code.starts_with("brain_core") || code.starts_with("core_")
        || code.starts_with("harness") || code.starts_with("opencode") || code.contains("timeout") || code == "model_error";
    (transient && !code.starts_with("tts")).then(|| pick(OOPS, seed))
}

/// The closing line when an answer stops part-way through.
pub fn partial_apology(reason: &str, seed: &str) -> &'static str {
    if reason == "core_requested" { PARTIAL_HANDOFF } else { pick(PARTIAL, seed) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_map_to_in_character_lines_or_stay_captions() {
        assert!(RETRY.iter().any(|l| Some(*l) == failure_line("asr_no_speech", "t1")));
        assert!(OOPS.iter().any(|l| Some(*l) == failure_line("brain_core_closed", "t1")));
        assert_eq!(failure_line("all_routes_unavailable", "t1"),
            Some(("我需要的服务现在不可用，请在 Mac 上检查模型设置。", "confused")));
        assert!(failure_line("local_worker_timeout: asr", "t").is_some());
        for quiet in ["cancelled", "brain_busy", "brain_not_started", "profile_requires_local_console", "tts_timeout", "stale_runtime"] {
            assert_eq!(failure_line(quiet, "t"), None, "{quiet}");
        }
        assert_eq!(partial_apology("core_requested", "t"), PARTIAL_HANDOFF);
        assert!(PARTIAL.contains(&partial_apology("core_timeout", "t")));
        let varied: std::collections::HashSet<_> = (0..20).map(|i| failure_line("asr_no_speech", &format!("turn-{i}"))).collect();
        assert!(varied.len() > 1, "varies across turns");
        let all = RETRY.iter().chain(OOPS).map(|l| l.1);
        assert!(all.into_iter().all(|e| crate::face_catalog_generated::EXPRESSION_IDS.contains(&e)));
    }
}

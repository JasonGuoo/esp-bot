//! Durable task results and the decision to speak them.
//!
//! The S31 accepts a presentation segment only while it is COMMITTED inside a
//! turn the device started. After a final segment it sits in WAIT_NEXT, so a
//! finished task is stored here and spoken on a later device turn, or offered
//! through the existing proactive path. This module does not run tasks.

/// Undelivered results plus tasks that have not stored one yet.
pub const MAX_UNDELIVERED: i64 = 32;
/// Rows in the inbox, including migrated placeholders that are never auto-spoken.
pub const MAX_INBOX_ROWS: i64 = 256;
pub const MAX_RESULT_BYTES: usize = 4_096;
pub const RESCAN_BATCH: usize = 8;
pub const MAX_PERSIST_PENDING: usize = 4;
/// An existing expression id. The installed firmware rejects SET_EXPRESSION while
/// passive, so the Mac publishes this hint and does not send a new device event.
pub const HINT_EXPRESSION: &str = "curious";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TurnDelivery {
    /// Speak the prepared line on this device turn.
    Speak,
    /// The person asked something else. Keep the result for a later wake.
    Defer,
}

/// A needs-input question is still asked on the next committed turn.
/// A result is spoken when this turn has no new question, or when the person
/// asks to hear the rest. Any other question leaves the result deferred.
pub fn on_committed_turn(user_text: &str, kind: &str) -> TurnDelivery {
    if kind == "needs_input" {
        return TurnDelivery::Speak;
    }
    let text = user_text.trim();
    if text.is_empty() || asks_to_expand(text) {
        TurnDelivery::Speak
    } else {
        TurnDelivery::Defer
    }
}

pub fn asks_to_expand(text: &str) -> bool {
    let text = text.trim();
    text.contains("展开") || text.contains("全部告诉") || text.contains("完整的") || text.contains("听完整")
}

pub fn asks_to_be_told(text: &str) -> bool {
    text.contains("做好告诉") || text.contains("查好告诉") || text.contains("做好了告诉") || text.contains("查到了告诉")
}

/// `brain_secure.c` only forwards `moment == "reminder"`, and the runtime only
/// accepts the reminder command kind. Any other moment is dropped on the board.
pub fn current_firmware_accepts_moment(moment: &str) -> bool {
    moment == "reminder"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Delivered,
    Partial,
    Deferred,
    Rejected,
}

/// `offered` or a prefix is not delivery. A matching completed receipt is.
/// A summary leaves the full text outstanding unless this play was the full promise.
pub fn settle_playback(promise_full: bool, played_full: bool, completed: bool, prefix_nonempty: bool, lease_matches: bool) -> ReceiptOutcome {
    if !lease_matches {
        return ReceiptOutcome::Rejected;
    }
    if completed && (promise_full || played_full) {
        ReceiptOutcome::Delivered
    } else if completed || prefix_nonempty {
        ReceiptOutcome::Partial
    } else {
        ReceiptOutcome::Deferred
    }
}

/// What is spoken first. A short result is the whole promise. A long one is a
/// sourced summary plus the question, and that play is not the full delivery.
pub fn spoken_result(full: &str) -> (String, bool) {
    let full = full.trim();
    if full.chars().count() <= 80 {
        (full.to_owned(), true)
    } else {
        let head: String = full.chars().take(40).collect();
        (format!("结果先说一句：{head}。要听完整的吗？"), false)
    }
}

pub fn played_row_id(activation: &str, task_id: &str, state: &str, expansion: bool) -> String {
    if expansion {
        format!("{activation}-lead-{task_id}-{state}-full")
    } else {
        format!("{activation}-lead-{task_id}-{state}")
    }
}

pub fn bound_text(text: &str, max_bytes: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max_bytes {
            break;
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_question_defers_a_result_and_still_asks_for_missing_input() {
        assert_eq!(on_committed_turn("树叶为什么是绿的", "result"), TurnDelivery::Defer);
        assert_eq!(on_committed_turn("", "result"), TurnDelivery::Speak);
        assert_eq!(on_committed_turn("展开完整的", "result"), TurnDelivery::Speak);
        assert_eq!(on_committed_turn("今天好冷", "needs_input"), TurnDelivery::Speak);
    }

    #[test]
    fn playback_evidence_is_not_replaced_by_an_offer_or_a_summary() {
        assert_eq!(settle_playback(true, false, false, false, true), ReceiptOutcome::Deferred);
        assert_eq!(settle_playback(true, false, true, true, false), ReceiptOutcome::Rejected);
        assert_eq!(settle_playback(false, false, true, true, true), ReceiptOutcome::Partial);
        assert_eq!(settle_playback(true, false, true, true, true), ReceiptOutcome::Delivered);
        assert_eq!(settle_playback(false, true, true, true, true), ReceiptOutcome::Delivered);
        assert_eq!(settle_playback(false, false, false, true, true), ReceiptOutcome::Partial);
    }

    #[test]
    fn a_long_result_is_a_summary_and_the_journal_row_is_not_reused() {
        let (line, full) = spoken_result("短结果。");
        assert_eq!((line.as_str(), full), ("短结果。", true));
        let long = "资".repeat(120);
        let (line, full) = spoken_result(&long);
        assert!(!full);
        assert!(line.contains("要听完整的吗？"));
        assert!(!line.contains(&"资".repeat(80)));
        let summary = played_row_id("device-1", "task_a", "succeeded", false);
        let expanded = played_row_id("device-1", "task_a", "succeeded", true);
        assert_ne!(summary, expanded);
        assert!(summary.ends_with("succeeded"));
    }

    #[test]
    fn the_installed_firmware_only_accepts_a_reminder_moment() {
        assert!(current_firmware_accepts_moment("reminder"));
        assert!(!current_firmware_accepts_moment("task_result"));
        assert!(asks_to_be_told("查好告诉我"));
        assert!(!asks_to_be_told("帮我查一下"));
    }
}

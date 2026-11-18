//! Per-sentence emotion tags from the Xiaodai core.
//!
//! The core may start a sentence with a one-word tag such as `〔worried〕`. The tag is
//! validated against the generated face catalogue, stripped before TTS and display,
//! and becomes that segment's `expression`. Untagged or invalid sentences fall back
//! to a small keyword table. The device runtime remains the authority over the face;
//! the tag is only the hint it prefers while speaking.

use crate::face_catalog_generated::EXPRESSION_IDS;

/// Expressions offered to the model: the conversational subset of the catalogue.
pub const OFFERED: &[&str] = &["happy","laughing","excited","overjoyed","gentle","affection","worried","sad",
    "surprised","curious","confused","thinking","shy","wink","smug","determined","relieved","sleepy"];

/// Prompt rule appended to the core persona.
pub fn prompt_rule() -> String {
    format!("每句话开头先写一个表情标签，例如〔gentle〕，标签不会被读出来，只能从这些里选：{}。按这句话的情绪选，开心用〔happy〕，不确定时也用〔happy〕。",
        OFFERED.join("、"))
}

/// Byte ranges of ASCII bracket tags (`〔id〕`, id = 1–15 of `a-z`, `_`, `-`).
fn tags(text: &str) -> Vec<(usize, usize, &str)> {
    let mut found = Vec::new();
    let mut rest = 0;
    while let Some(open) = text[rest..].find('〔').map(|i| rest + i) {
        let body = open + '〔'.len_utf8();
        let Some(close) = text[body..].find('〕').map(|i| body + i) else { break };
        let id = &text[body..close];
        if (1..=15).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'-') {
            found.push((open, close + '〕'.len_utf8(), id));
        }
        rest = body;
    }
    found
}

/// Remove every ASCII tag (valid or not). Non-ASCII marks such as `〔转交〕` stay.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (start, end, _) in tags(text) {
        out.push_str(&text[last..start]);
        last = end;
    }
    out.push_str(&text[last..]);
    out.trim().to_owned()
}

/// The sentence's first valid tag, and the sentence without any tags.
pub fn take(sentence: &str) -> (Option<&'static str>, String) {
    let tag = tags(sentence).into_iter()
        .find_map(|(_, _, id)| EXPRESSION_IDS.iter().copied().find(|known| *known == id));
    (tag, strip(sentence))
}

/// Keyword fallback when the model gave no valid tag.
pub fn from_keywords(text: &str) -> &'static str {
    const TABLE: &[(&[&str], &str)] = &[
        (&["哈哈","嘿嘿","好好笑","笑死"], "laughing"),
        (&["太好了","太棒了","恭喜","好厉害","真厉害"], "excited"),
        (&["疼","受伤","流血","害怕","担心"], "worried"),
        (&["难过","伤心","哭","可惜","遗憾"], "gentle"),
        (&["哇","居然","竟然","没想到"], "surprised"),
        (&["不知道","不确定","不记得","搞不清"], "confused"),
        (&["别怕","没关系","抱抱","辛苦了"], "gentle"),
        (&["晚安","困了","睡觉"], "sleepy"),
        (&["为什么","好奇","猜猜"], "curious"),
    ];
    TABLE.iter().find(|(words, _)| words.iter().any(|w| text.contains(w))).map(|(_, e)| *e).unwrap_or("happy")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_validated_stripped_and_backed_by_keywords() {
        assert!(OFFERED.iter().all(|e| EXPRESSION_IDS.contains(e)));
        assert_eq!(take("〔worried〕哎呀，疼不疼？"), (Some("worried"), "哎呀，疼不疼？".into()));
        assert_eq!(take("〔grumpy〕好吧。"), (None, "好吧。".into()), "unknown ids are stripped, not trusted");
        assert_eq!(take("小兔说：〔laughing〕“哈哈！”"), (Some("laughing"), "小兔说：“哈哈！”".into()));
        assert_eq!(take("〔转交〕"), (None, "〔转交〕".into()), "the escalation mark is not an emotion");
        assert_eq!(take("〔Happy〕你好。").1, "〔Happy〕你好。", "only lower-case catalogue ids");
        assert_eq!(strip("〔happy〕你好！〔wink〕再见。"), "你好！再见。");
        assert_eq!(from_keywords("哈哈，这个笑话好好笑。"), "laughing");
        assert_eq!(from_keywords("膝盖疼的话要告诉大人。"), "worried");
        assert_eq!(from_keywords("哇，好大的恐龙！"), "surprised");
        assert_eq!(from_keywords("这个我不知道。"), "confused");
        assert_eq!(from_keywords("今天天气不错。"), "happy");
    }
}

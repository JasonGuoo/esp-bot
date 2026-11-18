//! Conversation flow inside an engaged session: closing intents and the story
//! continuation offer (review §7.2 and §7.4). Notes are context for the model only.
use std::time::Instant;

const CLOSING: [&str; 14] = ["拜拜", "再见", "不聊了", "我走了", "下次再聊", "回头聊", "先这样", "就这样吧",
    "好了谢谢", "好的谢谢", "谢谢小呆", "没事了", "晚安", "睡觉了"];

/// "拜拜", "好了谢谢", "不聊了": a short goodbye, not a question that mentions one.
pub fn closing(transcript: &str) -> bool {
    let text: String = transcript.chars().filter(|c| !c.is_whitespace() && !"，。！？,.!?～~".contains(*c)).collect();
    if text.chars().count() > 12 || transcript.contains('？') || transcript.contains('?') || transcript.contains("什么") || transcript.contains("怎么") {
        return false;
    }
    CLOSING.iter().any(|p| text.contains(p))
}

/// A request for a story (the reply then ends with an offer to go on).
pub fn story(transcript: &str) -> bool {
    transcript.contains("故事") && !transcript.contains("不要") && !transcript.contains("别讲")
}

/// The note for this turn's flow, if any.
pub fn note(transcript: &str) -> Option<&'static str> {
    if closing(transcript) {
        Some("对方在道别：只回一句很短、温暖的告别（不超过12个字），不要提问，也不要邀请继续聊；句首用〔wink〕，晚上用〔sleepy〕。")
    } else if story(transcript) {
        Some("讲完这一段故事后，用一句很短的“要不要我讲下一段？”收尾；对方说要、好、继续时，就接着讲下一段。")
    } else {
        None
    }
}

/// Activations whose last turn was a goodbye; the gateway ends them once the reply is out.
#[derive(Default)]
pub struct Closing(std::sync::Mutex<std::collections::HashMap<String, Instant>>);

impl Closing {
    pub fn mark(&self, conversation_id: &str) {
        if let Ok(mut map) = self.0.lock() {
            if map.len() > 256 { map.clear(); }
            map.insert(conversation_id.to_owned(), Instant::now());
        }
    }
    /// True once, a moment after the goodbye was marked.
    pub fn take_due(&self, conversation_id: &str) -> bool {
        let Ok(mut map) = self.0.lock() else { return false };
        match map.get(conversation_id) {
            Some(at) if at.elapsed().as_millis() >= 1200 => { map.remove(conversation_id); true }
            _ => false,
        }
    }
    pub fn clear(&self, conversation_id: &str) {
        if let Ok(mut map) = self.0.lock() { map.remove(conversation_id); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_intents_are_short_goodbyes_only() {
        for text in ["拜拜", "好了谢谢！", "不聊了", "小呆再见～", "晚安啦", "那就这样吧", "谢谢小呆，拜拜"] {
            assert!(closing(text), "{text}");
        }
        for text in ["拜拜用英语怎么说？", "讲一个小兔子说再见的故事吧，要长一点的", "晚安是什么意思", "我们再聊聊恐龙", "谢谢你帮我查天气，明天会下雨吗"] {
            assert!(!closing(text), "{text}");
        }
    }

    #[test]
    fn stories_get_a_continuation_offer_and_goodbyes_a_short_sign_off() {
        assert!(note("给我讲个恐龙的故事").unwrap().contains("要不要我讲下一段"));
        assert!(note("不要讲故事了").is_none());
        assert!(note("拜拜").unwrap().contains("告别"));
        let closing = Closing::default();
        closing.mark("c");
        assert!(!closing.take_due("c"), "not before the reply had a moment to start");
    }
}

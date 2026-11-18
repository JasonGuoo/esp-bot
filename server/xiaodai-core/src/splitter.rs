//! Streaming sentence splitter for speech: emit each sentence as soon as it is complete.

const HARD: &[char] = &['。', '！', '？', '!', '?', '\n'];
const CLOSERS: &[char] = &['”', '’', '」', '』', '）', ')', '】', '"', '\'', '。', '！', '？', '!', '?', '…'];
const SOFT: &[char] = &['，', '、', ',', '；', ';', '：', ':', ' '];

pub struct SentenceSplitter {
    buf: String,
    max_chars: usize,
}

impl SentenceSplitter {
    pub fn new(max_chars: usize) -> Self {
        Self { buf: String::new(), max_chars: max_chars.max(8) }
    }

    /// Push a text delta; returns sentences completed by it.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = self.boundary() {
            let rest = self.buf.split_off(end);
            let sentence = std::mem::replace(&mut self.buf, rest);
            if let Some(s) = clean(&sentence) {
                out.push(s);
            }
        }
        out
    }

    /// End of stream: whatever is left.
    pub fn flush(&mut self) -> Option<String> {
        clean(&std::mem::take(&mut self.buf))
    }

    /// Byte index where the next sentence ends, if one is complete.
    fn boundary(&self) -> Option<usize> {
        let mut chars = self.buf.char_indices().peekable();
        let mut count = 0usize;
        let mut last_soft: Option<usize> = None;
        while let Some((i, c)) = chars.next() {
            count += 1;
            if HARD.contains(&c) {
                let mut end = i + c.len_utf8();
                // Attach trailing closers / repeated punctuation: 。」  ！？  ?!
                let mut at_end = true;
                while let Some(&(j, n)) = chars.peek() {
                    if CLOSERS.contains(&n) {
                        end = j + n.len_utf8();
                        chars.next();
                    } else {
                        at_end = false;
                        break;
                    }
                }
                // A closer (」 ”) may still arrive in the next delta; flush() ends the stream.
                if at_end && c != '\n' {
                    return None;
                }
                return Some(end);
            }
            if SOFT.contains(&c) && count >= 8 {
                last_soft = Some(i + c.len_utf8());
            }
            if count >= self.max_chars {
                return Some(last_soft.unwrap_or(i + c.len_utf8()));
            }
        }
        None
    }
}

/// Zero-width / BOM characters some providers emit (DeepSeek once streamed only these).
pub fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}' | '\u{00AD}')
}

/// True if the delta contains something a listener would hear (not just whitespace/zero-width).
pub fn has_visible(s: &str) -> bool {
    s.chars().any(|c| !c.is_whitespace() && !is_invisible(c))
}

fn clean(s: &str) -> Option<String> {
    let t: String = s.chars().filter(|c| !matches!(c, '*' | '#' | '`') && !is_invisible(*c)).collect();
    let t = t.trim();
    if t.chars().all(|c| CLOSERS.contains(&c) || SOFT.contains(&c) || c.is_whitespace()) {
        None
    } else {
        Some(t.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(deltas: &[&str]) -> Vec<String> {
        let mut s = SentenceSplitter::new(60);
        let mut out: Vec<String> = deltas.iter().flat_map(|d| s.push(d)).collect();
        out.extend(s.flush());
        out
    }

    #[test]
    fn splits_on_cjk_and_ascii_terminators() {
        assert_eq!(run(&["你好！我是小", "助手。今天怎么样？", "Fine!", " ok"]), vec!["你好！", "我是小助手。", "今天怎么样？", "Fine!", "ok"]);
    }

    #[test]
    fn emits_as_soon_as_complete() {
        let mut s = SentenceSplitter::new(60);
        assert!(s.push("珀斯是西澳").is_empty());
        assert_eq!(s.push("首府。阳光"), vec!["珀斯是西澳首府。"]);
        assert_eq!(s.flush().as_deref(), Some("阳光"));
    }

    #[test]
    fn keeps_a_closer_that_arrives_in_the_next_delta() {
        assert_eq!(run(&["她说：「好呀！", "」然后", "走了。", "“晚安！", "”"]), vec!["她说：「好呀！」", "然后走了。", "“晚安！”"]);
        let mut s = SentenceSplitter::new(60);
        assert!(s.push("你好！").is_empty());
        assert_eq!(s.push("我"), vec!["你好！"]);
    }

    #[test]
    fn keeps_closers_and_double_punct() {
        assert_eq!(run(&["他说“走吧。”然后呢？！好"]), vec!["他说“走吧。”", "然后呢？！", "好"]);
    }

    #[test]
    fn caps_long_runs_at_soft_punct_or_hard_limit() {
        let long = "这是一段很长的没有句号的话，".repeat(6);
        let out = run(&[&long]);
        assert!(out.len() > 1);
        assert!(out.iter().all(|s| s.chars().count() <= 60), "{out:?}");
        assert!(out[0].ends_with('，'));
        let solid = "字".repeat(130);
        let out = run(&[&solid]);
        assert_eq!(out.iter().map(|s| s.chars().count()).collect::<Vec<_>>(), vec![60, 60, 10]);
    }

    #[test]
    fn zero_width_only_is_not_visible() {
        assert!(!has_visible("\u{200D}\u{200B}\u{200B} \n"));
        assert!(has_visible("\u{200B}好"));
        assert_eq!(run(&["\u{200B}\u{200D}", "你好。"]), vec!["你好。"]);
    }

    #[test]
    fn strips_markdown_and_empty_fragments() {
        assert_eq!(run(&["**好的**。\n\n", "。"]), vec!["好的。"]);
    }
}

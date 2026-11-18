//! Incremental Server-Sent Events parser + OpenAI-style delta extraction.
use serde_json::Value;

/// Feed raw body bytes (arbitrary chunk boundaries, even mid-UTF-8); get complete `data:` payloads.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: String,
    has_data: bool,
}

impl SseParser {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut raw: Vec<u8> = self.buf.drain(..=pos).collect();
            raw.pop();
            if raw.last() == Some(&b'\r') {
                raw.pop();
            }
            let line = String::from_utf8_lossy(&raw).into_owned();
            self.line(&line, &mut out);
        }
        out
    }

    /// Call at end of stream to flush an event that had no trailing blank line.
    pub fn finish(&mut self) -> Option<String> {
        if !self.buf.is_empty() {
            let raw = std::mem::take(&mut self.buf);
            let line = String::from_utf8_lossy(&raw).into_owned();
            let mut out = Vec::new();
            self.line(&line, &mut out);
            if let Some(d) = out.pop() {
                return Some(d);
            }
        }
        if self.has_data {
            self.has_data = false;
            return Some(std::mem::take(&mut self.data));
        }
        None
    }

    fn line(&mut self, line: &str, out: &mut Vec<String>) {
        if line.is_empty() {
            if self.has_data {
                self.has_data = false;
                out.push(std::mem::take(&mut self.data));
            }
            return;
        }
        if line.starts_with(':') {
            return; // comment / keep-alive
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        if field == "data" {
            if self.has_data {
                self.data.push('\n');
            }
            self.data.push_str(value);
            self.has_data = true;
        }
    }
}

#[derive(Debug, PartialEq)]
pub enum Chunk {
    /// Visible answer text.
    Content(String),
    /// Thinking text (ignored for output; char count only).
    Reasoning(usize),
    Done,
    Error(String),
    Ignore,
}

pub fn parse_chunk(data: &str) -> Chunk {
    let d = data.trim();
    if d == "[DONE]" {
        return Chunk::Done;
    }
    let v: Value = match serde_json::from_str(d) {
        Ok(v) => v,
        Err(_) => return Chunk::Ignore,
    };
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        let msg = err
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| err.to_string());
        return Chunk::Error(truncate(&msg, 200));
    }
    let delta = &v["choices"][0]["delta"];
    if let Some(s) = delta["content"].as_str() {
        if !s.is_empty() {
            return Chunk::Content(s.to_owned());
        }
    }
    for k in ["reasoning_content", "reasoning"] {
        if let Some(s) = delta[k].as_str() {
            if !s.is_empty() {
                return Chunk::Reasoning(s.chars().count());
            }
        }
    }
    Chunk::Ignore
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(s: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":{}}}}}]}}\n\n", serde_json::to_string(s).unwrap())
    }

    #[test]
    fn parses_events_split_at_any_byte() {
        let body = format!("{}: keep-alive\n\n{}data: [DONE]\n\n", content("你好，"), content("世界。"));
        let bytes = body.as_bytes();
        // Every possible split point, including inside multi-byte chars.
        for cut in 0..bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.feed(&bytes[..cut]);
            got.extend(p.feed(&bytes[cut..]));
            let chunks: Vec<Chunk> = got.iter().map(|d| parse_chunk(d)).collect();
            assert_eq!(
                chunks,
                vec![Chunk::Content("你好，".into()), Chunk::Content("世界。".into()), Chunk::Done],
                "cut at {cut}"
            );
        }
    }

    #[test]
    fn handles_crlf_and_one_byte_feeds() {
        let body = content("a").replace('\n', "\r\n") + "data: [DONE]\r\n\r\n";
        let mut p = SseParser::default();
        let mut got = Vec::new();
        for b in body.as_bytes() {
            got.extend(p.feed(std::slice::from_ref(b)));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(parse_chunk(&got[0]), Chunk::Content("a".into()));
        assert_eq!(parse_chunk(&got[1]), Chunk::Done);
    }

    #[test]
    fn reasoning_is_not_content() {
        let d = r#"{"choices":[{"delta":{"reasoning_content":"让我想想","content":null}}]}"#;
        assert_eq!(parse_chunk(d), Chunk::Reasoning(4));
        let d = r#"{"choices":[{"delta":{"role":"assistant","content":""}}]}"#;
        assert_eq!(parse_chunk(d), Chunk::Ignore);
        let d = r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":3}}"#;
        assert_eq!(parse_chunk(d), Chunk::Ignore);
    }

    #[test]
    fn error_payload_and_finish_without_blank_line() {
        assert_eq!(parse_chunk(r#"{"error":{"message":"bad model"}}"#), Chunk::Error("bad model".into()));
        let mut p = SseParser::default();
        assert!(p.feed(b"data: [DONE]").is_empty());
        assert_eq!(p.finish().as_deref(), Some("[DONE]"));
    }
}

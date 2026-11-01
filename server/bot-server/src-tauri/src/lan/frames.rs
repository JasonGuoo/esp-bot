//! Application-level frames layered on the v1 LAN WebSocket handshake.
//!
//! The hello/welcome handshake stays protocol v1 (see `protocol.rs` and
//! shared/lan-discovery-v1.md). Everything after `welcome` uses the
//! versioned envelope from docs/local-first-hybrid-brain.md §5: one
//! request/response/event/error envelope instead of one endpoint per
//! ability.

use serde_json::{json, Value};

pub const ENVELOPE_VERSION: u64 = 2;
pub const METHOD_FACE_SET: &str = "face.set";

/// Theme ids mirror shared/themes.json; that file stays the source of truth.
pub const THEME_IDS: [&str; 5] = ["mint", "sakura", "lavender", "butter", "sky"];

/// Expression ids mirror the Face component and the legacy Python Brain.
pub const EXPRESSION_IDS: [&str; 9] = [
    "idle", "listening", "happy", "curious", "thinking", "surprised", "sleepy", "shy", "blink",
];

const PAYLOAD_KEYS: [&str; 5] = ["theme", "expression", "subtitle", "gaze_x", "gaze_y"];
pub const SUBTITLE_MAX_CHARS: usize = 180;
pub const GAZE_MIN: i64 = -100;
pub const GAZE_MAX: i64 = 100;
/// Relative age budget: the device interprets freshness locally, never by
/// comparing wall-clock time to its own uptime.
pub const DEFAULT_DEADLINE_MS: u64 = 45_000;

/// Validates a `face.set` payload with the same rules as the legacy Python
/// Brain (server/espbot/app.py): key allowlist, known theme/expression,
/// subtitle length and integer gaze range. Booleans are not integers.
pub fn validate_face_set(payload: &Value) -> Result<(), String> {
    let map = payload.as_object().ok_or("payload must be a JSON object")?;
    for key in map.keys() {
        if !PAYLOAD_KEYS.contains(&key.as_str()) {
            return Err(format!("unsupported payload field: {key}"));
        }
    }
    if let Some(theme) = map.get("theme") {
        let theme = theme.as_str().ok_or("theme must be a string")?;
        if !THEME_IDS.contains(&theme) {
            return Err(format!("unknown theme: {theme}"));
        }
    }
    if let Some(expression) = map.get("expression") {
        let expression = expression.as_str().ok_or("expression must be a string")?;
        if !EXPRESSION_IDS.contains(&expression) {
            return Err(format!("unknown expression: {expression}"));
        }
    }
    if let Some(subtitle) = map.get("subtitle") {
        let subtitle = subtitle.as_str().ok_or("subtitle must be a string")?;
        if subtitle.chars().count() > SUBTITLE_MAX_CHARS {
            return Err(format!("subtitle exceeds {SUBTITLE_MAX_CHARS} characters"));
        }
    }
    for axis in ["gaze_x", "gaze_y"] {
        if let Some(value) = map.get(axis) {
            let gaze = value
                .as_i64()
                .ok_or_else(|| format!("{axis} must be an integer"))?;
            if !(GAZE_MIN..=GAZE_MAX).contains(&gaze) {
                return Err(format!("{axis} must be within {GAZE_MIN}..={GAZE_MAX}"));
            }
        }
    }
    Ok(())
}

pub fn validate(method: &str, payload: &Value) -> Result<(), String> {
    match method {
        METHOD_FACE_SET => validate_face_set(payload),
        other => Err(format!("unsupported method: {other}")),
    }
}

pub fn build_request(
    cmd_id: &str,
    session_id: &str,
    method: &str,
    payload: &Value,
    sent_at_ms: u64,
    deadline_ms: u64,
) -> String {
    json!({
        "v": ENVELOPE_VERSION,
        "type": "request",
        "id": cmd_id,
        "method": method,
        "session_id": session_id,
        "deadline_ms": deadline_ms,
        "sent_at_ms": sent_at_ms,
        "payload": payload,
    })
    .to_string()
}

#[derive(Debug, PartialEq, Eq)]
pub struct ResponseFrame {
    pub id: String,
    pub ok: bool,
    pub detail: Option<String>,
    pub error: Option<String>,
}

/// Parses a device ack. Lenient on optional fields, strict on identity:
/// `type` must be `response` and `id`/`ok` must be present.
pub fn parse_response(frame: &Value) -> Option<ResponseFrame> {
    if frame.get("type").and_then(Value::as_str) != Some("response") {
        return None;
    }
    let id = frame.get("id")?.as_str()?.to_owned();
    let ok = frame.get("ok")?.as_bool()?;
    let detail = frame.get("detail").and_then(Value::as_str).map(str::to_owned);
    let error = frame.get("error").and_then(Value::as_str).map(str::to_owned);
    Some(ResponseFrame { id, ok, detail, error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::protocol::MAX_TEXT_BYTES;

    fn full_payload() -> Value {
        json!({
            "theme": "sakura",
            "expression": "happy",
            "subtitle": "你好！",
            "gaze_x": 35,
            "gaze_y": -20
        })
    }

    #[test]
    fn accepts_valid_full_payload() {
        assert!(validate_face_set(&full_payload()).is_ok());
        assert!(validate_face_set(&json!({})).is_ok());
    }

    #[test]
    fn rejects_unknown_theme_or_expression() {
        let mut payload = full_payload();
        payload["theme"] = json!("neon");
        assert_eq!(validate_face_set(&payload).unwrap_err(), "unknown theme: neon");
        let mut payload = full_payload();
        payload["expression"] = json!("angry");
        assert!(validate_face_set(&payload).unwrap_err().contains("unknown expression"));
    }

    #[test]
    fn rejects_unsupported_method_and_field() {
        assert!(validate("shell.exec", &json!({})).unwrap_err().contains("unsupported method"));
        let mut payload = full_payload();
        payload["cmd"] = json!("rm -rf");
        assert!(validate_face_set(&payload).unwrap_err().contains("unsupported payload field"));
    }

    #[test]
    fn rejects_bad_gaze_values() {
        let mut payload = full_payload();
        payload["gaze_x"] = json!(9999);
        assert!(validate_face_set(&payload).unwrap_err().contains("gaze_x"));
        let mut payload = full_payload();
        payload["gaze_y"] = json!(true);
        assert!(validate_face_set(&payload).unwrap_err().contains("gaze_y must be an integer"));
    }

    #[test]
    fn rejects_long_subtitle() {
        let mut payload = json!({});
        payload["subtitle"] = json!("x".repeat(SUBTITLE_MAX_CHARS + 1));
        assert!(validate_face_set(&payload).unwrap_err().contains("subtitle"));
    }

    #[test]
    fn request_frame_stays_within_text_limit() {
        let mut payload = json!({});
        payload["subtitle"] = json!("好".repeat(SUBTITLE_MAX_CHARS));
        let frame = build_request(
            "c1",
            "00000000-0000-0000-0000-000000000000",
            METHOD_FACE_SET,
            &payload,
            1_770_000_000_000,
            DEFAULT_DEADLINE_MS,
        );
        assert!(frame.len() <= MAX_TEXT_BYTES, "frame was {} bytes", frame.len());
        let value: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["v"], ENVELOPE_VERSION);
        assert_eq!(value["type"], "request");
        assert_eq!(value["method"], METHOD_FACE_SET);
    }

    #[test]
    fn parses_response_frames() {
        let ok: Value =
            serde_json::from_str(r#"{"v":2,"type":"response","id":"c17","ok":true,"detail":"applied"}"#).unwrap();
        assert_eq!(
            parse_response(&ok),
            Some(ResponseFrame { id: "c17".into(), ok: true, detail: Some("applied".into()), error: None })
        );
        let rejected: Value =
            serde_json::from_str(r#"{"v":2,"type":"response","id":"c18","ok":false,"error":"stale"}"#).unwrap();
        assert_eq!(parse_response(&rejected).map(|r| r.error), Some(Some("stale".into())));

        let not_response: Value = serde_json::from_str(r#"{"type":"event","topic":"x"}"#).unwrap();
        assert_eq!(parse_response(&not_response), None);
        let no_ok: Value = serde_json::from_str(r#"{"type":"response","id":"c19"}"#).unwrap();
        assert_eq!(parse_response(&no_ok), None);
    }
}

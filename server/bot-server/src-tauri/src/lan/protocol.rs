use serde::Deserialize;

pub const SERVICE_TYPE: &str = "_espbot-server._tcp.local.";
pub const WS_PATH: &str = "/v1/lan/ws";
pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_TEXT_BYTES: usize = 2048;
pub const HELLO_TIMEOUT_SECONDS: u64 = 7;

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub struct Hello {
    #[serde(rename = "type")]
    kind: String,
    pub protocol: u8,
    pub device_id: String,
    pub name: String,
}

pub fn parse_hello(input: &str) -> Result<Hello, &'static str> {
    if input.len() > 512 {
        return Err("hello is too large");
    }
    let hello: Hello = serde_json::from_str(input).map_err(|_| "invalid hello JSON")?;
    if hello.kind != "hello" || hello.protocol != PROTOCOL_VERSION {
        return Err("incompatible protocol");
    }
    if !valid_device_id(&hello.device_id) {
        return Err("invalid device_id");
    }
    if hello.name.is_empty()
        || hello.name.len() > 64
        || hello.name.chars().any(char::is_control)
    {
        return Err("invalid display name");
    }
    Ok(hello)
}

fn valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}

pub fn welcome(server_id: &str) -> String {
    serde_json::json!({
        "type": "welcome",
        "protocol": PROTOCOL_VERSION,
        "server_id": server_id,
        "heartbeat_sec": 25
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_hello() {
        let m = parse_hello(r#"{"type":"hello","protocol":1,"device_id":"korvo-01","name":"ESP Bot"}"#).unwrap();
        assert_eq!(m.device_id, "korvo-01");
    }

    #[test]
    fn rejects_wrong_protocol_or_unsafe_id() {
        assert!(parse_hello(r#"{"type":"hello","protocol":2,"device_id":"korvo-01","name":"ESP Bot"}"#).is_err());
        assert!(parse_hello(r#"{"type":"hello","protocol":1,"device_id":"../bad","name":"ESP Bot"}"#).is_err());
    }

    #[test]
    fn welcome_has_matching_version() {
        let value: serde_json::Value = serde_json::from_str(&welcome("sample")).unwrap();
        assert_eq!(value["protocol"], PROTOCOL_VERSION);
        assert_eq!(value["server_id"], "sample");
    }
}

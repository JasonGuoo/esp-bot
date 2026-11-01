//! Server→device command hub.
//!
//! Owns the live outbound sender per connected device, hands out monotonic
//! command ids, correlates device acks and never queues commands for
//! offline devices (fail fast with `CmdError::Offline`); durable queues
//! arrive with the firmware conversation integration.
//!
//! The hub is intentionally synchronous: sends use `try_send` so Tauri
//! commands can call `send()` without an async context, and the socket
//! session pumps the receiving half inside its `select!` loop.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

use serde_json::Value;

use super::{now_ms, EventSink, LanEvent};
use super::frames::{self, DEFAULT_DEADLINE_MS};
use super::protocol::MAX_TEXT_BYTES;

const OUTBOUND_CHANNEL_CAPACITY: usize = 16;
/// Acked and expired records are kept for a short window for the console
/// event log, then dropped so a chatty session cannot leak memory.
const IN_FLIGHT_TTL_MS: u64 = 120_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdError {
    /// No live session for the device, or the session link is congested.
    Offline,
    /// Pairing gate: the device has not been approved in the console.
    NotApproved,
    Validation(String),
    FrameTooLarge,
}

impl std::fmt::Display for CmdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CmdError::Offline => write!(f, "bot is not connected or the link is congested"),
            CmdError::NotApproved => write!(f, "device is not approved for commands"),
            CmdError::Validation(message) => write!(f, "invalid command: {message}"),
            CmdError::FrameTooLarge => {
                write!(f, "command frame exceeds the {MAX_TEXT_BYTES} byte text limit")
            }
        }
    }
}

#[derive(Clone)]
struct LiveSender {
    session_id: String,
    tx: mpsc::Sender<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AckRecord {
    pub ok: bool,
    pub at_ms: u64,
    pub detail: Option<String>,
    pub error: Option<String>,
}

struct InFlight {
    device_id: String,
    method: String,
    sent_at_ms: u64,
    ack: Option<AckRecord>,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    outbound: HashMap<String, LiveSender>,
    in_flight: HashMap<String, InFlight>,
}

pub struct CommandHub {
    sink: EventSink,
    /// Pairing gate; the single enforcement point for outbound commands.
    approval: Box<dyn Fn(&str) -> bool + Send + Sync>,
    inner: Mutex<Inner>,
}

impl CommandHub {
    /// Allow-all hub for tests; production wiring must use
    /// [`CommandHub::with_approval`].
    #[cfg(test)]
    pub fn new(sink: EventSink) -> Arc<Self> {
        Self::with_approval(sink, Box::new(|_| true))
    }

    pub fn with_approval(sink: EventSink, approval: Box<dyn Fn(&str) -> bool + Send + Sync>) -> Arc<Self> {
        Arc::new(Self { sink, approval, inner: Mutex::new(Inner::default()) })
    }

    /// Creates the outbound channel a socket session pumps frames from.
    pub fn create_outbound(&self) -> (mpsc::Sender<String>, mpsc::Receiver<String>) {
        mpsc::channel(OUTBOUND_CHANNEL_CAPACITY)
    }

    pub fn register_session(&self, device_id: &str, session_id: &str, tx: mpsc::Sender<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.outbound.insert(
            device_id.to_owned(),
            LiveSender { session_id: session_id.to_owned(), tx },
        );
    }

    /// Guarded by session id so a stale session cannot unregister its
    /// replacement (same pattern as `LanState::disconnected`).
    pub fn unregister_session(&self, device_id: &str, session_id: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.outbound.get(device_id).is_some_and(|live| live.session_id == session_id) {
            inner.outbound.remove(device_id);
        }
    }

    /// Validates, frames and pushes one command to the live device session.
    ///
    /// `session_id` in the frame comes from the live registration so the
    /// device can tie commands to the connection that carried them.
    pub fn send(&self, device_id: &str, method: &str, payload: &Value) -> Result<String, CmdError> {
        if !(self.approval)(device_id) {
            return Err(CmdError::NotApproved);
        }
        frames::validate(method, payload).map_err(CmdError::Validation)?;
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        sweep_expired(&mut inner);
        let live = inner.outbound.get(device_id).cloned().ok_or(CmdError::Offline)?;
        let cmd_id = format!("c{}", inner.next_id);
        inner.next_id += 1;
        let frame = frames::build_request(
            &cmd_id,
            &live.session_id,
            method,
            payload,
            now_ms(),
            DEFAULT_DEADLINE_MS,
        );
        if frame.len() > MAX_TEXT_BYTES {
            return Err(CmdError::FrameTooLarge);
        }
        if live.tx.try_send(frame).is_err() {
            // Closed (session ending) or saturated (device not draining);
            // either way the command is not deliverable right now.
            return Err(CmdError::Offline);
        }
        inner.in_flight.insert(
            cmd_id.clone(),
            InFlight {
                device_id: device_id.to_owned(),
                method: method.to_owned(),
                sent_at_ms: now_ms(),
                ack: None,
            },
        );
        drop(inner);
        (self.sink)(LanEvent::new(
            "command_sent",
            Some(device_id),
            method,
            Some(serde_json::json!({ "id": cmd_id })),
        ));
        Ok(cmd_id)
    }

    /// Records a device ack for a previously sent command.
    ///
    /// Returns whether the ack matched a live in-flight command; unknown,
    /// duplicate and malformed acks still surface as events so a
    /// misbehaving device is visible in the console instead of silent.
    pub fn ack(&self, device_id: &str, frame: &Value) -> bool {
        let Some(response) = frames::parse_response(frame) else {
            (self.sink)(LanEvent::new(
                "command_ack_malformed",
                Some(device_id),
                "response frame missing id or ok",
                Some(frame.clone()),
            ));
            return false;
        };
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        sweep_expired(&mut inner);
        let record = inner.in_flight.get_mut(&response.id);
        match record {
            Some(record) if record.ack.is_none() => {
                let ack = AckRecord {
                    ok: response.ok,
                    at_ms: now_ms(),
                    detail: response.detail.clone(),
                    error: response.error.clone(),
                };
                record.ack = Some(ack.clone());
                let method = record.method.clone();
                drop(inner);
                let outcome = if response.ok { "applied" } else { "rejected" };
                (self.sink)(LanEvent::new(
                    "command_acked",
                    Some(device_id),
                    &format!("{} {method} {outcome}", response.id),
                    Some(serde_json::to_value(&ack).unwrap_or(Value::Null)),
                ));
                true
            }
            Some(_) => {
                drop(inner);
                (self.sink)(LanEvent::new(
                    "command_ack_duplicate",
                    Some(device_id),
                    &response.id,
                    Some(frame.clone()),
                ));
                false
            }
            None => {
                drop(inner);
                (self.sink)(LanEvent::new(
                    "command_ack_unknown",
                    Some(device_id),
                    &format!("no in-flight command {}", response.id),
                    Some(frame.clone()),
                ));
                false
            }
        }
    }

    /// Commands sent to the device that are still awaiting their first ack.
    pub fn pending_count(&self, device_id: &str) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .in_flight
            .values()
            .filter(|record| record.ack.is_none() && record.device_id == device_id)
            .count()
    }

    pub fn has_live_session(&self, device_id: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.outbound.contains_key(device_id)
    }
}

fn sweep_expired(inner: &mut Inner) {
    let now = now_ms();
    inner.in_flight.retain(|_, record| now.saturating_sub(record.sent_at_ms) <= IN_FLIGHT_TTL_MS);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn test_hub() -> Arc<CommandHub> {
        static EVENTS: AtomicUsize = AtomicUsize::new(0);
        CommandHub::new(Arc::new(|_event| {
            EVENTS.fetch_add(1, Ordering::SeqCst);
        }))
    }

    fn register(hub: &CommandHub, device_id: &str) -> mpsc::Receiver<String> {
        let (tx, rx) = hub.create_outbound();
        hub.register_session(device_id, "session-1", tx);
        rx
    }

    fn valid_payload() -> Value {
        serde_json::json!({ "expression": "happy", "subtitle": "hi" })
    }

    #[test]
    fn send_rejects_offline_device() {
        let hub = test_hub();
        let err = hub.send("korvo-01", frames::METHOD_FACE_SET, &valid_payload()).unwrap_err();
        assert_eq!(err, CmdError::Offline);
    }

    #[test]
    fn send_rejects_invalid_payload_before_touching_link() {
        let hub = test_hub();
        register(&hub, "korvo-01");
        let err = hub
            .send("korvo-01", frames::METHOD_FACE_SET, &serde_json::json!({ "theme": "neon" }))
            .unwrap_err();
        assert!(matches!(err, CmdError::Validation(_)));
    }

    #[test]
    fn send_delivers_frame_and_ack_closes_the_loop() {
        let hub = test_hub();
        let mut rx = register(&hub, "korvo-01");

        let cmd_id = hub.send("korvo-01", frames::METHOD_FACE_SET, &valid_payload()).unwrap();
        assert_eq!(hub.pending_count("korvo-01"), 1);

        let frame = rx.try_recv().expect("frame should be queued on the channel");
        let value: Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["id"], cmd_id.as_str());
        assert_eq!(value["type"], "request");
        assert_eq!(value["method"], frames::METHOD_FACE_SET);
        assert_eq!(value["session_id"], "session-1");
        assert_eq!(value["payload"]["expression"], "happy");

        let ack = serde_json::json!({
            "v": frames::ENVELOPE_VERSION,
            "type": "response",
            "id": cmd_id,
            "ok": true,
            "detail": "applied"
        });
        assert!(hub.ack("korvo-01", &ack));
        assert_eq!(hub.pending_count("korvo-01"), 0);
    }

    #[test]
    fn unknown_and_duplicate_acks_do_not_panic_or_match() {
        let hub = test_hub();
        // Keep the receiver alive: dropping it closes the channel and send() would be Offline.
        let mut rx = register(&hub, "korvo-01");
        let unknown = serde_json::json!({
            "type": "response",
            "id": "c999",
            "ok": true
        });
        assert!(!hub.ack("korvo-01", &unknown));

        let cmd_id = hub.send("korvo-01", frames::METHOD_FACE_SET, &valid_payload()).unwrap();
        let _ = rx.try_recv();
        let ack = serde_json::json!({ "type": "response", "id": cmd_id, "ok": true });
        assert!(hub.ack("korvo-01", &ack));
        assert!(!hub.ack("korvo-01", &ack), "second ack must not match again");
    }

    #[test]
    fn unapproved_device_never_receives_commands() {
        let hub = CommandHub::with_approval(
            Arc::new(|_event| {}),
            Box::new(|device_id| device_id != "stranger"),
        );
        let mut rx = register(&hub, "stranger");
        let err = hub
            .send("stranger", frames::METHOD_FACE_SET, &valid_payload())
            .unwrap_err();
        assert_eq!(err, CmdError::NotApproved);
        assert!(rx.try_recv().is_err(), "no frame may reach an unapproved device");
    }

    #[test]
    fn unregister_is_session_guarded() {
        let hub = test_hub();
        register(&hub, "korvo-01");
        let (tx, _rx_new) = hub.create_outbound();
        hub.register_session("korvo-01", "session-2", tx);

        hub.unregister_session("korvo-01", "session-1");
        assert!(hub.has_live_session("korvo-01"), "stale unregister must not remove session-2");

        hub.unregister_session("korvo-01", "session-2");
        assert!(!hub.has_live_session("korvo-01"));
    }
}

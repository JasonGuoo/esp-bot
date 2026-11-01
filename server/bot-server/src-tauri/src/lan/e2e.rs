//! End-to-end WebSocket session test: a fake bot performs the exact v1
//! handshake the firmware `lan_link` component speaks, gets paired, and
//! completes a command/ack roundtrip. No mDNS, no real device — this
//! validates the server side only and says nothing about hardware.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use super::{CommandHub, LanEvent, LanState};
use crate::pairing::PairingStore;

type BotSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn read_json(socket: &mut BotSocket) -> Value {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => return serde_json::from_str(&text).expect("server sent valid JSON"),
            Some(Ok(_)) => continue,
            other => panic!("expected a JSON text frame, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn handshake_pairing_command_and_ack_roundtrip() {
    let dir = std::env::temp_dir().join(format!("espbot-e2e-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();

    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let events = Arc::clone(&events);
        Arc::new(move |event: LanEvent| events.lock().unwrap().push(event.kind.clone()))
    };

    let pairing = Arc::new(PairingStore::new(&dir, Arc::clone(&sink) as _));
    let pairing_for_gate = Arc::clone(&pairing);
    let hub = CommandHub::with_approval(
        Arc::clone(&sink) as _,
        Box::new(move |device_id| pairing_for_gate.is_approved(device_id)),
    );
    let hub_handle = Arc::clone(&hub);
    let pairing_handle = Arc::clone(&pairing);
    let state = Arc::new(LanState::default());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        super::run_server(
            listener,
            state,
            sink,
            "e2eserverid00000000000000000000".to_owned(),
            hub,
            pairing,
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // --- Fake bot: hello/welcome handshake exactly as firmware sends it.
    let (mut bot, _response) =
        tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}/v1/lan/ws")).await.expect("connect");
    bot.send(Message::Text(
        r#"{"type":"hello","protocol":1,"device_id":"e2e-bot-01","name":"E2E Bot"}"#.into(),
    ))
    .await
    .unwrap();
    let welcome = read_json(&mut bot).await;
    assert_eq!(welcome["type"], "welcome");
    assert_eq!(welcome["server_id"], "e2eserverid00000000000000000000");
    assert_eq!(welcome["protocol"], 1);

    // --- First-seen device is pending: it connected, but commands are gated.
    let payload = json!({ "expression": "happy", "subtitle": "hi" });
    assert_eq!(
        hub_handle.send("e2e-bot-01", super::METHOD_FACE_SET, &payload),
        Err(super::cmd::CmdError::NotApproved)
    );

    // --- Approve from the "console"; the command now flows to the socket.
    pairing_handle.approve("e2e-bot-01").unwrap();
    let cmd_id = hub_handle
        .send("e2e-bot-01", super::METHOD_FACE_SET, &payload)
        .expect("approved bot with live session accepts the command");
    assert_eq!(hub_handle.pending_count("e2e-bot-01"), 1);

    let request = read_json(&mut bot).await;
    assert_eq!(request["type"], "request");
    assert_eq!(request["id"], cmd_id.as_str());
    assert_eq!(request["method"], "face.set");
    assert_eq!(request["payload"]["expression"], "happy");
    assert!(request["deadline_ms"].is_u64(), "device needs a relative freshness budget");
    assert!(request["sent_at_ms"].is_u64());

    // --- Fake bot acks like firmware will; the in-flight record drains.
    bot.send(Message::Text(
        json!({ "v": 2, "type": "response", "id": cmd_id, "ok": true, "detail": "applied" }).to_string(),
    ))
    .await
    .unwrap();
    for _ in 0..100 {
        if hub_handle.pending_count("e2e-bot-01") == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(hub_handle.pending_count("e2e-bot-01"), 0, "ack must clear the in-flight command");
    assert!(events.lock().unwrap().contains(&"command_acked".to_owned()));

    server.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

use std::{
    net::{IpAddr, SocketAddr},
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tokio::time::{interval, timeout, MissedTickBehavior};
use uuid::Uuid;

use super::{
    now_ms,
    protocol::{parse_hello, welcome, HELLO_TIMEOUT_SECONDS, MAX_TEXT_BYTES},
    Bot, LanEvent, RouteState,
};

/// mDNS does not cross routers. Reject globally routed peers even if the host
/// is accidentally exposed via a firewall/port forward. This is NOT auth.
fn is_local_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local() || v4.is_loopback(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

pub async fn upgrade(
    State(state): State<RouteState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    ws: WebSocketUpgrade,
) -> Response {
    if !is_local_peer(addr.ip()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    ws.max_message_size(MAX_TEXT_BYTES)
        .on_upgrade(move |socket| session(socket, state, addr))
        .into_response()
}

async fn session(mut socket: WebSocket, ctx: RouteState, peer: SocketAddr) {
    let first = timeout(Duration::from_secs(HELLO_TIMEOUT_SECONDS), socket.recv()).await;
    let hello = match first {
        Ok(Some(Ok(Message::Text(text)))) => match parse_hello(&text) {
            Ok(hello) => hello,
            Err(_) => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }
        },
        _ => {
            let _ = socket.send(Message::Close(None)).await;
            return;
        }
    };

    let device_id = hello.device_id.clone();

    // Pairing gate: denied devices get the handshake they require and then
    // an explicit close, so the firmware logs a real refusal instead of a
    // protocol mystery. Pending and approved devices connect normally;
    // commands to non-approved devices are blocked inside CommandHub.
    if ctx.pairing.classify(&device_id, &hello.name) == crate::pairing::PairState::Denied {
        let _ = socket.send(Message::Text(welcome(&ctx.server_id).into())).await;
        let _ = socket
            .send(Message::Close(Some(CloseFrame {
                code: 4003,
                reason: "pairing_denied".into(),
            })))
            .await;
        (ctx.sink)(LanEvent::new(
            "bot_denied",
            Some(&device_id),
            "denied device attempted to connect",
            None,
        ));
        return;
    }

    let session_id = Uuid::new_v4().to_string();
    if socket.send(Message::Text(welcome(&ctx.server_id).into())).await.is_err() {
        return;
    }
    // Register the outbound sender before announcing the bot so a command
    // triggered by the bot_connected event can never race a missing channel.
    let (out_tx, mut out_rx) = ctx.hub.create_outbound();
    ctx.hub.register_session(&device_id, &session_id, out_tx);
    ctx.state.connected(
        Bot {
            device_id: device_id.clone(),
            name: hello.name,
            peer: peer.ip().to_string(),
            connected_at_ms: now_ms(),
        },
        session_id.clone(),
    );
    (ctx.sink)(LanEvent::new("bot_connected", Some(&device_id), &peer.to_string(), None));

    let mut ticker = interval(Duration::from_secs(25));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;
    let mut last_received = Instant::now();
    loop {
        tokio::select! {
            received = socket.recv() => {
                match received {
                    Some(Ok(Message::Text(text))) if text.len() <= MAX_TEXT_BYTES => {
                        last_received = Instant::now();
                        if let Ok(value) = serde_json::from_str::<Value>(&text) {
                            match value.get("type").and_then(Value::as_str) {
                                Some("event") => {
                                    (ctx.sink)(LanEvent::new("bot_message", Some(&device_id), "event received", Some(value)));
                                }
                                Some("response") => {
                                    ctx.hub.ack(&device_id, &value);
                                }
                                // Unknown frames stay visible in the console
                                // log instead of being dropped silently.
                                other => {
                                    (ctx.sink)(LanEvent::new("bot_frame_unhandled", Some(&device_id), &format!("frame type {other:?}"), Some(value)));
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Pong(_))) | Some(Ok(Message::Ping(_))) => {
                        last_received = Instant::now();
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {}
                }
            }
            _ = ticker.tick() => {
                if last_received.elapsed() > Duration::from_secs(80) {
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            frame = out_rx.recv() => {
                match frame {
                    Some(frame) => {
                        if socket.send(Message::Text(frame.into())).await.is_err() {
                            break;
                        }
                    }
                    // Sender replaced by a newer session for this device.
                    None => break,
                }
            }
        }
    }
    ctx.hub.unregister_session(&device_id, &session_id);
    if ctx.state.disconnected(&device_id, &session_id) {
        (ctx.sink)(LanEvent::new("bot_disconnected", Some(&device_id), &peer.to_string(), None));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ip_range_check() {
        assert!(is_local_peer("192.168.1.12".parse().unwrap()));
        assert!(is_local_peer("10.0.0.5".parse().unwrap()));
        assert!(is_local_peer("127.0.0.1".parse().unwrap()));
        assert!(!is_local_peer("8.8.8.8".parse().unwrap()));
    }
}

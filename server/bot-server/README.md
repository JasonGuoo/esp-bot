# ESP Bot Server — Rust + Tauri 2 console

This new server is intentionally isolated from the old Python Brain. It
advertises a LAN service, accepts Bot-initiated WebSocket connections and
displays live device sessions. It does not implement LLM access, tools, face
commands, audio, or main application integration.

## Run on macOS or Linux

Prerequisites: Rust stable, Node.js (supported by Vite 7), Tauri 2 CLI
prerequisites, and on Linux the WebKitGTK/GTK development packages.

    cd server/bot-server
    npm install
    npm run tauri dev

The application binds an ephemeral IPv4 TCP port on 0.0.0.0 and broadcasts
the actual port via Bonjour/mDNS. No port number or IP needs to be configured
on the ESP32. The console starts the service automatically in Tauri setup.

Check advertisement:
- macOS: dns-sd -B _espbot-server._tcp local
- Linux: avahi-browse -rt _espbot-server._tcp

Rust protocol/session tests (native Tauri prerequisites required):

    cargo test --manifest-path server/bot-server/src-tauri/Cargo.toml

## Module structure

- src-tauri/src/lan/discovery.rs: mDNS service registration
- src-tauri/src/lan/protocol.rs: strict hello/welcome schema and tests
- src-tauri/src/lan/socket.rs: WebSocket accept, handshake, heartbeat
- src-tauri/src/lan/mod.rs: in-memory snapshot, event sink and listener
- src-tauri/src/lib.rs: Tauri lifecycle and read-only snapshot command
- src/main.ts: minimal status / sessions / event console

Automatic accept means a compatible Bot's hello is accepted without a
server-side confirmation dialog. This is currently **unauthenticated**.
The only accepted application message is an inert JSON event forwarded to the
local event sink; there is no way to execute tools or control the machine.

Do not forward the port or operate it on an untrusted LAN. A later security
iteration should add durable device identities, pairing and authenticated
encryption before any sensitive device or server action is enabled.

See shared/lan-discovery-v1.md for the exact service/TXT/WebSocket contract.

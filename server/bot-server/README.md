# ESP Bot Server — Rust + Tauri 2 local brain console

## Current Mac implementation — 2026-11-06

**[小呆 Mac bot-server 完整指南](../../docs/mac-bot-server.md)** is the current setup
and implementation record. The main companion tab provides a device-independent path:
OpenCode v2.2.5 harness, Qwen Coding Plan → GLM Coding Plan → DeepSeek Flash,
inherited browser/deepseek-search, local ASR/TTS, explicit face/speaker enrollment,
SQLite memory, parent tool approvals, rich cards and stop/cancellation.

Use [`local-models/README.md`](local-models/README.md) for model installation,
[`tools/README.md`](tools/README.md) for MCP setup, and
[`../../docs/mac-gateway.md`](../../docs/mac-gateway.md) for the new TLS device endpoint.
`scripts/configure_mac.py` writes Mac paths; `brain.json` configures the new chain.
The historical provider selection below remains in `config.json`.

**Source-only delivery:** no tests, build, dependency resolution, model/API runs or
CI. Existing Cargo.lock predates the new direct dependencies and needs normal
resolution on the Mac. Firmware does not yet implement the WSS v3 client;
diagnostic LAN v1 is not private media transport. BLE remains pending.

## Historical diagnostic LAN console — 2026-10-31

The following describes the retained diagnostic link and legacy text adapters.
It is independent of the new companion tab and does not authenticate devices.

Implemented today (2026-10-31):

- **Persistent identity** — stable server id persisted in the app config
  dir so the firmware-side mDNS-TXT / welcome id match survives restarts.
- **Discovery + WebSocket transport** — mDNS advertise, hello/welcome
  handshake, heartbeat, per-device sessions (unchanged v1 link contract).
- **Pairing soft gate** — first-seen devices are pending and receive no
  commands until approved in the console; denied devices get welcome +
  Close 4003 `pairing_denied`. Persisted in `devices.json`. This is a
  trusted-LAN UI gate, **not** authentication.
- **Command push** — validated `face.set` request frames (v2 envelope)
  over the live socket with ack correlation and staleness deadlines.
  Commands to offline devices fail fast; there is no offline queue.
- **Provider adapters** — `Provider` trait with an OpenAI-compatible
  HTTP adapter (mlx-lm serve / LM Studio; ports the Python adapter
  verbatim) and an experimental HF Speech-to-Speech Realtime-style WS
  adapter (text-in/text-out only). Selected via `config.json`.
- **Chat** — console chat box → provider → reply pushed to the selected
  approved bot as `face.set {subtitle, expression:"happy"}`, mirroring the
  Python `/v1/chat` behavior; unconfigured providers answer with an honest
  `llm_not_configured` error, never a fake reply.

Not implemented on this legacy link: authenticated audio/STT/TTS streaming,
BLE provisioning/fallback, TLS or private media. The new Mac WSS service is
documented separately; firmware-side integration remains pending.

## Run on macOS or Linux

Prerequisites: Rust stable, Node.js (supported by Vite 7), Tauri 2 CLI
prerequisites, and on Linux the WebKitGTK/GTK development packages.

Development (vite dev server + hot reload, the window loads
`http://localhost:1420`):

    cd server/bot-server
    npm install
    npm run tauri dev

Standalone runnable binary (frontend assets embedded, no vite needed):

    npm run tauri build -- --no-bundle
    src-tauri/target/release/espbot-server-console

**Debug binaries need vite.** Any *debug* build (`cargo build`, or running
`target/debug/espbot-server-console` directly) is compiled in Tauri dev mode
and hardcodes the vite dev URL as its frontend — run without
`npm run tauri dev` and the window comes up **blank**. This is Tauri 2
behavior: `tauri-build` sets `cfg(dev)` for debug profiles, which makes the
webview prefer `build.devUrl` over the embedded `frontendDist`. The stderr
diagnostics make this visible instead of silent: a healthy start logs
`[window] main -> <url>` plus `[page] Started/Finished <url>` — blank-window
reports should start there.

The application binds an ephemeral IPv4 TCP port on 0.0.0.0 and broadcasts
the actual port via Bonjour/mDNS. No port number or IP needs to be configured
on the ESP32. The console starts the service automatically in Tauri setup.

Check advertisement:
- macOS: `dns-sd -B _espbot-server._tcp local`, or `python3 scripts/mdns_query.py`
- Linux: avahi-browse -rt _espbot-server._tcp

Rust unit tests plus an in-process fake-bot end-to-end session test
(hello → welcome → pairing gate → command → ack):

    cargo test --manifest-path server/bot-server/src-tauri/Cargo.toml

## Configuration

On first launch the app creates `config.json` in the Tauri app config dir
(macOS: `~/Library/Application Support/ai.reasonia.espbot.server/`):

    {
      "provider": "none",
      "openai": {
        "base_url": "http://127.0.0.1:8080/v1",
        "api_key": null,
        "model": "default"
      },
      "s2s": {
        "url": "ws://127.0.0.1:7860/v1/realtime"
      }
    }

Set `"provider"` to `"openai"` (with a `base_url`) or `"s2s"` (with a
`url`), then press **reload config** in the console. A malformed file never
crashes the app: the console shows `config_error` and keeps the previous
selection. `provider: "none"` keeps chat honestly disabled.

Files in the config dir: `server_id.txt` (stable identity),
`devices.json` (pairing registry), `config.json` (providers).

## Module structure

- src-tauri/src/lan/discovery.rs: mDNS service registration
- src-tauri/src/lan/protocol.rs: strict hello/welcome schema and tests
- src-tauri/src/lan/frames.rs: v2 request/response envelope + face.set validation
- src-tauri/src/lan/cmd.rs: CommandHub — outbound senders, ack correlation
- src-tauri/src/lan/socket.rs: WebSocket accept, handshake, heartbeat, outbound pump
- src-tauri/src/lan/e2e.rs: fake-bot end-to-end session test (no mDNS, no hardware)
- src-tauri/src/lan/mod.rs: snapshot, event sink, listener
- src-tauri/src/identity.rs: persistent server id
- src-tauri/src/pairing.rs: pairing registry with atomic persistence
- src-tauri/src/config.rs: provider config (load-or-default)
- src-tauri/src/provider/: Provider trait, OpenAI-compatible and S2S adapters
- src-tauri/src/lib.rs: Tauri lifecycle and console commands
- src/main.ts: console UI — pairing approvals, device/chat target, chat, provider status

## Security posture

Automatic LAN-only peer filtering is **not** auth. Pairing is a soft gate:
device ids are self-claimed, the link is unencrypted, and no secrets flow
here. Do not operate the port on an untrusted LAN. Durable device
identities, cryptographic pairing and an authenticated encrypted channel
are required before any audio leaves the Bot (docs/local-first-hybrid-brain.md).

## Validation honesty

Verified for real (2026-10-31, on the dev Mac, no board required):

- `cargo test`: 42 tests pass (41 by default, including the in-process
  fake-bot end-to-end session test, plus 1 opt-in live-HTTP test —
  see below).
- The **real app binary** launches, creates its config dir, and serves.
- **Real mDNS**: service `_espbot-server._tcp.local.` visible via raw
  DNS-SD query (scripts/mdns_query.py); TXT `proto/id/path` correct; the
  advertised id is stable across restarts (persistent identity works).
- **Real WebSocket handshake** from a foreign process
  (scripts/fake-bot.mjs): welcome `server_id` equals the mDNS TXT id.
- **Pairing paths against the real app**: first-seen device persisted as
  `pending` in devices.json; `denied` device receives welcome then Close
  4003 `pairing_denied`; `approved` device receives commands.
- **Full stack roundtrip** (via `VITE_ESPBOT_AUTOTEST=1 npm run tauri dev`):
  webview `invoke` → `send_face_command`/`chat` → provider HTTP →
  `face.set` frame delivered to the connected bot → ack correlated
  (`command_acked` in the stderr event log).
- **Real provider HTTP** through the actual adapter against a live local
  OpenAI-compatible endpoint (the repo mock):
  `node scripts/mock_llm_server.mjs && ESPBOT_TEST_OPENAI_BASE=http://127.0.0.1:8765/v1 cargo test -- --ignored real_http`.

Dev scripts (Node ≥ 22, zero deps): `scripts/fake-bot.mjs` (fake device:
handshake, events, receive/ack commands), `scripts/mock_llm_server.mjs`
(OpenAI-compatible stub), `scripts/mdns_query.py` (raw DNS-SD browser).

Still **not** verified: a live HF Speech-to-Speech sidecar (adapter is
protocol-mapped only), a real GPU model (mock stands in), real ESP32-S31
hardware (the firmware does not yet consume app frames — see
shared/lan-discovery-v1.md), and literal mouse-clicks in the window (the
same webview→IPC→command path is exercised by the autotest hook instead).

Console events are also printed to stderr, so headless runs of the binary
can be diagnosed without the window.

See shared/lan-discovery-v1.md for the exact service/TXT/WebSocket/app-frame
contract.

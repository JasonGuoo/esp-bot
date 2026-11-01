# ESP Bot local discovery / link protocol v1

**Scope:** LAN discovery, transport and (since 2026-10-31) server→device
text command frames plus a server-side pairing soft gate. No Internet
discovery, cloud broker, speech/audio, or binary media on this link. The
previous Python HTTP polling prototype is unchanged and retires later.

## Discover

- mDNS/DNS-SD PTR service type: _espbot-server._tcp.local.
- The server advertises its **actual listening TCP port** (selected dynamically).
- SRV target: espbot-<8-character-id>.local.
- TXT properties: proto=1, id=<server id>, path=/v1/lan/ws.
- The advertised id is the server's **persistent identity**
  (`server_id.txt` in the console's app config dir), stable across
  restarts — not a per-boot UUID — because the Bot rejects a welcome whose
  server_id differs from the discovered TXT id.
- Clients obtain IPv4 addresses from DNS-SD and prefer their STA subnet.
- Server starts advertising and accepting device connections automatically
  when the Rust/Tauri console launches.
- Exactly one compatible server: Bot may auto-connect. Multiple servers:
  require manual choice. None should be chosen at random.

## Connect

HTTP/1.1 WebSocket upgrade to ws://<discovered-ip>:<port>/v1/lan/ws.
The first text frame must arrive within 7 seconds:

    {"type":"hello","protocol":1,"device_id":"korvo-01","name":"Desktop Bot"}

Server validates the version, ID (1..64 ASCII alphanumeric / _ / -) and name
(1..64 UTF-8 bytes, no control characters) then answers:

    {"type":"welcome","protocol":1,"server_id":"<advertised-id>","heartbeat_sec":25}

Bot verifies the server_id equals the value in DNS-SD. This is an *advisory*
check, not cryptographic authentication. Server sends WebSocket pings; the Bot
client responds with automatic pongs. Stale links expire on inbound inactivity.

Once welcomed, Bot may send JSON text frames:
- event frames, e.g. `{"type":"event","topic":"button.pressed","value":1}`
  — forwarded to the local event sink / Tauri event "lan:event";
- **response frames** acknowledging server requests (see below).

Server maximum text frame is 2048 bytes; Bot send limit is 512 bytes.
No binary/media transport in protocol v1.

## App frames (v2 envelope, 2026-10-31)

After the unchanged v1 handshake, server→device commands use the single
versioned envelope from docs/local-first-hybrid-brain.md §5 — one
request/response/error shape instead of per-feature endpoints. The only
method implemented today is `face.set`; unsupported methods must be
rejected by the device, not ignored.

Server → Bot request (total frame ≤ 2048 bytes, enforced server-side):

    {"v":2,"type":"request","id":"c17","method":"face.set",
     "session_id":"<bot session id>","deadline_ms":45000,
     "sent_at_ms":1770000000000,
     "payload":{"theme":"mint","expression":"happy","subtitle":"Hi!",
                "gaze_x":0,"gaze_y":0}}

Bot → Server ack:

    {"v":2,"type":"response","id":"c17","ok":true,"detail":"applied"}
    {"v":2,"type":"response","id":"c17","ok":false,"error":"stale|unsupported_method|bad_payload|busy"}

Rules:
- `deadline_ms` is a **relative age budget**. Devices must interpret
  freshness locally and reject expired commands (`error:"stale"`); a stale
  command is never executed late. Server wall-clock time must not be
  compared to device uptime.
- Payload key allowlist: `theme`, `expression`, `subtitle`, `gaze_x`,
  `gaze_y`; all optional, only supplied fields apply. Server-side
  validation: theme ∈ shared/themes.json ids, expression ∈ the nine Face
  expressions, subtitle ≤ 180 chars, gaze integer −100..100.
- Server-to-device `gaze_x`/`gaze_y` exist for **manual renderer testing
  only**; the server never generates per-frame gaze and runtime may refuse.
- Acks are correlated by `id`; unknown or duplicate acks are logged and
  ignored. Commands to offline devices fail fast on the server (no queue).
- The device-side consumer of these frames is not yet integrated into
  firmware `app_main`; only the transport-forwarding (`lan_link`) exists.

## Pairing soft gate (2026-10-31)

First-seen devices are recorded **pending** in the console's `devices.json`
and shown for Approve/Deny. Pending devices may connect and send events but
**never receive command frames**. Denied devices receive the welcome and
then a WebSocket Close (code 4003, reason `pairing_denied`).

This is a **UI gate on a trusted LAN, not authentication**: the device_id
is self-claimed, the link is unencrypted, and no secrets may flow until a
persistent-key pairing and an authenticated channel exist.

## Lifecycle and trust

- Wi-Fi STA provisioning belongs to the firmware application; lan_link
  starts only when its API is explicitly invoked.
- mDNS uses link-local multicast UDP/5353; it normally requires the same
  layer-2 broadcast domain.
- A successful welcome is **not secure pairing**. Ids are public metadata;
  this version has no cryptographic authentication or encryption.
- Rust listener binds an OS-selected port on 0.0.0.0 but rejects non-local
  source addresses at upgrade. Configure host firewall for trusted LAN only.
- No device-initiated tool access and no secrets on the link until pairing
  and an authenticated channel have been implemented.
- Auto-reconnect is through a new discovery scan. Manual disconnect pauses it
  until explicitly resumed. Pairing state persists on the Mac; session state
  is memory-only.


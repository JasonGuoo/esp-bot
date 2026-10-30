# ESP Bot local discovery / link protocol v1

**Scope:** LAN discovery and transport only. No Internet discovery, cloud
broker, LLM access, speech, face control, or changes to the previous Python
HTTP polling prototype. Modules are stand-alone until main integration.

## Discover

- mDNS/DNS-SD PTR service type: _espbot-server._tcp.local.
- The server advertises its **actual listening TCP port** (selected dynamically).
- SRV target: espbot-<8-character-id>.local.
- TXT properties: proto=1, id=<server UUID>, path=/v1/lan/ws.
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

    {"type":"welcome","protocol":1,"server_id":"<advertised-UUID>","heartbeat_sec":25}

Bot verifies the server_id equals the value in DNS-SD. This is an *advisory*
check, not cryptographic authentication. Server sends WebSocket pings; the Bot
client responds with automatic pongs. Stale links expire on inbound inactivity.

Once welcomed, Bot may send an optional JSON text event frame, e.g.:

    {"type":"event","topic":"button.pressed","value":1}

Rust only forwards these to the local event sink / Tauri event "lan:event".
No execution, forwarding to LLM/tools, or server-to-device control is enabled.
Server maximum text frame is 2048 bytes; Bot send limit is 512 bytes.
No binary/media transport in protocol v1.

## Lifecycle and trust

- Wi-Fi STA provisioning belongs to the firmware application; lan_link
  starts only when its API is explicitly invoked.
- mDNS uses link-local multicast UDP/5353; it normally requires the same
  layer-2 broadcast domain.
- A successful welcome is **not secure pairing**. UUIDs are public metadata;
  this version has no cryptographic authentication or encryption.
- Rust listener binds an OS-selected port on 0.0.0.0 but rejects non-local
  source addresses at upgrade. Configure host firewall for trusted LAN only.
- No device-initiated tool access, no server-to-device control, no secrets on
  the link until pairing and an authenticated channel have been implemented.
- Auto-reconnect is through a new discovery scan. Manual disconnect pauses it
  until explicitly resumed. Server state is memory-only.

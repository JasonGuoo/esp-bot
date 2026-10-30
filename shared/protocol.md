# ESP Bot Brain protocol v1 (HTTP / JSON)

Scope: a minimal working LAN command protocol. ESP32 **polls** the Brain, which keeps an in-memory queue per device. This is a transport boundary, not an audio/video protocol.

## Security

- Server default bind is `127.0.0.1:8766` (only local machine).
- To listen on LAN, set `ESPBOT_BIND=0.0.0.0` and a nonempty `ESPBOT_TOKEN`; the server refuses to start if the token is missing.
- Requests other than `GET /health` must include `Authorization: Bearer <token>` when a token is configured.
- HTTP is **unencrypted**. Use only trusted LAN during development; production requires TLS or an authenticated encrypted transport and secure pairing. Do not expose directly to the public Internet.
- No arbitrary client-supplied tool code, URLs, shell commands or file paths are executed.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| GET | `/health` | Process health (public) |
| GET | `/v1/themes` | Appearance-pack descriptors (renderer, palette, motion) |
| GET | `/v1/expressions` | Available expression IDs |
| GET | `/v1/devices` | Connected/recently polled devices |
| GET | `/v1/commands/next?device_id=korvo-01` | Dequeue next command or `{"command":null}` |
| POST | `/v1/commands` | Enqueue a validated command for a device |
| POST | `/v1/devices/events` | Receive simple telemetry/event (not media) |
| POST | `/v1/chat` | Optional LLM text response, enqueue subtitle |
| GET | `/v1/tools` | List explicit server-side allowed tools |
| POST | `/v1/tools/call` | Explicit allowlisted tool invocation |

### Device command example

```http
POST /v1/commands
Content-Type: application/json
Authorization: Bearer <token>

{
  "device_id": "korvo-01",
  "type": "face.set",
  "payload": {"theme": "sakura", "expression": "happy", "subtitle": "你好！"}
}
```

Returns `{"ok":true,"command_id":1}`, then:

```http
GET /v1/commands/next?device_id=korvo-01
Authorization: Bearer <token>
```

```json
{"command":{"id":1,"type":"face.set","payload":{"theme":"sakura","expression":"happy","subtitle":"你好！"}}}
```

Expressions: `idle`, `listening`, `happy`, `curious`, `thinking`, `surprised`, `sleepy`, `shy`, `blink`.

Only supplied payload fields are updated. `face.set` may specify any subset of `theme`, `expression`, `subtitle`, `gaze_x`, `gaze_y`. Gaze integer range: -100..100; subtitle string <= 180 Unicode code points. Empty string clears subtitle. Each appearance pack chooses a renderer, palette and animation profile; both eyes share the **same target gaze vector**, regardless of renderer. A remote `face.set` can change appearance temporarily, but the on-device Settings → Appearance → Apply flow persists a selection in NVS.

Commands are removed from the queue on polling and are **not durable**; delivery/ack/retry are future work. The ESP32 network task must always acquire the LVGL/BSP display lock before updating the face. Devices must not invoke external LLM services or hold provider API keys.

### Text model

`POST /v1/chat` with `{"device_id":"korvo-01","message":"你好"}`; if `ESPBOT_LLM_BASE_URL` and `ESPBOT_LLM_MODEL` are unset, return HTTP 503 (`llm_not_configured`), not a fake answer. When configured, calls server-owned OpenAI-compatible `/chat/completions` using the optional `ESPBOT_LLM_API_KEY` and queues a `face.set` command with subtitle text.

### Tools

The Brain's tool registry only executes **registered named functions** with validated parameters. Initial examples: `clock.utc`, `math.add`. LLM automatic tool selection, user consent policies and external MCP providers are future milestones.

### Discovery (planned)

Wi-Fi discovery via mDNS/UDP, secure pairing and BLE provisioning are **not implemented** in this version. For the first integration, configure a fixed `Brain URL` on the ESP32 via ESP-IDF menuconfig. The touch-revealed gear opens a full-screen **Settings** menu, then **Appearance**, where the user selects a candidate and taps **Apply** to return to the face. This menu does not pretend Bluetooth or Wi-Fi discovery has been implemented.


### Human eye-contact integration (future, NOT in this HTTP API yet)

Real tracking will not be a series of arbitrary `face.set gaze_x/gaze_y` guesses. The future camera/vision pipeline must report a timestamped target track ID, camera-relative bearing angles, face confidence and a **separate** person-looking-at-bot estimate; the future motion driver reports actual head pose. The C Attention coordinator converts these to a shared pupil gaze vector and bounded pan/tilt commands. Brain-to-device target transport, clock synchronization, face/gaze inference and motor actuation are **not implemented**. The current Brain `face.set` gaze_x/y is a manual renderer-testing interface only. See [docs/eye-contact.md](../docs/eye-contact.md).

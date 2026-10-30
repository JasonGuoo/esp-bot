# Local-First Hybrid Brain — product architecture decision (2026-10-30)

**Status: NEW SELECTED DESIGN, not implemented end-to-end. Supersedes older assumptions that Mac/Linux is only a generic optional HTTP Brain and that every new capability gets its own route.**

## 1. Locked product requirements

- Device remains **小呆**, ESP32-S31-Korvo-1 V1.1 development board (16 MB flash, 16 MB PSRAM, 800×480 touch LCD, camera, twin mics, integrated ES8389 and **two NS4150B 3 W speaker power amps**, outputs to **4 Ω / 3 W speakers via 2.00 mm pitch ports**; physical speaker units are separate). No replacement base board until proof of concept.
- **Primary Brain:** user's **Apple Silicon M4 Pro Mac, 48 GB unified memory**, running **Rust/Tauri bot-server**. Optimize for **near-zero cloud AI spend when local resources suffice**, **low time-to-first-audio** and privacy; model processes may be separate local sidecars, notably MLX/Speech-to-Speech.
- Prefer **existing successful software/hardware modules**: see [upstream-reuse-catalog.md](upstream-reuse-catalog.md). New code should be glue, permissions, session ownership or uniquely 小呆 logic, not a fresh speech framework.
- **Wi-Fi LAN first**, automatic mDNS discovery of **previously paired** bot-server, never trust raw mDNS UUID as authentication.
- **Bluetooth required for Mac pairing/provisioning and degraded communication if Wi-Fi is unavailable**. BLE is guaranteed target for control/bootstrap; experimental compressed speech over BLE is **conditional on measured throughput/latency**. Not promise full speech/video when Wi-Fi fails. Bluetooth radio presence does not prove availability of the desired HFP/A2DP/LE Audio software/profile combination.
- Robot must maintain its **full four-syllable local name-wake 小呆小呆**, passive behavior and optional local eye gaze without Mac, Wi-Fi or cloud. The wake model is **not yet created**. Never upload ambient pre-wake audio.
- **Target physical product now includes attractive enclosed head, audible external speakers and optional 2-axis pan/tilt using pre-made parts**. Existing V1 motorless/fixed-head documentation describes the *currently installed* hardware only, not a permanent product decision. Hardware/mechanics are not yet selected or assembled.
- Allow explicit remote/cloud policy as an opt-in fallback; local service available/healthy wins, but never use a bad/incomplete local answer as if it is fresh internet data.

## 2. Exactly one local decision authority

bot_runtime owns wake, active conversation, connection preferences, failover permissions and behavior priority. bot_bus carries typed events/latest snapshots, not AI actions or audio data. attention_manager owns tracked human focus. vision_attention/attention/face/head_motion retain their existing single ownership and safety contracts. bot_platform/audio owns one microphone/codec. The server may request a **high-level** action with expiry/permission but cannot steer servo or screen pupil on each frame.

### Architecture layers

1. **ESP device**: ESP-IDF official BSP + compatible XiaoZhi board port → mic/wake/audio render → bot_runtime + Face/attention + device UI → brain_link.
2. **Transport selector** beneath brain_link: Wi-Fi LAN WS (primary), Bluetooth local link (bootstrap/backup), device-local skills (last resort). No automatic cloud voice upload without explicit policy.
3. **Mac bot-server (Rust core + Tauri console)**: pairing registry, capability advertisement, session/routing, access policy, local model health metrics, tools, media catalog, telemetry.
4. **Local inference sidecars**: existing HF Speech-to-Speech process on localhost (STT+LLM+TTS), optionally MLX/llama.cpp/Whisper/Piper/Wyoming services; Rust adapters, not rebuilt inference.
5. **Optional cloud providers/search**: may be invoked by Mac for tasks whose abilities truly need them (fresh data or heavier reasoning); no cloud model call if policy disallows. The device Cloud Direct mode is a future separately authorized product feature, not included in present code.

## 3. Local-first selection policy (explicit and measurable)

**Route each capability individually** (STT, LLM, TTS, web search, vision), not a monolithic "local versus cloud" switch. Default profile: **Auto — prefer local bot-server**.

1. Boot: always launch local wake, face and passive policy. If Wi-Fi has usable IP, discover via mDNS then validate **stored paired server public key**; connect and negotiate protocol and capabilities. Existing LAN v1 has an ephemeral server UUID, **NOT** stable trust; must be upgraded before private audio/tools.
2. On Mac, read each local provider's availability, warm status, language/task support, measured p95 first-response and queue depth. A healthy local provider within configured latency/quality budget wins.
3. When local model is missing, overloaded or unsupported, choose approved **Mac-mediated cloud provider** only after cost/privacy policy check. A search request may require internet **data retrieval** while its speech, summary and TTS remain entirely local.
4. Wi-Fi unavailable: try BLE to previously paired Mac; support device status, pairing, Wi-Fi credentials and low-bandwidth commands. Start a **separate measured BLE speech proof-of-concept** with short compressed frames (Opus/ADPCM) and explicit congestion control if practical; if not, gracefully degrade to offline commands and local screen UI. No claim that a Bluetooth speaker HFP profile automatically carries the app's bidirectional RPC.
5. If Mac absent: remain locally functional; Cloud Direct only if enabled and credentials are safely provisioned. Never fail open by sending sensitive speech to cloud.
6. Reconnect with jitter/backoff; avoid path flapping (hysteresis); preserve conversation/session semantics, cancel partial streams on transport switch, do not duplicate physical actions or automatically replay private audio.

**Profiles:** Auto local-first (default), Local Only, Cloud Direct (explicit). Separate toggles: cloud model escalation on Mac, direct cloud when Mac absent, usage caps, and whether BLE degraded speech is enabled after hardware verification.

## 4. BLE vs Wi-Fi: make two independent features

**A. BLE onboarding (required):** Use Espressif maintained **network_provisioning** / supported protocomm BLE transport and proper mutual proof-of-possession with authenticated encryption, instead of inventing credential serialization. Mac CoreBluetooth is the central/controller; Bot peripheral advertises a narrowly-scoped provision service. Flow:

    No Wi-Fi / first boot → Bot BLE advertises provisioning
    → Mac bot-server detects "小呆", user approves/pairs on console
    → provision Wi-Fi credentials over protected handshake
    → Bot joins Wi-Fi → mDNS discovers same trusted Mac
    → Wi-Fi WS stream becomes primary, BLE remains on-demand fallback

For S31 the chip has Classic BT and BLE 5.4 at hardware level; **macOS Bluetooth role/profile support and installed ESP-IDF stacks must be checked for actual software support**. For a normal macOS app, BLE GATT via CoreBluetooth is a safer universal starting point than assuming HFP/LE Audio can form a private, bidirectional application data channel.

**B. BLE app channel (required control, exploratory speech):** Reuse same high-level message envelope but a **BLE framing adapter** with MTU-aware chunking, sequence/ack, congestion, bounded queues, encryption, pause/resume, close/timeouts and session binding. Provisioning may reuse ESP-IDF components; app data protocol is distinct from provision security. BLE does not carry JPEG/video by default. Audio on BLE is not committed until full-duplex audio+mic speaker+A​EC coexistence and actual Mac–S31 throughput tests pass. Do not silently send uncompressed 16k/16-bit PCM over a small GATT characteristic.

**Security:** persistent device + server key/identity; user approval on first bond, host authorization, pair rotation/revocation, BLE Secure Connections where supported, channel binding/anti-replay, proof-of-possession in provisioning; protect NVS credentials. Neither BLE discovery nor mDNS name/UUID is an identity check.

**Source references:** [ESP-S31 BLE stack](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32s31/api-guides/ble/overview.html), [new S31 network provisioning recommendation](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-guides/ble/blufi.html), [ESP-IDF provisioning](https://github.com/espressif/esp-idf/blob/master/docs/en/api-reference/provisioning/provisioning.rst), [ESP Bluetooth Audio overview](https://docs.espressif.com/projects/esp-adf/en/latest/solution-center/bluetooth-audio.html).

## 5. Do not explode the API

**One versioned request/event/control envelope**, not a new REST endpoint for each ability. Proposed evolution from current LAN v1 hello/event:

    { "v": 2, "type": "request", "id": "r123", "method": "assistant.turn.start",
      "session_id": "s456", "payload": {"input":"audio","stream_id":7} }

Standard types: hello, welcome, request, response, event, error, cancel. Standard fields: protocol version, message id, session id, turn id, deadline, content, trace id, idempotency key (for non-repeatable actions), error codes. Capabilities are negotiated; unsupported methods result in explicit errors. Use WebSocket ping/pong for Wi-Fi heartbeat; BLE transport has its own connection supervision; don't introduce high-rate JSON heartbeat.

For sound, use **binary frames with stream_id, format, sequence, timestamp**, bounded buffers, backpressure and cancellation. For images/video, use server-side image processing/transcoding and explicit media descriptors, then separate authenticated HTTP-range/streaming path if Wi-Fi. Video playback is optional and requires board codec profiling; prefer official ESP-GMF render over custom decoder. BLE accepts metadata/very small previews only where budget allows, never assumes full video.

## 6. Mac-first audio pipeline — prioritize feel over absolute model size

After device-local wake, the **Mac** receives the explicit user session's audio. Prefer [HF Speech-to-Speech](https://github.com/huggingface/speech-to-speech) running **locally** as a sidecar; on Apple Silicon it exposes MLX models and a Realtime-compatible WS endpoint. Rust bot-server bridges proprietary device codec/framing to it.

    ESP mic → local AFE/wake → Wi-Fi audio frames
      → Rust bot-server authenticated session
      → local speech service VAD/STT
      → local MLX LLM (with permissioned Mac search/tools if needed)
      → streaming local TTS
      → Rust audio packets → ESP codec speaker(s)

Keep each service replacable through adapter capabilities (`stt.transcribe`, `llm.respond`, `tts.synthesize`, `web.search`). Use VAD and TTS chunks for early playback, cancellation/barge-in, AEC while speaker is active, warm-start policy and latency instrumentation (capture→STT, first LLM token, first TTS packet, first speaker audio). No promise of particular delay until instrumented on **this** Mac.

Text-only queries should not require audio models; on-device offline commands should not wait for Mac. Startup should not pre-load every model merely because 48 GB exists; keep a configurable active set and enforce memory pressure.

## 7. What exists vs. work still required

**Already in repo:** Rust/Tauri mDNS+WebSocket skeleton, ESP-IDF lan_link, Face, attention and vision_attention modules, legacy Python Brain. **Not implemented yet:** BLE provisioning/bridge; secure pairing; identity persistence; binary media; Mac speech-to-speech integration; local MLX model management; TTS/audio sessions; server-side search; movable head motor hardware/driver; full-word wake acoustic model.

### Recommended next development gates

1. Verify XiaoZhi S31 board port + official BSP/codec and write minimal adapter map; no wholesale fork.
2. Mac Rust service supervisor/adapters for installed HF Speech-to-Speech and local LLM; plain local loopback process health.
3. Pairing/identity and secured LAN channel before microphones/audio leave Bot.
4. BLE Mac provisioning, then BLE app-control fallback; performance-gate compressed BLE speech.
5. Audio session+media framing, interrupt/stop, command/event correlation; MediaScene JPEG player as later adapter.
6. Identify a finished enclosure + reusable 2-DOF pan/tilt kit, add stereo speakers; validate power/servo protection before actuation. Prefer vendor CAD and hardware reference design over writing new mechanics.

## 8. Sources / decisions to preserve

- [XiaoZhi on exact board](https://github.com/78/xiaozhi-esp32/blob/main/main/boards/espressif/esp32-s31-korvo-1/README.md)
- [Official board audio amp and speaker connectors](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)
- [HF Mac local end-to-end speech reference](https://huggingface.co/blog/local-reachy-mini-conversation)
- [HF Speech-to-Speech source and adapters](https://github.com/huggingface/speech-to-speech)
- [ESP-SR wake and AFE supported targets](https://github.com/espressif/esp-sr)
- [ESP-GMF media/audio example catalog](https://github.com/espressif/esp-gmf/blob/main/gmf_examples/README.md)
- [Home Assistant Wyoming architecture](https://www.home-assistant.io/integrations/wyoming)
- [S31 Classic/LE Audio software documentation](https://docs.espressif.com/projects/esp-adf/en/latest/solution-center/bluetooth-audio.html)

**No CI. No claims of board build or Mac voice performance testing.**

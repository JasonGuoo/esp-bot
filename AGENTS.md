# esp-bot development instructions

## Delivery and testing policy

- **Do not create or configure CI pipelines** for this project. Do not add GitHub Actions verification jobs or treat CI as a merge prerequisite.
- Prioritize delivering modular, reviewable source code and clear integration contracts. If a real device, toolchain or practical test environment is unavailable, **write the code without attempting or requiring tests**.
- State the verification status precisely in the handoff: distinguish written code from builds, device runs, and camera/motor calibration actually performed. Never call an untested implementation "verified".
- Do not add testing infrastructure or workflow changes solely for reassurance. Existing local/manual test examples may remain for optional future use; do not execute or enforce them without a concrete reason or explicit request.
- Preserve separation between `vision_attention` (all face tracks and eye estimates), `attention` (selected target + head/eye motion planning), `face` (LVGL expression rendering), and server modules.
- Hardware target: **ESP32-S31-Korvo-1 V1.1 / ESP32-S31**, not ESP32-S3.

## NEW direction 2026-10-30: reuse-first, Local-First Hybrid Brain

- **Read [docs/local-first-hybrid-brain.md](docs/local-first-hybrid-brain.md) and [docs/upstream-reuse-catalog.md](docs/upstream-reuse-catalog.md)** before planning new device, audio, server, Bluetooth, or mechanical work. These supersede the former "build all components" assumption while retaining module ownership and passive behavior.
- **Reuse upstream successful components** before inventing drivers/voice/media systems. **XiaoZhi already supports our exact S31-Korvo-1 V1.1 board**, and Espressif has official BSP/ESP-SR/GMF.
- **Preferred server is Rust/Tauri bot-server on user's Mac M4 Pro with 48 GB unified memory**; all audio/dialogue prioritizes local models on Mac. AI engines may run as existing MLX/Python sidecars. Cloud is optional and policy-gated.
- **Bluetooth is required:** Mac BLE pairing + secure Wi-Fi provisioning, and a low-bandwidth command fallback if Wi-Fi is unavailable; streaming audio over BLE is a measured research gate, never assumed. Preserve offline device wake.
- **Target physical robot is a good-looking enclosed device with real speakers and pre-made 2-axis pan/tilt**, not a custom PCB or hand-built mechanical drive. The installed V1 configuration remains fixed and motorless until actual compatible hardware is acquired and validated. Motor/IMU APIs must never pretend hardware is installed.
- Do **not** copy Reachy Mini's CC BY-SA-NC hardware designs into a commercial shell; check all source, CAD, voice model, and assets licenses.
- No CI; do not claim unrun hardware tests.

## Canonical project architecture and product constraints (2026-10-30)

- Read [docs/architecture.md](docs/architecture.md) FIRST. It is the overall authority; see [docs/module-contracts.md](docs/module-contracts.md) for data ownership/events and [docs/implementation-roadmap.md](docs/implementation-roadmap.md) for actual progress versus unimplemented design. Do not treat older Python Brain descriptions as the selected new architecture.
- Name: **小呆**. The ONLY production wake phrase is **小呆小呆**, interpreted as a **single complete four-syllable keyword**; one standalone **小呆** is NOT a wake. The dedicated acoustic model is NOT yet provisioned. Name configuration alone cannot make a recognizer work. See [docs/wake-word.md](docs/wake-word.md).
- **Passive companion by default:** only explicit wake or intentional user touch/manual action initiates conversation; face presence, looking at Bot, speech energy, other conversation and acoustic DOA are NOT wake events. Idle → bored → drowsy → sleeping; night dark should keep local audio keyword recognition running while camera and screen are inactive. See [docs/behavior-lifecycle.md](docs/behavior-lifecycle.md).
- **Currently installed V1 hardware:** fixed, non-actuated camera/head that may be moved by hand; **no installed IMU, servo or joint feedback**. **Revised physical target** is an enclosed speaker-equipped two-axis moving head using off-the-shelf parts; do not fabricate capability before real installation. Visual follow means rendered eyes update from CURRENT camera-relative human eye position if a visible target remains in frame. Software cannot turn camera to reacquire an off-screen person. The rendered eyes should not auto-recenter while the head has not physically aligned. Future motors/IMU are optional adapters. See [docs/eye-contact.md](docs/eye-contact.md), [docs/hardware-power.md](docs/hardware-power.md).
- **One authority for each decision:** bot_runtime owns local behavior/lifecycle and command arbitration; bot_bus transports events/commands/snapshots but makes no decisions; attention_manager owns the single official human focus; vision_attention owns multi-person perception, eye landmarks and human look-at-Bot evidence; attention owns eye/head spatial planning; face renders; head_motion owns physical actuator safety when present. No server/LLM per-frame motor or pupil instructions.
- **Audio single owner:** microphone capture is shared by local keyword recognizer and optional DOA, never independently opened twice; unrecognized pre-wake audio is neither persisted nor uploaded. Rust/Tauri bot-server is the chosen future server, but current Rust LAN and ESP lan_link are standalone and not yet the integrated Brain.
- Keep contracts versioned, capture times monotonic for local control, explicit freshness/validity flags, ephemeral track IDs and distinct human-eye LOCATION versus human-GAZE estimates. No false claim of mutual eye contact when camera is unavailable or too dark.
- When writing code, state what exists, what is only documented, and what requires absent board/model/motor resources. Respect the **no CI** project policy above even when adding new modules.

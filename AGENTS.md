# esp-bot development instructions

## Current Mac and device behavior — 2026-11-18

- Managed model keys come only from the current App session or its launch environment. Do not read, migrate, overwrite, or remove legacy provider Keychain items. This does not change device pairing or signing.
- The installed S31 has a working external speaker; preserve its owner-selected volume. The paired Mac can bind managed GLM, DeepSeek and Qwen HTTPS requests to the active Wi-Fi or Ethernet interface. An HTTP response proves reachability, not model inference. See the [November 18 local acceptance and VPN limitation](docs/review-network-speech-2026-11-18.md).

## Xiaodai core is the default brain — 2026-11-18

- `server/xiaodai-core` (crate `xiaodai-fast-responder`) is vendored source; the bot-server depends on it by path. Keep it free of `runtime/`, `target/`, `.env` and secrets, and never copy from `~/GitHub/codex-dialed`.
- Ordinary spoken turns go to the core (dual: Luna first, GLM/DeepSeek after 2 s). OpenCode keeps tool keyword requests, private-profile turns, pending task input, the core's `〔转交〕` escalation, core errors/timeouts and `agent.work`. Switch off with `brain_core.enabled=false` in `brain.json`.
- The core gets the same memory context as OpenCode and its keys from the App session or launch environment. Luna's thread resets on a new household day, `memory_changed` or `identity_revoked`.
- Isolated live acceptance passed on the Mac; no device or installed-App run yet. See [docs/review-xiaodai-core-2026-11-18.md](docs/review-xiaodai-core-2026-11-18.md).
- Cargo and git need `DEVELOPER_DIR=/Library/Developer/CommandLineTools` while the Xcode licence is unaccepted.

## Accepted design direction — 2026-11-16

- Read [the household companion design](docs/specs/household-companion/README.md)
  and [HC-01–HC-15 implementation plan](docs/specs/household-companion/implementation-plan.md)
  before new architecture work. The [November 16 implementation record](docs/specs/household-companion/implementation-2026-11-16.md)
  describes the original source-only delivery. The subsequent [local and S31
  acceptance](docs/review-household-acceptance-2026-11-16.md) records the builds,
  actual model calls and physical runs, and distinguishes the remaining gaps.
  The [November 17 follow-up](docs/review-household-acceptance-2026-11-17.md)
  records isolated speaker, face, memory and Graphiti runs plus the AEC failure.
  The accepted direction supersedes older person-isolated
  conversation, device Cloud Direct and monolithic turn/task assumptions below.
- The [second static review](docs/review-household-static-2026-11-16.md) records
  subsequent source fixes for speaker/private scope, activation forgetting,
  source cleanup, task cancellation, print reconciliation and segmented output.
  The review itself did not run builds, tests, SQL, models or hardware; use the subsequent acceptance record for runtime evidence.
- Keep Bot a responsive terminal connected to its paired Mac. Device owns
  complete local wake, one audio owner, local stop, bounded playback and runtime
  arbitration. Mac owns ASR/identity, OpenCode, household memory, presentation
  preparation and persistent tasks. No standalone device Internet Agent in this scope.
- After authorized wake, run ASR and speaker recognition in independent managed
  lanes. Identity may arrive late; ordinary responses must not wait for it.
  Speaker labels never grant permissions or cause tool replay.
- Use one household daily record in the household timezone, with people as
  participants/entities. Separate archive, OpenCode context/compaction,
  background memory, device activation, task and output-lease lifecycles.
  Preserve original sources, corrections, deletion and existing private scopes.
- SQLite remains authoritative; the pinned Graphiti v0.31.2 adapter is a
  rebuildable derived index. Isolated FalkorDB and local model integration was
  exercised on November 17; Neo4j and production deployment remain untested,
  and enabling configuration does not prove readiness. Reuse upstream implementations
  rather than adding a competing agent runtime or a new memory framework.
- Treat presentation as a first-class server responsibility: frozen semantic
  segments, immutable assets, synchronized bounded playback and actual receipts.
  ACK/progress completion must not end the task; report real task stages.
  Printing requires an explicit intent and traceable, idempotent effect receipts.
- The firmware [integration contract](firmware/HOUSEHOLD_INTEGRATION.md) records
  negotiated segments, output leases, hardware/display receipts and the real
  DAC-reference AEC path. AEC and acoustic barge-in default off; retain the
  half-duplex fallback. Never treat source integration as acoustic validation.
- Historical delivery notes below describe their original snapshots. Previous
  hardware/build evidence is in [2026-11-15 acceptance](docs/review-acceptance-2026-11-15.md)
  and does not validate this revision. Do not infer full HC-01–HC-15 or #7–#11
  acceptance from the subset exercised on November 16. The no-CI and precise verification rules below
  remain unchanged; neither design nor source merge implies a new device pass.

## User's research and reuse priority

- Before implementing or debugging a feature, first inspect applicable official
  examples and reuse working implementations where possible. Next consult the
  official development documentation; then review other open-source projects.
  Prefer adapting supported existing components to building replacements.
- For requested firmware work, proceed with source changes, local builds, and
  routine application-only flashing/verification on the identified development
  board without repeatedly asking for confirmation. Preserve provisioning and
  the model/data partitions. For networking, use the matching official client
  and server examples as the first real-board comparison.
- Exercise state changes and display layout locally with the project ESP-IDF/LVGL
  versions first. For routine speech and screen checks, use Mac speech synthesis
  next to the board microphone and the official Bot Server USB screen preview.

## Delivery context

- **2026-11-13 Mac-backed voice extension:** The user explicitly keeps Mac as the conversation backend. Read [voice-interaction.md](docs/voice-interaction.md) for the current sound UI, NVS settings, active-session fixed commands, capture-ready/onset behavior and opt-in Mac speech. `bot_voice` contains driver-free shared settings; `bot_runtime` is still the sole authority. PREVIEW playback is an explicit settings-only lease with no session/mic token. Passive MultiNet has only the complete wake phrase; fixed commands join its graph only in authorized listening. Prepare the next turn token before WAITING, never relabel earlier PCM. `REPLY_PRESENT` retains text while hardware playback remains pending; actual playback completion arms follow-up, and low-frequency successful hardware writes alone renew playback progress. Image results keep their own reading token during prepared follow-up. Default `output.audio=false` remains compatible; `audio.reply.v1` opt-in uses the existing Mac TTS and hardware playback receipts. This extension has host runtime checks, but no S31 IDF or macOS build, real model call, flashing or physical playback acceptance in its delivery environment. No CI.

- **2026-11-13 local wake and responses:** Source now defaults to official ESP-SR 2.6.1 **MultiNet7 `mn7_cn`**, continuously recognizing the one complete command `xiao dai xiao dai`; it never combines two short-name events. A dedicated WakeNet model is still not provisioned. `bot_responses` includes 104 real offline Chinese clips; `bot_runtime` alone selects/authorizes an acknowledgement and owns its expression/caption. `wake_response_player` streams through the existing audio owner. The microphone/remote session gate stays closed during an acknowledgement; Stop/settings/new turns invalidate queued playback. Read [wake-test.md](docs/wake-test.md) for configuration migration, model backup/update and on-board acceptance. This update has no S31 IDF build or physical-board verification in the delivery environment; do not claim measured recognition accuracy or speaker output.

- **2026-11-13 model selection:** The active default order is **GLM Coding Plan → DeepSeek Flash**. Qwen Coding Plan remains configurable but is disabled by default. This supersedes the earlier Qwen-first preference; preserve the existing OpenCode harness and local ASR/memory/identity modules. This routing update is source-only: no tests, builds, inference, or CI were run.

- **2026-11-12 device connection:** The production S31 firmware discovers the paired Mac's
  `_espbot-brain._tcp.local.` service and initiates authenticated WSS over TCP. A single
  running board automatically followed two port changes. See the [real-board acceptance
  record](docs/specs/server-voice-tasks/acceptance.md) and [local ESP-IDF test
  workflow](docs/esp-idf-development.md). BLE provisioning and physical speaker playback
  remain pending.

### 2026-11-06 bring-up

- **First real board bring-up is now in progress at the user's explicit request.** Read [docs/board-bringup-2026-11-06.md](docs/board-bringup-2026-11-06.md) before further firmware work. The connected ESP32-S31 has 16 MB Flash / PSRAM and an actual **SC101IOT** camera, unlike the earlier OV3660 assumption. A full original Flash backup, native module tests, local IDF build and initial flash have been performed; acceptance and remaining failures are recorded there. Do not infer that all peripherals or private Mac speech have passed.
- **Mac server is now source-integrated.** Read [docs/mac-bot-server.md](docs/mac-bot-server.md) and [docs/mac-opencode.md](docs/mac-opencode.md) before changing the server. The user's selected LLM harness is **OpenCode v2 (source target 2.2.5)**; the current route order follows the November 13 selection above. This explicit selection supersedes older local-LLM/HF-S2S defaults for LLM orchestration; ASR, TTS, face/speaker embeddings and personal memory remain local. Reuse installed `browser` / `deepseek-search` MCP definitions.
- The new Mac `brain`/`harness`/`memory` modules, Python local model worker and Chinese companion console are written. The Mac WSS v3 endpoint is separate from diagnostic LAN v1. **Firmware has no WSS v3 client yet**, so no end-to-end private device media is claimed. BLE provisioning remains pending.
- **Later Mac build requested on 2026-11-06:** TypeScript/Vite, Rust release, Cargo.lock resolution and the standalone Mac App now build locally. The real console opened and OpenCode v2.2.5 start/stop with saved Keychain credentials was exercised. The current UI removes the prototype Chat/Provider/Events; API keys are configured in Settings and stored in macOS Keychain. The current console has eight task-based sections; see [docs/mac-console-design.md](docs/mac-console-design.md). LAN v1 discovery/diagnostics are enabled by default because the current firmware still needs them; the device page distinguishes this from secure WSS media. See [docs/mac-console.md](docs/mac-console.md). This console update did not exercise cloud model calls or local model inference; board validation is tracked separately. Do not silently run model calls/inference or CI in a follow-up.
- Hardware was awaiting delivery in the source-only handoff. The later first-board session is recorded above; no motors or IMU have been added or verified.
- The local runtime and official peripheral adapters are now source-integrated. Read [docs/local-runtime.md](docs/local-runtime.md) for actual startup, model/dependency requirements and limitations. This delivery was not compiled, tested or flashed; do not reinterpret source integration as hardware verification.

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
- Name: **小呆**. The ONLY production wake phrase is **小呆小呆**, interpreted as a **single complete four-syllable keyword**; one standalone **小呆** is NOT a wake. The dedicated WakeNet acoustic model is NOT yet provisioned; the current default uses official MultiNet7 with the complete four-syllable command. Name configuration alone cannot make a recognizer work. See [docs/wake-word.md](docs/wake-word.md).
- **Passive companion by default:** only explicit wake or intentional user touch/manual action initiates conversation; face presence, looking at Bot, speech energy, other conversation and acoustic DOA are NOT wake events. Idle → bored → drowsy → sleeping; night dark should keep local audio keyword recognition running while camera and screen are inactive. See [docs/behavior-lifecycle.md](docs/behavior-lifecycle.md).
- **Currently installed V1 hardware:** fixed, non-actuated camera/head that may be moved by hand; **no installed IMU, servo or joint feedback**. **Revised physical target** is an enclosed speaker-equipped two-axis moving head using off-the-shelf parts; do not fabricate capability before real installation. Visual follow means rendered eyes update from CURRENT camera-relative human eye position if a visible target remains in frame. Software cannot turn camera to reacquire an off-screen person. The rendered eyes should not auto-recenter while the head has not physically aligned. Future motors/IMU are optional adapters. See [docs/eye-contact.md](docs/eye-contact.md), [docs/hardware-power.md](docs/hardware-power.md).
- **One authority for each decision:** bot_runtime owns local behavior/lifecycle and command arbitration; bot_bus transports events/commands/snapshots but makes no decisions; attention_manager owns the single official human focus; vision_attention owns multi-person perception, eye landmarks and human look-at-Bot evidence; attention owns eye/head spatial planning; face renders; head_motion owns physical actuator safety when present. No server/LLM per-frame motor or pupil instructions.
- **Audio single owner:** microphone capture is shared by local keyword recognizer and optional DOA, never independently opened twice; unrecognized pre-wake audio is neither persisted nor uploaded. Rust/Tauri bot-server composes the Mac Brain. Device media requires an explicitly activated session through the authenticated v3 client. The LAN v1 link is for diagnostics.
- Keep contracts versioned, capture times monotonic for local control, explicit freshness/validity flags, ephemeral track IDs and distinct human-eye LOCATION versus human-GAZE estimates. No false claim of mutual eye contact when camera is unavailable or too dark.
- When writing code, state what exists, what is only documented, and what requires absent board/model/motor resources. Respect the **no CI** project policy above even when adding new modules.

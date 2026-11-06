# Engineering roadmap and honest implementation inventory — esp-bot

**Scope:** implementation sequencing for passive desktop robot 小呆 on ESP32-S31-Korvo-1 V1.1. This is a plan, NOT a claim that unintegrated modules already work. The user specifically requires **NO CI**; when hardware/model/build environment is absent, deliver modular code and mark unverified behavior instead of adding testing pipelines.

## New reuse-first integration priority (2026-10-30)

**Strategic revision:** [Local-First Hybrid Brain](local-first-hybrid-brain.md) and [upstream reuse catalog](upstream-reuse-catalog.md) take precedence for *how* speech, media, pairing and hardware are implemented. **Mac M4 Pro 48 GB + Rust/Tauri bot-server is the preferred AI execution host**. Reuse XiaoZhi's existing exact S31-Korvo board support, ESP-SR/GMF and HF Speech-to-Speech on MLX before writing equivalents. BLE pairing/Wi-Fi onboarding + degraded control is required. **Mechanical target** is now an attractive off-the-shelf enclosed 2-axis head with speakers; currently no motors are installed. Do not confuse proposed devices with hardware validation.

Before adding custom audio/motor/transport code: identify upstream equivalent, document its license/version and integrate via adapter when possible. Preserve single ownership of mic, Face, attention and runtime. No CI.

## 1. Product constraints locked

1. Passive/quiet by default: occasional blink → bored → drowsy → sleeping; at night attempt fully dark LCD; **no unsolicited greeting**.
2. Device-local full-phrase wake **小呆小呆** only. Bare 小呆 and incidental faces/sounds do NOT wake or initiate conversation.
3. Initial V1 target **fixed, non-actuated** head/camera that can be lifted, rotated or tilted by a human; **no IMU**, no compass or room coordinate map.
4. V1 vision-first rendering: track a chosen person's **eye midpoint**, keep bot's rendered eyes jointly aimed while target remains visible. **Cannot physically turn camera** in V1.
5. Vision tracks multiple faces independently; actual conversation target selection belongs to attention_manager. Actual human look-at-Bot inference is distinct from "eyes are located at x/y".
6. Future head pan/tilt and optional IMU must plug into the same attention controller, rather than owning a second target selector.
7. **Mac M4 Pro / 48 GB Rust/Tauri Bot Server**, with **reused local AI sidecars**, is the prioritized ASR/LLM/TTS/tool provider; secure BLE provisioning/backup comes before cloud dependency. Local wake/gaze must survive offline.
8. Audio before wake remains local and transient; no default uploads. No CI.

See [architecture.md](architecture.md) for canonical module ownership, [module-contracts.md](module-contracts.md) for events, [behavior-lifecycle.md](behavior-lifecycle.md), [wake-word.md](wake-word.md), [eye-contact.md](eye-contact.md) and [audio-direction.md](audio-direction.md).

## 2. Implementation inventory (as of 2026-11-06)

Hardware is awaiting delivery. This local-runtime source delivery was **not compiled, tested, flashed or run**. Detailed startup/model/dependency instructions: [local-runtime.md](local-runtime.md).

| Area | Source now present and integrated | Still missing |
|---|---|---|
| Face/appearance | LVGL renderers/themes, touch wake, stop/settings intent, passive expressions, main runtime rendering | Device tuning and UI hardware validation |
| Geometry attention | Selected focus → current camera-relative stationary gaze; no fake measured pose or mechanical recenter | Real camera/screen geometry and future motor feedback |
| Multi-face vision | Official BSP + esp_video 2.4.1 V4L2 + ESP-DL human_face_detect adapter feeding existing tracker | Physical orientation/latency checks, calibrated human gaze/iris and family identity |
| On-device wake/audio | Single codec owner, official ESP-SR AFE/WakeNet/VAD, PCM queue/playback/cancel, runtime token gates | **Dedicated 小呆小呆 model unprovisioned**; true AEC, voice identity, hardware validation |
| DOA | Official Espressif example catalogued | Dual-mic synchronized direction adapter and calibration |
| Local coordinator/bus | bot_runtime, bot_bus, attention_manager, epoch fences, overflow/stop handling and resource requests | Future additional semantic/media actions |
| Display/night | Logical black overlay preserving touch, camera STREAMOFF/inference pause, audio kept running | BSP backlight control unsupported; actual full blackout/rail power-off not provided |
| Motion/IMU | Stationary head_motion capability adapter, unsupported motion requests, invalid pose feedback | Motors, IMU and real actuator driver |
| Connectivity | Main brain_link uses existing Wi-Fi/mDNS/WS; AUTH_REQUIRED receipts; runtime has typed semantic arbitration | Cryptographic pairing/encrypted transport, BLE provisioning, private media, Mac speech/harness integration |
| Legacy transport | Python server and HTTP client source retained | HTTP client is already excluded from firmware main; legacy server remains independent |

Existing Rust/Tauri server text providers, command console and soft pairing remain unchanged by this firmware delivery. No live hardware wake, camera gaze, DOA, physical blackout or mechanical tracking is claimed verified.

## 3. Delivery sequence, with clear gates

### P0 — central ownership and contracts (source delivered)
- Write **bot_bus** (events, commands, latest snapshots, bounded queue rules).
- Write **bot_runtime** (one behavior authority, passive lifecycle, wake event, session/turn generations, preferences and capability/health flags).
- Write **attention_manager** (one official current focus; candidate/confirmed/temporarily-lost/reacquisition).
- Write **head_motion** stationary adapter reporting no motors rather than fabricated joint feedback.
- app_main is the composition root. Existing Face remains; legacy HTTP polling is excluded from main to preserve single authority.
- Confirm interface discipline: never allow server or renderer to steal live gaze.

### P1 — local name-based wake (adapter delivered, acoustic model pending)
- Introduce unified **audio** input owner, two-channel capture and local on-device WakeNet adapter.
- Provision and package real recognition resources for the **entire phrase 小呆小呆**; bare 小呆 stays negative.
- Emit one WAKE_DETECTED event with capture time; no pre-wake voice upload; wake remains alive with LCD/camera stopped and network offline.
- Implement waking Face animation and interactive session start; no promise of physical head lift.
- If model is missing: report WAKE_MODEL_MISSING, never a fake wake success. Development stock models are bring-up aids ONLY, not production aliases.

### P2 — camera-driven rendered-eye gaze (adapter/selection/gaze source delivered)
- Use official S31 BSP + esp_video frame acquisition with single capture owner and timestamp/rotation normalization.
- Add ESP-DL MSR/MNP multi-face/5-landmark adapter, use high-resolution eye ROIs for separate pupil/gaze stage; preserve UNKNOWN when evidence insufficient.
- Connect perception → focus manager → calibrated camera-relative eye target → stationary attention gaze planner → Face LVGL.
- Handle 0,1,2+ faces, target switch hysteresis and short visual loss. Distinguish human eye location, human looking-at-Bot and Bot's ability to aim eyes.
- When robot is rotated manually: remeasure and adjust rendered eyes **if target still in FOV**, otherwise mark localization lost without pretending the camera rotated back.

### P3 — conversation and lifecycle integration (local policy delivered; secure Mac media pending)
- Connect audio session (after wake) to Rust/Tauri Brain gateway with explicit session/turn IDs and command arbitration.
- Implement listening/thinking/speaking expressions without overriding gaze, stop/interrupt handling, caption lifetime.
- Implement configurable IDLE/BORED/DROWSY/SLEEPING/NIGHT_DARK and camera/LCD sleep request, wake/resume.
- Verify later on real hardware whether LCD panel sleep physically produces a fully dark screen; never assume from API alone.

### P4 — optional DOA and richer conversation
- Integrate esp_apa_doa-style dual-mic angle + quality with same audio owner. Sound direction is a search hint, not a person ID and not an extra wake method.
- Optionally correlate speaking face with sound when reliable; preserve ambiguity for multiple speakers and linear-array front/back uncertainty.
- Add better per-person calibrated eye gaze and interaction UX after real-world dataset evidence.

### P5 — targeted off-the-shelf actuated pan/tilt and optional IMU (not yet installed)
- Replace stationary adapter with actual feedback/limit-aware servo driver.
- Extend attention from relative camera error to full coordinated eye–head plans: eyes lead, head follows, eyes recenter **only when physical head aligns**.
- Compensate head's own camera motion using measured joint pose. If IMU is added, enhance short-term disturbance rejection, but visual feedback remains corrective and authoritative.
- Large base tilt/handheld motion is bounded by 2-axis mechanical reach; safe held-by-hand mode before any autonomous corrective motor movement.

## 4. Engineering invariants for every iteration

- Priority: **wake and local behavior responsiveness** independent of network. No model-generated per-frame servo or pupil commands.
- One owner per hardware device, one official focus, one gaze controller, one runtime behavior authority.
- Never equate gaze inference UNKNOWN with NOT_LOOKING; never infer person identity from ephemeral track ID.
- Event timestamp and actual capture age must be preserved across queues. New measurements replace stale camera positions.
- Human gaze at Bot cannot, by itself, trigger wake.
- A valid full-name wake is required before any background ambient audio is routed to Brain.
- Fixed camera cannot be re-aimed. Pure visual compensation only functions while an appropriate visual target is observed.
- No CI workflows. If no execution/test conditions, write code and clearly state "not built / not run / not hardware verified".

## 5. Handoff criteria (specification, not CI gates)

For each future implementation, provide:
1. Source paths, module public API and ownership boundaries.
2. Required firmware dependencies/models, asset/provisioning details and optional flags.
3. What is actually implemented vs hardware-dependent or conceptual.
4. Relevant integration call sites (or explicit notice when main integration has **not** been performed).
5. Hardware limitations and observable failure states rather than fake successes.

A hardware demonstration may be done manually when the user chooses; no request to create CI or automatically perform an unavailable board test.

# Engineering roadmap and honest implementation inventory — esp-bot

**Scope:** implementation sequencing for passive desktop robot 小呆 on ESP32-S31-Korvo-1 V1.1. This is a plan, NOT a claim that unintegrated modules already work. The user specifically requires **NO CI**; when hardware/model/build environment is absent, deliver modular code and mark unverified behavior instead of adding testing pipelines.

## 1. Product constraints locked

1. Passive/quiet by default: occasional blink → bored → drowsy → sleeping; at night attempt fully dark LCD; **no unsolicited greeting**.
2. Device-local full-phrase wake **小呆小呆** only. Bare 小呆 and incidental faces/sounds do NOT wake or initiate conversation.
3. V1 **fixed, non-actuated** head/camera that can be lifted, rotated or tilted by a human; **no IMU**, no compass or room coordinate map.
4. V1 vision-first rendering: track a chosen person's **eye midpoint**, keep bot's rendered eyes jointly aimed while target remains visible. **Cannot physically turn camera** in V1.
5. Vision tracks multiple faces independently; actual conversation target selection belongs to attention_manager. Actual human look-at-Bot inference is distinct from "eyes are located at x/y".
6. Future head pan/tilt and optional IMU must plug into the same attention controller, rather than owning a second target selector.
7. Mac/Linux Rust/Tauri Bot Server supplies later ASR/LLM/TTS/tools. Local wake/gaze must survive offline.
8. Audio before wake remains local and transient; no default uploads. No CI.

See [architecture.md](architecture.md) for canonical module ownership, [module-contracts.md](module-contracts.md) for events, [behavior-lifecycle.md](behavior-lifecycle.md), [wake-word.md](wake-word.md), [eye-contact.md](eye-contact.md) and [audio-direction.md](audio-direction.md).

## 2. Implementation inventory (as of 2026-10-30)

| Area | Existing repository code | Still missing |
|---|---|---|
| Face/appearance | ESP-IDF/LVGL Face component, replaceable eye renderers, themes, touch appearance menu, browser preview | Runtime policy integration and actual wake/sleep behavior; device-level validation |
| Geometry attention | Standalone C attention planner, shared pupil direction, theoretical pan/tilt output, freshness/dwell fields | Stationary V1 mode, selected-target adapter, true sensor coordinate calibration, real motor feedback later |
| Multi-face vision | vision_attention C temporal multi-track snapshots, per-person fixation clocks, provisional dark-pupil candidate locator | S31 esp_video capture, actual ESP-DL model adapter, robust calibrated eye-gaze inference, camera ROI integration |
| On-device wake | shared/bot-identity.json, wake-word design | **Dedicated 小呆小呆 model unprovisioned**, shared mic/AFE/WakeNet capture, runtime event integration |
| DOA | Public Espressif S31 example documented | Board audio-direction module, reliable synchronized stream adapter, calibration and focus fusion |
| Local coordinator/bus | Architecture and docs | bot_runtime, bot_bus, attention_manager, resource/power lifecycle |
| Motion/IMU | Pure geometric planner + design | Real motor hardware absent; stationary adapter first; IMU optional future |
| Connectivity | firmware/components/lan_link, Rust/Tauri server/bot-server discovery and WebSocket skeleton | Main integration, pairing/authenticated channel, audio/video media routing, ASR/LLM/TTS services |
| Legacy server/transport | Python server and ESP HTTP polling client exist | Retirement/reconciliation after Rust/Tauri Brain reaches feature parity |

No live wake-on-name, camera gaze, DOA localization, sleeping physical blackout or true mechanical tracking is claimed implemented in hardware.

## 3. Delivery sequence, with clear gates

### P0 — central ownership and contracts
- Write **bot_bus** (events, commands, latest snapshots, bounded queue rules).
- Write **bot_runtime** (one behavior authority, passive lifecycle, wake event, session/turn generations, preferences and capability/health flags).
- Write **attention_manager** (one official current focus; candidate/confirmed/temporarily-lost/reacquisition).
- Write **head_motion** stationary adapter reporting no motors rather than fabricated joint feedback.
- app_main becomes thin composition root. Keep existing Face and legacy transport functioning until cutover.
- Confirm interface discipline: never allow server or renderer to steal live gaze.

### P1 — hard requirement: local name-based wake
- Introduce unified **audio** input owner, two-channel capture and local on-device WakeNet adapter.
- Provision and package real recognition resources for the **entire phrase 小呆小呆**; bare 小呆 stays negative.
- Emit one WAKE_DETECTED event with capture time; no pre-wake voice upload; wake remains alive with LCD/camera stopped and network offline.
- Implement waking Face animation and interactive session start; no promise of physical head lift.
- If model is missing: report WAKE_MODEL_MISSING, never a fake wake success. Development stock models are bring-up aids ONLY, not production aliases.

### P2 — true camera-driven rendered-eye gaze (fixed base)
- Use official S31 BSP + esp_video frame acquisition with single capture owner and timestamp/rotation normalization.
- Add ESP-DL MSR/MNP multi-face/5-landmark adapter, use high-resolution eye ROIs for separate pupil/gaze stage; preserve UNKNOWN when evidence insufficient.
- Connect perception → focus manager → calibrated camera-relative eye target → stationary attention gaze planner → Face LVGL.
- Handle 0,1,2+ faces, target switch hysteresis and short visual loss. Distinguish human eye location, human looking-at-Bot and Bot's ability to aim eyes.
- When robot is rotated manually: remeasure and adjust rendered eyes **if target still in FOV**, otherwise mark localization lost without pretending the camera rotated back.

### P3 — conversation and lifecycle integration
- Connect audio session (after wake) to Rust/Tauri Brain gateway with explicit session/turn IDs and command arbitration.
- Implement listening/thinking/speaking expressions without overriding gaze, stop/interrupt handling, caption lifetime.
- Implement configurable IDLE/BORED/DROWSY/SLEEPING/NIGHT_DARK and camera/LCD sleep request, wake/resume.
- Verify later on real hardware whether LCD panel sleep physically produces a fully dark screen; never assume from API alone.

### P4 — optional DOA and richer conversation
- Integrate esp_apa_doa-style dual-mic angle + quality with same audio owner. Sound direction is a search hint, not a person ID and not an extra wake method.
- Optionally correlate speaking face with sound when reliable; preserve ambiguity for multiple speakers and linear-array front/back uncertainty.
- Add better per-person calibrated eye gaze and interaction UX after real-world dataset evidence.

### P5 — future actuated pan/tilt and optional IMU
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

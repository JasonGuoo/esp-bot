# Module contracts and local event bus — esp-bot V1

**Status:** design contract, NOT implemented as a running event bus or main integration. **Authority:** [architecture.md](architecture.md) for ownership, [behavior-lifecycle.md](behavior-lifecycle.md) for policy, [eye-contact.md](eye-contact.md) for gaze geometry. The names below denote logical modules, not a promise of one RTOS task per component.

## 1. Single owners

| Concern | Sole owner | Responsibilities | Forbidden cross-layer shortcut |
|---|---|---|---|
| App lifetime, behavior mode, permissions | **bot_runtime** | Start/stop modules, passive policy, conversation stage, screen-off requests, command arbitration | Vision, server, or Face changing behavior mode by themselves |
| Module event transport | **bot_bus** | Bounded event queues, command dispatch, latest-state snapshots and subscriptions | Choosing a person or deciding to wake |
| Camera frames, timestamps, BSP, storage | **bot_platform** | Single resource ownership, frame allocation, NVS, LVGL lock, audio codec initialization, uptime clock | Each component independently reinitializing camera / codec / I2C |
| Multiple faces and eye evidence | **vision_attention** | Per-person transient IDs, face/eye/iris observations, valid-looking classification, durations | Choosing the official active interaction target; moving head |
| Audio input / output | **audio** | Shared mic capture, local keyword, voice activity, optional DOA, TTS playback | Remote server as the only wake detector |
| Current human target | **attention_manager** | Focus, hysteresis, target preservation, selection under runtime policy | Calling face_set_gaze or motor GPIO |
| Shared gaze + optional motor plan | **attention** | Target projection, tracking, filtering, gaze residual, physical reachability | Face detection, target arbitration, or direct servo I/O |
| Expression, eyes, settings UI | **face** | Render gaze and expression; display subtitle; emit explicit touch intent | Deciding conversation state, overriding gaze with idle animation |
| Actual head hardware | **head_motion** | Current: stationary adapter. Future: actuation, limits and joint feedback | Inventing measured pose or selecting people |
| Remote connection | **brain_link** | Session/link lifecycle, authenticated commands, media routing | Direct live pupil/servo control from the server |

Transport note: existing **lan_link** is an implementation below brain_link, NOT a second app controller. Existing **brain_client** and Python Brain are legacy prototypes; Rust/Tauri server is the selected future architecture. They are not all integrated.

**Source authority:** Face theme/appearance behavior remains in [face-design.md](face-design.md) and shared appearance packs. Person tracking remains in [visual-attention.md](visual-attention.md), voice name in [wake-word.md](wake-word.md).

## 2. Three distinct transport semantics

**Events** mean something happened: bounded, ordered by producer, backpressure and overflow metrics. Critical wake, explicit touch, stop, model-failed and safety events must not be silently dropped. Examples: WAKE_DETECTED, FACE_ENTER, LOOK_BEGIN, LOOK_END, FOCUS_CHANGED, UTTERANCE_STARTED, PLAYBACK_DONE, CAMERA_UNAVAILABLE, LINK_LOST.

**Commands** express an authorized intention: runtime issues SET_BEHAVIOR, REQUEST_SCREEN_SLEEP, START_VISUAL_ACQUISITION, SET_EXPRESSION, START_POST_WAKE_AUDIO_SESSION, STOP_PLAYBACK. Consumers acknowledge accepted/rejected with reason or updated status; stale commands are rejected, not executed late.

**Latest snapshots** expose rapidly changing measurements: PerceptionSnapshot, FocusSnapshot, GazePlan, HeadState, AudioHealth. Keep ONE latest immutable value per topic (plus sequence) or a bounded double buffer; replace older positions instead of queueing every frame. Consumers must check capture age and validity.

**Media streams** are separate producer-owned camera/audio buffers with explicit refcount/borrow lifetime. Never copy camera frames or persistent pre-wake audio into the ordinary event bus. Camera can drop old frames if inference lags; real-time audio cannot use the same arbitrary frame-drop policy.

## 3. Suggested typed envelope (proposed, not an existing wire protocol)

~~~json
{
  "schema_version": 1,
  "source": "vision_attention",
  "topic": "perception.snapshot",
  "sequence": 418,
  "observed_monotonic_ms": 742910,
  "published_monotonic_ms": 742933,
  "payload": {
    "camera_ok": true,
    "tracks": []
  }
}
~~~

Invariants:
- Observation time means actual sensor capture time. Publish time and arrival time are different. NEVER refresh a stale observation timestamp during prediction.
- Use ESP-side monotonic clocks for internal control. Wall clock is ONLY for schedules and logs. Server time must be translated into local age, not directly compared to ESP uptime.
- Always carry valid/unknown fields and confidence; UNKNOWN does not mean false.
- A track ID is an ephemeral tracking handle, NOT identity recognition or a persistent person profile.
- A new focus assignment increments focus_generation; a late command/report with an old generation or conversation turn must not seize the new target.
- Estimated human **eye location** and estimated human **gaze direction** are different fields and never interchangeable.

## 4. Minimal snapshots

### PerceptionSnapshot (vision_attention)
- capture timestamp, camera validity, canonical frame orientation and dimensions, intrinsics/calibration ID.
- Up to implementation-defined N faces, each with track_id, bbox, two eye centers, eye midpoint, candidate pupils if valid, direction to face, optional head pose and human look-at-Bot score with independent validity.
- Independent continuous and accumulated gaze duration and last seen timestamps.
- Global LOOKING_AT_BOT simply means at least one confirmed case, **not** that runtime may initiate conversation.

### FocusSnapshot (attention_manager)
- focus_generation, active_track_id or NONE, source/reason, focus state: NONE, ACQUIRING, TRACKING, TEMPORARILY_LOST, REACQUIRING.
- Latest observation timestamp, localization_valid, last bearing, optionally prior audio candidate.
- ACTIVE SESSION is separate: a person may look away or leave view yet remain the conversation partner.

### GazePlan (attention)
- focus_generation, observed timestamp, eye_x/eye_y common for both rendered eyes, bearing confidence, motor target when supported, overall gaze validity, clamped/reachable status.
- In stationary mode only rendered-eye output is actionable. No invented motor pose, no automatic recenter while active target is still off-axis.

### RuntimeSnapshot (bot_runtime)
- behavior: IDLE, BORED, DROWSY, SLEEPING, NIGHT_DARK, WAKING, ENGAGED.
- conversation stage (LISTENING, THINKING, SPEAKING, etc.) separate from passive level.
- wake engine availability, display state, audio state, vision requested/available, network state.
- active command owner / priority, active session ID and turn ID.

## 5. Authority and arbitration

- Seeing a face, prolonged gaze at Bot, audio energy or room chatter MUST NOT automatically change from passive mode to ENGAGED.
- Valid full phrase 小呆小呆 (exact local wake engine) or explicit touch/manual user interaction may initiate ENGAGED. Current wake model is not installed; do not synthesize fake WAKE_DETECTED events.
- **bot_runtime** owns behavior modes and permissions. **attention_manager** owns actual target under those permissions. **attention** owns target-to-gaze math. **face** draws results. **head_motion** owns actuator safety even if runtime asks otherwise.
- Runtime may refuse an expression suggestion from server, or allow expression but not gaze override. Remote legacy face.set gaze is for development/manual renderer preview and must not race with live tracking.
- Settings mode owns UI focus but doesn't silently change wake phrase or bypass hardware capabilities.
- When camera fails, mark visual fields invalid and retain the relationship only for a bounded policy interval; report "still engaged" separately from "still making eye contact".

## 6. Scheduling and responsiveness

Suggested initial budgets, to be profiled on the real ESP32-S31 with LVGL and audio running:
- Wake audio: continuously sampled at codec/model-required rate with no dependence on camera or server.
- Face detector: ~5–10 Hz active, eye ROIs more frequently when resources allow.
- Perception snapshots: capture timestamps, overwrite old results when new ones arrive.
- Gaze/controller and Face motion: ~20–50 Hz local lightweight work, not inside inference or network callbacks.
- head_motion future: joint controller follows hardware capabilities; actual feedback, not commanded pose.
- Audio/media network, camera inference and LVGL owner each get isolated execution context; no heavyweight work in event dispatch.
- Monitor worst-case age of face snapshots, dropped camera frames, wake misses, event overflow, stale commands and head feedback health when integrations are implemented.

Implementation choice: a thin FreeRTOS event + latest snapshot abstraction (or ESP-IDF esp_event with additional snapshot storage) is suitable. Do not introduce a separate distributed broker inside one device.

## 7. Local versus server contract

Device must continue wake listening, passive behavior and any local gaze tracking when Mac/Linux is offline. Rust/Tauri bot-server will eventually provide ASR/LLM/TTS/tools through brain_link, but must not own a low-latency eye or motor loop. LAN discovery/transport is described in [lan-discovery.md](lan-discovery.md); discovery is not authentication or device pairing.

## 8. Delivery rule

Write modular code directly; **no CI workflows or CI merge gate** (see AGENTS.md). State clearly when firmware, real camera gaze, microphone DOA, sleep/blackout and other features are only interfaces and have not been run on a board.

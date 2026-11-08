# Lively companion behavior, daytime naps and night mode — 小呆

**Updated user direction, 2026-11-07:** lively during the day, looking around on its own, occasionally drowsy and then awake again; rest at night. It never starts a conversation merely because a face, gaze or sound is detected. Full **小呆小呆** and deliberate touch/manual input remain the interaction triggers.

**Authority:** bot_runtime/behavior_policy owns transitions. audio owns continuous keyword recognition; face owns animation; bot_platform owns display sleep and camera power; attention_manager may choose a target only under runtime policy. Full wake phrase requirements: [wake-word.md](wake-word.md).

## 1. Orthogonal states — avoid a giant Cartesian state machine

**Passive/engagement mode (one current mode):**
- IDLE: awake eyes, blinking and occasional self-directed screen-eye glances. Daytime local face detection can request a silent close-face smile; it does not start speech or a conversation.
- BORED: legacy expression/state identifier; no automatic daytime transition to it.
- DROWSY: a short daytime doze; automatically returns to IDLE without opening a conversation.
- SLEEPING: retained state identifier; ordinary daytime idle does not enter indefinite sleep.
- NIGHT_DARK: display sleep requested, camera stream/inference paused, local keyword recognition remains active.
- WAKING: local wake accepted; restore display, eyes open naturally; camera may spin up if visual conditions allow.
- ENGAGED: active interaction after explicit wake/touch. Human focus, listening, thinking and speaking are handled as additional conversation/focus substates.

**Conversation stage (orthogonal):** NONE, LISTENING, THINKING, SPEAKING, WAITING, ENDING. It must not be conflated with the human target location or presence.

**Focus state (owned by attention_manager):** NONE, ACQUIRING, TRACKING, TEMPORARILY_LOST, REACQUIRING. Even when A looks away or temporarily disappears, a conversation can continue with A.

**Device capability/health state (orthogonal):** camera available/unavailable, wake model ready/missing, audio playback busy, local time valid/invalid, display sleep supported/not verified, server online/offline.

## 2. Wake permissions and invariants

| Trigger | From passive mode | ENGAGED | Notes |
|---|---|---|---|
| One standalone 小呆 | ignore | continue | Name alone is NOT wake phrase |
| Complete 小呆小呆 recognized on-device | wake | may mark fresh turn per policy | Single four-syllable acoustic keyword; do not combine independent 小呆 events |
| Deliberate screen touch / local user action | may open Settings or manually wake | UI action | Distinguish accidental touch from explicit intent; optional screen wake policy |
| Face appears / person remains in frame | NO wake | updates visual target only if requested | Vision is not a conversation trigger |
| Person stares at Bot for a long time | NO wake | may inform engagement | Eye contact is not consent to start chatting |
| Speech energy / random voices / motion / DOA | NO wake | optional VAD/speaker data | DOA is direction estimate, not wake recognition |
| Incoming server suggestion/tool action | NO implicit wake | may request UI/response under runtime checks | Network cannot override sleeping policy |

- Local wake must continue in SLEEPING and NIGHT_DARK **without Mac, Wi-Fi, camera or active LCD**; it is an application-level always-on audio mode, **not ESP deep sleep**.
- Pre-wake audio remains a short transient buffer processed locally; do not upload, persist or use as surveillance audio. After accepted wake and applicable consent, start session audio transfer.
- Do not claim wake-ready until the dedicated full-phrase model is loaded. Current identity manifest explicitly says recognition_model=unprovisioned.
- Bot's own TTS must not retrigger the keyword; prefer acoustic echo suppression and event cooldown, subject to measured hardware behavior.
- If wake model fails, report degraded state visibly when display is awake / in console; do not falsely claim sleeping device can be awakened by voice.
- Waking does **not** require camera or ASR to find a face; in darkness it may listen and answer without human gaze tracking.

## 3. Passive timeline — current starting defaults

The cycle restarts after an explicit interaction/session or at the end of night. Its time source is monotonic, never the last face observation.

| Idle elapsed | Policy | Typical visual behavior |
|---|---|---|
| First 20 min | IDLE | Open eyes blink and glance in different directions every 3.5–7 s |
| Next 1 min | DROWSY | Brief subdued eyes; display remains requested awake |
| Afterwards | IDLE again | Automatically begin another awake interval |
| Valid night policy while not engaged | NIGHT_DARK immediately | Display sleep, camera paused; keyword engine still available |

The two intervals are configurable. Glance targets and intervals vary locally; the fixed V1 device moves only the drawn eyes. No face observation, unsolicited speech or physical head motion is implied.

### User interaction end

- During an active conversation, ordinary speech continues without repeated name.
- When speech/playback pauses, retain the conversation for a configurable multi-second context grace; do not exit on every 0.5 s pause.
- After session timeout or clear end, stop session audio, release target under attention_manager policy and return to IDLE (or NIGHT_DARK if currently night).
- Visual loss is **not** immediate session termination. It invalidates target position while the session can remain active.

## 4. Night and darkness policy

Primary night decision: user-configured local time window (illustrative 22:30–07:00). Camera image quality is a **secondary visual-usability gate**, not a reliable clock or independent authority to wake. Light sensitivity, exposure/gain and noise can indicate that vision cannot identify eyes; false night classification should not suppress wake audio.

- NIGHT_DARK requests camera capture and vision inference off, LCD panel sleep and no visible idle animation. Wake audio capture continues.
- On full wake phrase, leave NIGHT_DARK, show waking eyes if hardware display allows, attempt camera acquisition only when light/quality is adequate; if camera remains unusable, enter ENGAGED_AUDIO_ONLY.
- After the interaction, return to NIGHT_DARK if the night window still applies.
- When LCD/camera are asleep, **they cannot continuously measure ambient light**. A scheduled sample/wake, explicit wake, or later external light sensor may reassess darkness. Don't promise autonomous daylight detection from a powered-off camera.
- The initial configured window is 22:30–07:00 in POSIX `CST-8` (China time). ESP-IDF SNTP starts after Wi-Fi obtains an address and may contact the configurable `pool.ntp.org` server. This wall clock is used only for the night schedule, not authorization or session deadlines.
- If wall clock has never synchronized, do not guess that it is night: continue the daytime awake/doze cycle and report `clock_valid=0`. After power loss without network there is no battery-backed clock guarantee.
- This board's LCD sleep API availability does **not** guarantee complete physical backlight blackout until checked on hardware. Product-grade full darkness may require additional backlight power gating.

## 5. Device resource policy

| Mode | Audio capture + wake recognizer | Camera + face/eye inference | LCD/Face | Link/server |
|---|---|---|---|---|
| IDLE / brief DROWSY | ON if model ready | ON locally for close-face reaction | Animated eyes / short doze; close face pauses glances and smiles with blinks | Background connection optional |
| NIGHT_DARK | ON | OFF | Sleep requested (full-black conditional on hardware) | Optional |
| WAKING | ON; accepted wake debounced | START if allowed | Eye-opening animation | Connect/reuse if needed |
| ENGAGED | ON for speech and interrupt policy | ON when usable | Tracking, subtitles, expressions | Conversation offloaded when available |

Wake detection and DOA may share the same microphone owner but do not both need heavy inference continuously. Night mode is NOT a forced full-chip low-power sleep and its power should be measured later.

## 6. Waking sequence (animation intent, not hardcoded timing)

1. audio emits one validated WAKE_DETECTED with model identity, monotonic capture time, confidence and event ID.
2. bot_runtime accepts exactly one wake, changes state to WAKING, requests display resume.
3. face shows gentle eye opening and a brief sleepy expression; no mechanical "head raise" is possible in the current fixed base.
4. Runtime requests post-wake audio session and optional camera activation. If DOA was estimated from the call, it is a *candidate acoustic direction* only.
5. attention_manager chooses a visible speaker/face when evidence allows, retaining multi-person tracks independently. attention calculates a single gaze for both eyes.
6. bot_runtime enters ENGAGED/LISTENING. Voice conversation runs through brain_link when available; local gaze and wake logic never require server availability.
7. When finished, return to passive path without spontaneous follow-up chatter.

## 7. Error and privacy behavior

- Camera fails -> no claim of eye contact; voice wake still works if local model is ready.
- Server offline -> local wake/face still works; show nonverbal unavailable cue only if needed; don't invent an LLM answer.
- Wake model missing -> flag WAKE_MODEL_MISSING and missing capability; never pretend the display name alone enables wake.
- Panel cannot actually turn its backlight fully off -> report partial screen sleep and document hardware restriction.
- Don't persist video frames, ephemeral person tracking IDs or pre-wake ambient audio by default. No cloud upload before wake.

## 8. Current implementation status

The lively daytime policy and network clock have been built and flashed; on-board idle glances and time synchronization have been observed. The full real-time 20-minute/one-minute cycle and an actual overnight transition remain unverified; simulated boundary tests passed. See the latest board bring-up record for evidence. The dedicated 小呆小呆 model is still missing. BSP 1.1.0 does not support backlight brightness control; night uses a black overlay with active touch, and camera pause means STREAMOFF/no inference rather than rail power-off.

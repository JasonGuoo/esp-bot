# Lively companion behavior, daytime naps and night mode — 小呆

**2026-11-15 real-room check: NOT PASSED.** After the user announced lights-off, the 21:18–21:28 observation still reported IDLE and non-black frames. Valid camera luma was 38–107 (median 78), above the provisional entry threshold. The 22:30 scheduled transition was outside this observation. See [the measured result and serial-reset limitation](night-light-check-2026-11-15.md); source policy and simulated tests are not real-room acceptance.

**Owner clarification, 2026-11-19:** Active companionship takes place after a complete
wake phrase or deliberate touch. Idle face presence, nearby speech and time of day do not start a
spoken greeting. The Mac no longer offers unprompted lines and the firmware rejects older
`proactive.offer` commands. Previously saved reminders and journals remain available, but automatic
idle playback is disabled. The user can continue a woken conversation with questions, stories,
expressions and follow-ups.

The 2026-11-18 unprompted-greeting design in the historical UX review is superseded by this rule.

**Updated user direction, 2026-11-08:** lively when it is light, occasionally drowsy and then awake again; rest at night or after the room stays dark. It never starts a conversation merely because a face, gaze, sound or brighter room is detected. Full **小呆小呆** and deliberate touch/manual input remain the interaction triggers.

**Authority:** bot_runtime/behavior_policy owns transitions. audio owns continuous keyword recognition; face owns animation; bot_platform owns display sleep and camera power; attention_manager may choose a target only under runtime policy. Full wake phrase requirements: [wake-word.md](wake-word.md).

## 1. Orthogonal states — avoid a giant Cartesian state machine

**Passive/engagement mode (one current mode):**
- IDLE: awake eyes, blinking and occasional self-directed screen-eye glances. Daytime local face detection can request a silent close-face smile; it does not start speech or a conversation.
- BORED: legacy expression/state identifier; no automatic daytime transition to it.
- DROWSY: a short daytime doze; automatically returns to IDLE without opening a conversation.
- SLEEPING: room-darkness policy requests a black screen and pauses the camera between brief brightness-only probes; ordinary daytime idle alone does not enter indefinite sleep.
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
| Room becomes brighter | resume passive idle after confirmed brightness | no change | Does not start a conversation; scheduled night still takes priority |
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
| Sustained dark while not engaged | SLEEPING | Black screen, periodically probe camera brightness; local audio continues |
| Valid night policy while not engaged | NIGHT_DARK immediately | Display sleep, camera paused; keyword engine still available |

The two intervals are configurable. Glance targets and intervals vary locally; the fixed V1 device moves only the drawn eyes. No face observation, unsolicited speech or physical head motion is implied.

### User interaction end

- During an active conversation, ordinary speech continues without repeated name.
- When speech/playback pauses, retain the conversation for a configurable multi-second context grace; do not exit on every 0.5 s pause.
- After session timeout or clear end, stop session audio, release target under attention_manager policy and return to IDLE (or NIGHT_DARK if currently night).
- Visual loss is **not** immediate session termination. It invalidates target position while the session can remain active.

## 4. Night and darkness policy

The user-configured night window and sustained room darkness are independent reasons to rest. The installed board has no documented ambient light sensor; it reuses the existing camera's mean pixel luma as an **uncalibrated estimate**, not lux. Auto-exposure, camera direction and lens occlusion can change the result. Current provisional thresholds are 25/255 for 30 seconds to enter room-dark sleep and 45/255 for 3 seconds to return to passive idle; use only fresh valid captures and retain the previous state when capture fails. Thresholds need real-room calibration.

- NIGHT_DARK requests camera capture and vision inference off, LCD panel sleep and no visible idle animation. Wake audio capture continues.
- On full wake phrase, leave NIGHT_DARK, show waking eyes if hardware display allows, attempt camera acquisition only when light/quality is adequate; if camera remains unusable, enter ENGAGED_AUDIO_ONLY.
- After the interaction, return to NIGHT_DARK if the night window still applies.
- Room-dark SLEEPING pauses the camera stream and every 30 seconds opens it for at most 10 seconds solely to sample brightness; there is no face inference or debug camera frame during a probe. A confirmed bright probe resumes passive idle. If scheduled NIGHT_DARK also applies, the clock policy takes precedence and the camera remains paused without these probes. Neither mode physically removes the camera's power rail.
- The initial configured window is 22:30–07:00 in POSIX `CST-8` (China time). ESP-IDF SNTP starts after Wi-Fi obtains an address and may contact the configurable `pool.ntp.org` server. This wall clock is used only for the night schedule, not authorization or session deadlines.
- If wall clock has never synchronized, do not guess that it is scheduled night: report `clock_valid=0`; the independent darkness policy can still apply. After power loss without network there is no battery-backed clock guarantee.
- This board's LCD sleep API availability does **not** guarantee complete physical backlight blackout until checked on hardware. Product-grade full darkness may require additional backlight power gating.

## 5. Device resource policy

| Mode | Audio capture + wake recognizer | Camera + face/eye inference | LCD/Face | Link/server |
|---|---|---|---|---|
| IDLE / brief DROWSY | ON if model ready | ON locally for close-face reaction | Animated eyes / short doze; close face pauses glances and smiles with blinks | Background connection optional |
| NIGHT_DARK | ON | OFF | Sleep requested (full-black conditional on hardware) | Optional |
| SLEEPING (room dark) | ON | OFF between short brightness-only probes | Black overlay (backlight unsupported on this board) | Optional |
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

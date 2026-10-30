# Passive companion behavior, sleep and night mode — 小呆

**V1 product personality:** quiet, passive, slightly drowsy desktop companion. It **never** starts a conversation because a face is seen, a person stares at it, or a random sound is heard. It reacts to its full **小呆小呆** local wake phrase and deliberate touch/manual input; once engaged it can listen/respond without requiring the name every utterance.

**Authority:** bot_runtime/behavior_policy owns transitions. audio owns continuous keyword recognition; face owns animation; bot_platform owns display sleep and camera power; attention_manager may choose a target only under runtime policy. Full wake phrase requirements: [wake-word.md](wake-word.md).

## 1. Orthogonal states — avoid a giant Cartesian state machine

**Passive/engagement mode (one current mode):**
- IDLE: awake eyes, quiet rare blink, slight low-amplitude idle movement; no speech.
- BORED: fewer movements, heavy/slow occasional blink, subdued eyes; no speech.
- DROWSY: eyelids droop, eyes periodically close; no speech.
- SLEEPING: static closed eyes or panel sleep depending configured display policy; wake engine still runs.
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

## 3. Passive timeline — suggested starting defaults, not committed performance

The elapsed clock is **time since last valid explicit interaction or completion of a session**, never since the last face was seen.

| Idle elapsed | Policy | Typical visual behavior |
|---|---|---|
| 0–60 s | IDLE | Quiet open eyes, occasional natural blink |
| 60–180 s | BORED | Less eye movement, slow half-blinks, slightly distracted |
| 180–300 s | DROWSY | Slow eyelid droop and short doze |
| >300 s | SLEEPING | Eyes closed; optionally sleep panel |
| Valid night policy while not engaged | NIGHT_DARK immediately | Display sleep, camera paused; keyword engine still available |

These thresholds should be configurable in behavior_policy and receive small bounded timing jitter in animations (not in wake logic). No unsolicited greeting, spoken yawn, or attention-grabbing movements. Visual theme pack controls how boredom/sleep is drawn but not who can wake the Bot.

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
- If wall clock has never synchronized, use known time if available; otherwise choose conservative passive/quiet policy and do not assume a valid sunset. Clock/timezone validity and schedule are separate fields.
- This board's LCD sleep API availability does **not** guarantee complete physical backlight blackout until checked on hardware. Product-grade full darkness may require additional backlight power gating.

## 5. Device resource policy

| Mode | Audio capture + wake recognizer | Camera + face/eye inference | LCD/Face | Link/server |
|---|---|---|---|---|
| IDLE / BORED / DROWSY | ON if model ready | OFF by default (optional low-rate diagnostics only, never a wake trigger) | Animated eyes | Background connection optional |
| SLEEPING | ON | OFF | Closed eyes or panel sleep | Optional |
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

This document defines the intended product policy, **not** a claim of running implementation. The repository currently implements Face rendering/idle expressions and independent attention geometry, but does NOT yet have the local behavior runtime, always-on wake engine, wake model, LCD-night integration, or live camera tracking wired together. No CI is required; add code modularly and label untested hardware behavior.

# Xiao Dai (小呆) — device identity and local wake phrase

## Locked product decision (2026-10-30)

- **Robot's name / displayed name:** 小呆 (`xiǎo dāi`).
- **Only wake phrase:** **小呆小呆** (`xiǎo dāi xiǎo dāi`).
- One standalone **小呆** must **never** initiate waking or a fresh conversation.
- Treat **小呆小呆** as a **single continuous four-syllable acoustic keyword** (allowing a natural short pause between the repetitions), not as two independently recognized `小呆` events that might be merged after a long gap.
- A wake only changes the device from passive sleep/idle to interactive listening. Once engaged, normal conversation **does not require repeating** the wake phrase for each utterance.
- This is a fixed V1 product identity and wake phrase, **not** a user-facing free-text rename setting. Changing the visible name alone must not silently change the wake behavior.

Single source of truth for textual identity: [`shared/bot-identity.json`](../shared/bot-identity.json). Firmware may compile a generated C constant from it or load the corresponding model resource; do not duplicate independent strings in the face app, runtime and server.

## Requirements for implementation

### Offline always-on wake

- `audio_capture` continuously supplies the on-device wake engine with the appropriate audio stream while app is in IDLE, BORED, DROWSY, SLEEPING or NIGHT_DARK.
- No network, LLM, Mac/server, camera or active display is required to recognize the wake phrase.
- Do not upload or persist the normal pre-wake audio stream. The ring buffer is bounded and transient.
- Trigger one `WAKE_DETECTED` event with monotonic timestamp, engine confidence and an event sequence ID on a complete recognized **小呆小呆**.
- Use a configurable threshold/cooldown to prevent a wake echo from retriggering the same session; handle robot playback with echo cancellation or suppression where feasible.
- During an active session, speech can continue without repeated wake-word detection until the interaction manager ends the session.
- A sufficiently high wake threshold should be tuned with realistic Chinese speech, multiple speakers, noise, distance and the standalone `小呆` negative case.

### Recognition model constraint

A custom full-phrase keyword needs a corresponding locally deployable recognizer/model. Setting `wake.phrase` in JSON is **not** sufficient to train, install or activate that model.

Preferred production direction: dedicated on-device WakeNet-compatible **小呆小呆** acoustic keyword model (subject to custom-model provisioning and S31 model/toolchain compatibility).

Development can temporarily use an **explicitly documented** stock wake-word model to bring up the hardware pipeline, but the application must not present it as the final name or accept it as a production alias. If the custom model is not installed, surface `WAKE_MODEL_MISSING` and report **not ready**, rather than pretending that text matching makes a custom wake phrase work.

The included `shared/bot-identity.json` currently declares `recognition_model: "unprovisioned"`. This means the identity decision is saved, **but wake-on-name is not yet implemented or device-tested**.

### Wake behavior

1. Wake keyword triggers device-local `WAKE_DETECTED`; no response to ordinary voices, eye contact or a single `小呆`.
2. `bot_runtime` transitions from its passive state (even NIGHT_DARK) to a waking animation, followed by interactive listening.
3. `face` gently opens both eyes, with a sleepy expression initially; do not require camera detection for animation.
4. `vision_attention` starts when adequate lighting/compute is available; `attention_manager` can look for the speaker/face without making successful visual acquisition a condition of waking.
5. `brain_link` may carry the post-wake conversation to the server; if unavailable, local wake is still valid.
6. Once interaction ends, return to the passive trajectory and optionally to NIGHT_DARK. Acknowledge a wake only once; don't retrigger from the robot's own sound.

### Ownership

- `audio` owns mic input, the local keyword engine and detection thresholds.
- `bot_bus` carries `WAKE_DETECTED` and `WAKE_MODEL_MISSING` notifications.
- `bot_runtime` **alone** authorizes state transition / waking face animation.
- `vision_attention` is not a wake trigger; `attention_manager` selects the person only after the device is engaged.
- `face` renders the transition and does not independently recognize or choose a wake phrase.
- Server does not act as the always-on wake engine.

## No-CI development policy

Following [`AGENTS.md`](../AGENTS.md), don't add CI or automated verification workflows. Implement modules directly. When hardware or model files are unavailable, say explicitly what cannot yet be compiled, run or validated.

## Acceptance criteria (manual later, on hardware)

1. One spoken `小呆`, unrelated conversation, similar names, or the robot's own TTS must not wake it.
2. A naturally spoken `小呆小呆` should wake from NIGHT_DARK without a working server or camera.
3. Phrase separated by an extended delay must not be counted as two independent wake events.
4. Two users with different voices should be able to wake it, without user voice enrollment.
5. If the keyword model is absent or fails to load, report unavailable accurately.

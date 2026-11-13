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
- Trigger one `WAKE_DETECTED` event with monotonic timestamp and backend on a complete recognized **小呆小呆**. Report a native score only when the backend provides one; the bus assigns its own sequence. Never invent a calibrated confidence.
- Use a configurable threshold/cooldown to prevent a wake echo from retriggering the same session; handle robot playback with echo cancellation or suppression where feasible.
- During an active session, speech can continue without repeated wake-word detection until the interaction manager ends the session.
- A sufficiently high wake threshold should be tuned with realistic Chinese speech, multiple speakers, noise, distance and the standalone `小呆` negative case.

### Implemented model path (2026-11-13)

Setting `wake.phrase` in JSON is **not** sufficient to train, install or activate a model. The firmware now declares and packages a real local acoustic recognizer through its pinned **ESP-SR 2.6.1 / ESP32-S31** dependency.

| Priority | Local recognizer | Exact acceptance policy |
| --- | --- | --- |
| 1, when separately supplied | Dedicated S31-compatible WakeNet | Configured model entry and complete metadata must match `小呆小呆`; only model index 1 / word index 1 |
| 2, default available model | Official Chinese MultiNet7 `mn7_cn` | Clear built-in commands; register **one** command ID 1, phrase ID 0, `xiao dai xiao dai`; accept only the top result at or above the configured score |
| Neither usable | Missing/unavailable status | Retain manual touch/audio operation; do not use VAD, face detection, or another keyword to wake |

The default implementation follows the continuous MultiNet custom-keyword pattern used by [XiaoZhi's CustomWakeWord](https://github.com/78/xiaozhi-esp32/blob/67ca62d4bb7d91e514d7ccd5e6977745002f41f4/main/audio/wake_words/custom_wake_word.cc). It does not wait for a stock WakeNet phrase, cloud transcription, a Mac, or a camera. It uses the real [S31 MultiNet interface](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/include/esp32s31/esp_mn_iface.h), [official command registration](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/src/esp_mn_speech_commands.c), and the tone-free, space-separated Chinese pinyin format in [Espressif's command list](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/multinet_model/fst/commands_cn.txt).

This is a general command model configured for the complete phrase, **not a newly trained dedicated WakeNet model**. No `xiao dai` command or two-hit counter exists. A dedicated model can still replace it later, but is not needed to prepare the default firmware for testing.

### Configuration and actual model image

```ini
CONFIG_MODEL_IN_FLASH=y
CONFIG_SR_VADN_WEBRTC=y
CONFIG_SR_MN_CN_MULTINET7_QUANT=y
CONFIG_SR_MN_EN_NONE=y
CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK=y
CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT=80
CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS=3000
CONFIG_BOT_AUDIO_MODEL_PARTITION="model"
CONFIG_BOT_AUDIO_WAKE_MODEL_NAME=""
```

`mn7_cn` and its `fst` resources ship with the pinned upstream component. The official [model packer](https://github.com/espressif/esp-sr/blob/b6774a31118de04742e51ab57dd68c9001fa1091/model/movemodel.py) generates `build/srmodels/srmodels.bin`. The minimum combination is about **2,681,351 bytes / 2.557 MiB**, fitting the existing 6 MiB model region; other selected models add to it. This is a **flash budget**, not measured runtime RAM. Never mount or format this raw model image as SPIFFS even though the partition table uses that subtype.

Existing `sdkconfig` values take precedence over defaults. Follow [wake-test.md](wake-test.md) to prepare the existing checkout without erasing Wi-Fi/NVS credentials. The first installation must update the generated `model` image as well as the application; `idf.py app-flash` alone leaves the old model contents in place. Full `idf.py flash` writes the generated model image, so retain an existing dedicated model's external path if one has already been provisioned. Do not replace an installed model with a blank build or invent a vendor directory name.

### Recognition lifetime and own-speaker suppression

One capture owner feeds official AFE mono PCM to MultiNet. A single bounded, aligned PSRAM staging frame adapts native frame sizes without resampling or retaining a pre-wake recording. No inference runs under the owner's spinlock. It uses `detect` continuously and `clean` after detection or a 3000 ms native timeout; permission-epoch changes also discard partial state.

The initial model-score threshold is **0.80**. Accepted wake events are at least **2000 ms** apart. Continuous **600 ms below −48 dBFS**, measured by AFE before AGC, clears partial state so a later separate call cannot reuse that history. A natural short pause within the full phrase remains possible. These are implementation thresholds awaiting acoustic tuning; **no finite-word model guarantees zero false recognition**. The requirement that standalone `小呆` never activates the robot remains an acceptance criterion to test, not a fabricated result of this source change.

From any playback BEGIN, including a queued local greeting, `bot_audio` blocks wake events, VAD, ambient-sound reactions and PCM uplink. It continues through the default **800 ms** speaker tail, then discards RX DMA backlog plus one capture block and resets AFE/VAD/MultiNet before listening resumes. Explicit touch/stop cancels output. AEC and automatic voice interruption remain unavailable; this suppression is not claimed to be measured echo cancellation.

`wake_status=READY` plus `wake_backend=MULTINET7` means initialization succeeded; `model_name=mn7_cn` identifies the actual model. Healthy capture/AFE is additionally required for `wake_available`. MultiNet wake events expose the original top-result score with `confidence_available=true`; dedicated WakeNet has no corresponding event score and keeps the flag false. A score is not measured recall, false-accept rate, or identity confidence.

### Wake behavior

1. Wake keyword triggers device-local `WAKE_DETECTED`; no response to ordinary voices, eye contact or a single `小呆`.
2. `bot_runtime` transitions from its passive state (even NIGHT_DARK) to waking, optionally authorizes a local acknowledgement and its expression, then enters interactive listening after the actual playback completion.
3. `face` gently opens both eyes, with a sleepy expression initially; do not require camera detection for animation.
4. `vision_attention` starts when adequate lighting/compute is available; `attention_manager` can look for the speaker/face without making successful visual acquisition a condition of waking.
5. `brain_link` may carry the post-wake conversation to the server; if unavailable, local wake and the built-in acknowledgement remain valid. The local acknowledgement keeps the conversation audio gate closed; its playback uses the sole audio owner and an independent runtime token, so a WSS session cannot replace it.
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

The codec/AFE hardware work recorded on 2026-11-06 is historical evidence only.
The 2026-11-13 MultiNet and local-response changes have received source review
and an off-board C syntax/API check of the keyword adapter against exact
ESP-SR 2.6.1 headers with minimal platform declarations. This session has no
S31 toolchain or board: no ESP-IDF build/link, flashing, microphone wake test,
CPU/RAM peak measurement, or acoustic false-wake result is claimed.

## Acceptance criteria (manual later, on hardware)

1. One spoken `小呆`, unrelated conversation, similar names, or the robot's own TTS must not wake it.
2. A naturally spoken `小呆小呆` should wake from NIGHT_DARK without a working server or camera.
3. Phrase separated by an extended delay must not be counted as two independent wake events.
4. Two users with different voices should be able to wake it, without user voice enrollment.
5. If the keyword model is absent or fails to load, report unavailable accurately.
6. Local acknowledgement plays once with matching expression, completes or cancels with the original token/source/ID, and does not enter the network PCM path.
7. Measure missed complete phrases and false wakes separately across distance, speaker voices, noise, natural short pauses, long gaps, repeated calls and real speaker playback. Record internal heap/PSRAM peak and sustained capture/AFE health while vision and Wi-Fi run; do not substitute an S3 benchmark for an S31 measurement.

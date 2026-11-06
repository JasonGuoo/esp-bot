# Main composition contract

Public API is `include/bot_audio.h`. Use `bot_audio_default_config()` → set callback/context → `bot_audio_create()` → `bot_audio_start()`. `bot_audio` alone creates one ES8389 IN_OUT codec. Main/platform may call the idempotent `bsp_i2c_init()` for the shared bus but **must not call either BSP codec helper or read/write I2S**. Audio must make the first `bsp_audio_init()` call so the BSP does not retain someone else's sample configuration.

Callbacks post bus events; they must be short, nonblocking, thread-safe, and never call start/stop/destroy. Copy the audio token's epoch/session_id/turn_id/focus_generation explicitly to the runtime token. Copy borrowed reason/PCM before returning. All observed timestamps are `esp_timer_get_time()`; divide by 1000 for bus milliseconds. VAD events have `echo_safe=false`; automatic VAD barge-in must remain disabled. Wake events only contain the complete `小呆小呆` keyword. Wake `echo_safe=true` means the speaker/tail suppression gate was clear, not validated AEC. AFE has no calibrated confidence output; `confidence_available=false`, so do not invent a measured score in diagnostics.

Reconcile `bot_audio_set_session(audio, &original_runtime_token, snapshot.audio_uplink_permitted)` when audio_session_requested is true. Pass NULL otherwise. `audio_session_requested` authorizes local VAD; only `audio_uplink_permitted` releases postauthorization PCM. For the current plaintext/local-only link, keep `on_pcm=NULL` and `deliver_pcm=false`. Stop output via `bot_audio_playback_cancel(audio, 0)`; this does not close the microphone and its latch does not depend on queue space. Sleeping/night UI states must leave this audio owner started.

Repeated reconcile calls with unchanged token/permission are idempotent. A changed token or permission flushes the old RX window and resets AFE/VAD before events for the new token. If WAITING → utterance START causes runtime to allocate a new turn token, continuing speech can emit START again under that new token and later END under the same token. Runtime should ignore a duplicate START when that utterance is already active. Do not substitute the runtime's newest token into an older callback. Receivers must reject stale callbacks already in flight at revocation.

Startup reports HEALTH unavailable until the first successful microphone read. Missing model is nonfatal; codec failure is a start error. AFE failure can leave raw/manual capture available with VAD/wake unavailable. Capability flags for AEC, automatic barge-in, hardware capture timestamps, and voice identity remain false. `stop(timeout_ms)` accepts 0..10000 and retains resources on timeout; retry later, never free a live instance. Destroy only after external producer/status callers are quiesced.

## Dependency and model configuration

- BSP: `espressif/esp32_s31_korvo_1 =1.1.0`.
- Codec: `espressif/esp_codec_dev =1.5.11`, compatible with BSP's `~1.5` range.
- Speech: `espressif/esp-sr =2.6.1`.
- Project baseline: IDF >=6.1, target `esp32s31`; `CONFIG_CODEC_ES8389_SUPPORT=y`; `CONFIG_BSP_ERROR_CHECK=n` for recoverable BSP returns.
- Real ESP-SR symbols: `CONFIG_MODEL_IN_FLASH=y`, `CONFIG_ESP_SR_EXTERNAL_MODEL_PATH="/actual/vendor/export/root"`, `CONFIG_SR_VADN_WEBRTC=y`.
- Project symbols: `CONFIG_BOT_AUDIO_MODEL_PARTITION="model"`, `CONFIG_BOT_AUDIO_WAKE_MODEL_NAME="actual_exact_model_directory_name"`.
- Partition: parent owns the partition table; `model,data,spiffs,0x810000,0x600000` is the planned 6 MiB model region on 16 MiB flash. It holds the official raw SR model image and must not be mounted/formatted as SPIFFS.

The custom Espressif-compatible ESP32-S31 WakeNet export must contain one complete `小呆小呆` keyword and matching metadata. No stock wake model is selected as a substitute. Blank, missing, malformed, or nonmatching model → `BOT_AUDIO_WAKE_MODEL_MISSING`; codec/manual interaction can still start. Install the actual vendor model, set the exact model name/external root, then use the official model packer; stop/start reloads it. See README for future-only commands and immutable upstream sources.

**Flash integration:** ESP-SR 2.6.1's CMake hardcodes the partition name `model`, generates `build/srmodels/srmodels.bin`, and attaches it to the full `flash` target. A later complete flash with no correct external-model configuration can overwrite an earlier manually flashed custom model, even when no stock WakeNet is selected. Its explicit generation dependency is sdkconfig; changed external weights alone require regenerating the model image. Do not put the sole vendor export in the generated `build/srmodels` directory, which the official packer recreates.

Hardware is unavailable; no compilation, tests, CI, or flashing have been run for this component.

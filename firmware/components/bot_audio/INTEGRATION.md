# Main composition contract

Public API is `include/bot_audio.h`. Use `bot_audio_default_config()` → set callback/context → `bot_audio_create()` → `bot_audio_start()`. `bot_audio` alone owns the I2S channels and ES8389 IN_OUT codec. Main/platform may call the idempotent `bsp_i2c_init()` for the shared bus but **must not call `bsp_audio_init()`, either BSP codec helper, or read/write that I2S peripheral**. The owner uses the official board DOA example's four-slot TDM input selection and frees its own I2S/data interface at stop.

Callbacks post bus events; they must be short, nonblocking, thread-safe, and never call start/stop/destroy. Copy the audio token's epoch/session_id/turn_id/focus_generation explicitly to the runtime token. Copy borrowed reason/PCM before returning. All observed timestamps are `esp_timer_get_time()`; divide by 1000 for bus milliseconds. VAD events have `echo_safe=false`; automatic VAD barge-in must remain disabled. Wake events only contain the complete `小呆小呆` keyword. Wake `echo_safe=true` means the speaker/tail/DMA/AFE suppression gate was clear, not validated AEC. `wake_backend` identifies dedicated WakeNet or full-phrase MultiNet7. WakeNet has `confidence_available=false`; MultiNet7 provides its highest native score with that flag true. This score is not a calibrated probability or measured accuracy.

Reconcile `bot_audio_set_session(audio, &original_runtime_token, snapshot.audio_uplink_permitted)` when audio_session_requested is true. Pass NULL otherwise. `audio_session_requested` authorizes local VAD; only `audio_uplink_permitted` releases postauthorization PCM. For the current plaintext/local-only link, keep `on_pcm=NULL` and `deliver_pcm=false`. Stop output via `bot_audio_playback_cancel(audio, 0)`; this does not close the microphone and its latch does not depend on queue space. Sleeping/night UI states must leave this audio owner started.

Repeated reconcile calls with unchanged token/permission are idempotent. A changed token or permission flushes the old RX window and resets AFE/VAD before events for the new token; the keyword engine also drops partial state across gate changes. If WAITING → utterance START causes runtime to allocate a new turn token, continuing speech can emit START again under that new token and later END under the same token. Runtime should ignore a duplicate START when that utterance is already active. Do not substitute the runtime's newest token into an older callback. Receivers must reject stale callbacks already in flight at revocation.

## Local acknowledgement playback

Runtime alone decides whether a complete wake should receive a local reply. Use
`bot_audio_playback_begin_local(audio, &runtime_token, &id)`, then the existing
`bot_audio_playback_write`, `bot_audio_playback_end` and `bot_audio_playback_cancel`.
The four-field token has nonzero epoch/session_id but need not match an open
audio/network session. This API never opens the microphone permission gate.
Local write/end validate the playback ID, so an unrelated WSS session transition
cannot invalidate or replace the greeting.

All playback events contain `playback_source`: `BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE`
or `BOT_AUDIO_PLAYBACK_SESSION`. Copy this distinction into bus events and match
the original token plus ID. Wait for real STARTED/DONE/FAILED/CANCELLED events;
do not guess completion from text length. `set_session` only auto-cancels the
SESSION kind. Runtime must call cancel when a local operation ends, is replaced,
or loses authorization. Both kinds share a single codec and bounded queue;
another begin is refused while either reservation is active.

From successful BEGIN through the 800 ms default tail, every playback blocks
wake, VAD and `on_pcm`, including the pre-first-frame queue interval. After the
tail the capture owner discards one complete RX DMA capacity plus an input frame
and resets AFE/VAD/keyword state before release. This also applies when the
network permission was already true. Runtime should keep its audio session/uplink
closed during the local greeting and open it after completion. There is no AEC
or voice barge-in; explicit touch/cancel remains available.

Startup reports HEALTH unavailable until the first successful microphone read. Missing model is nonfatal; codec failure is a start error. AFE failure can leave raw/manual capture available with VAD/wake unavailable. Capability flags for AEC, automatic barge-in, hardware capture timestamps, and voice identity remain false. `stop(timeout_ms)` accepts 0..10000 and retains resources on timeout; retry later, never free a live instance. Destroy only after external producer/status callers are quiesced.

## Dependency and model configuration

- BSP: `espressif/esp32_s31_korvo_1 =1.1.0`.
- Codec: `espressif/esp_codec_dev =1.5.11`, compatible with BSP's `~1.5` range.
- DOA: `espressif/esp_apa_doa =1.0.0`, licensed for use with Espressif products; see its component license.
- Speech: `espressif/esp-sr =2.6.1`.
- Project baseline: IDF >=6.1, target `esp32s31`; `CONFIG_CODEC_ES8389_SUPPORT=y`; `CONFIG_BSP_ERROR_CHECK=n` for recoverable BSP returns.
- Real ESP-SR defaults: `CONFIG_MODEL_IN_FLASH=y`, `CONFIG_SR_VADN_WEBRTC=y`, `CONFIG_SR_MN_CN_MULTINET7_QUANT=y`, `CONFIG_SR_MN_EN_NONE=y`.
- Project defaults: `CONFIG_BOT_AUDIO_MODEL_PARTITION="model"`, `CONFIG_BOT_AUDIO_WAKE_MODEL_NAME=""`, `CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK=y`, threshold percent 80, native window 3000 ms.
- Partition: parent owns the partition table; `model,data,spiffs,0x810000,0x600000` is the planned 6 MiB model region on 16 MiB flash. It holds the official raw SR model image and must not be mounted/formatted as SPIFFS.

The default full-phrase fallback uses the actual `mn7_cn` packaged by ESP-SR
2.6.1, plus its `fst` resources. It needs no custom vendor export and never
registers a standalone `xiao dai`. Its only command is ID 1, phrase ID 0,
`xiao dai xiao dai`. It continuously calls the official detector without first
requiring a different WakeNet keyword. A single aligned PSRAM frame handles
AFE/MultiNet frame-size differences; both remain 16 kHz mono PCM16.

A separately provisioned compatible S31 WakeNet with exact full `小呆小呆`
metadata remains preferred. Set its exact model name and
`CONFIG_ESP_SR_EXTERNAL_MODEL_PATH` while keeping the generated image intact.
No stock wake word becomes an alias. If neither backend is usable, report
missing/unavailable and retain manual interaction. See README for immutable
official API/model sources and the XiaoZhi continuous-custom-wake precedent.

First-board update: the selected one-mic path requires equal feed/fetch block
sizes and executes official reset/feed/fetch calls in the single capture worker.
Playback remains independent. AFE freshness comes from successful processing,
not just microphone reads. ES8389 1.5.11's `set_mute` writes the ADC mute bits;
normal speaker silence therefore uses PA GPIO 7 and TX zero flushing, preserving
the microphone. Codec close may still mute ADC after capture has stopped.
Keep the official AFE ring capacity. Its reserved onset history includes both
`vad_min_speech_ms` and `vad_delay_ms` (256 ms with this configuration), plus
room for incoming PCM. Four or eight 32 ms frames alone eventually leave no
feed space. This history is not exported. A zero feed result still allows a
bounded fetch so temporary backpressure cannot permanently stop draining work.

**Flash integration:** ESP-SR 2.6.1's CMake hardcodes the partition name `model`, generates `build/srmodels/srmodels.bin`, and attaches it to the full `flash` target. A later complete flash with no correct external-model configuration can overwrite an earlier manually flashed custom model, even when no stock WakeNet is selected. Its explicit generation dependency is sdkconfig; changed external weights alone require regenerating the model image. Do not put the sole vendor export in the generated `build/srmodels` directory, which the official packer recreates.

The first real S31 build and flash were explicitly requested and performed on
2026-11-06. See [the board record](../../../docs/board-bringup-2026-11-06.md)
for actual per-feature acceptance; do not infer acoustic validation from the
build. The MultiNet and local-source playback additions on 2026-11-13 have not
been built in ESP-IDF or run on hardware in this session. An off-board syntax/API
check of the keyword adapter uses exact ESP-SR 2.6.1 headers and minimal platform
declarations; it does not validate linking, S31 execution, timing or acoustics.
CPU/RAM peak, false wakes, missed calls, repeated calls and self-playback remain
manual board checks. No CI was added.

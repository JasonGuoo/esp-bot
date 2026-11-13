# Main composition contract

Public API is `include/bot_audio.h`. Load saved `bot_voice_settings_t`, use `bot_audio_default_config()` with `output_volume = muted ? 0 : volume_percent`, set callbacks/context, create, call `bot_audio_set_controls()` with the complete saved settings, then start. Pre-start requests survive the status reset; PA stays off during initialization. `bot_audio` alone owns I2S and the ES8389 IN_OUT codec. Platform may call idempotent `bsp_i2c_init()` for the shared bus, but must not call `bsp_audio_init()`, either BSP codec helper, or read/write that I2S peripheral.

Callbacks must be short, nonblocking and thread-safe; never call start/stop/destroy. Copy epoch/session_id/turn_id/focus_generation explicitly to the runtime token, and copy borrowed reason/PCM before returning. All observed times are local `esp_timer_get_time()`; divide by 1000 for bus milliseconds. VAD has `echo_safe=false`; automatic VAD barge-in remains disabled. Wake events only carry the complete `小呆小呆`. Wake `echo_safe=true` means local playback/tail/DMA/AFE suppression was clear, not validated AEC. WakeNet has no available score; MN's native score is uncalibrated, never measured accuracy.

## Capture lease and follow-up

Reconcile `bot_audio_set_session_ex(audio, &original_runtime_token, &options)` only while runtime authorizes that session; pass NULL/NULL to close. Its options are explicit:

| Option | Meaning |
| --- | --- |
| `deliver_pcm` | Current authenticated transport is ready to consume this token's PCM |
| `retain_start` | Permit bounded RAM-only capture after this token's RX/AFE fence |
| `waiting_for_followup` | Use stricter VAD onset while waiting for the next sentence |
| `local_commands` | Permit the fixed local control command graph after authorization |

LISTENING/WAITING may enable retain/commands. THINKING/REPLY_PRESENT keep the session with all three capture permissions false so SESSION playback still works. Legacy `set_session` enables neither retention nor fixed commands. LAN v1 diagnostics never authorize audio.

Main enables retain only with an available authenticated encrypted WSS control connection that can negotiate this turn's media; this is separate from actual media readiness. Offline commands remain enabled with retain false, so a long local command is not interrupted by an irrelevant upload-buffer overflow. The PCM media transport itself remains the project's existing negotiated policy.

Identical requests are idempotent. New token/close changes capture epoch, discards a full RX DMA capacity plus one block, then resets AFE/VAD before `CAPTURE_READY.available=true`. This is local readiness, not network readiness. Same-token transport/waiting changes preserve the fence. Runtime prepares the next token **before WAITING**; VAD START consumes that token without incrementing it. Transport may negotiate it in advance; delivery stays false until local speech and all permissions are ready. Never relabel old PCM. Receivers must still reject stale callbacks already in flight at revocation.

Retained PCM is limited to complete frames within `CONFIG_BOT_AUDIO_START_BUFFER_MS` (default 800 ms, 25,600 PCM bytes plus frame metadata) in PSRAM. Both initial LISTENING and WAITING roll the latest authorized window until VAD START; silent network negotiation can exceed the window without failing. VAD START protects the onset with a 256 ms local observation-time margin. If waiting for transport would overwrite protected audio, the owner wipes and latches that token closed, then emits `BUFFER_OVERFLOW`; runtime cancels it and offers a retry. This depends on functioning VAD and is not a completeness guarantee when VAD misses speech.

Up to four retained frames are delivered per capture iteration before current audio, retaining original sequence/token/read timestamps. Every callback rechecks authorization. Stop, playback BEGIN, token changes, retain revocation, delivery revocation, command consumption and read failures wipe the window. Native `vad_cache` is never exported. `buffered_drops` counts known protected-onset loss; ordinary pre-onset rolling eviction is excluded.

`LOCAL_COMMAND` carries a shared `bot_voice_command_t` and uncalibrated score. Before emitting it the owner latches that token's PCM closed and wipes retained audio. Runtime alone executes/rejects it, cancels already-sent prefixes remotely, and establishes a new token when appropriate. Repeated same-token reconcile cannot reopen a consumed/overflowed turn. Passive mode has only the complete wake phrase; commands never start a conversation. Playback blocks these commands too: there is no acoustic stop/barge-in promise.

Sleeping/night display states leave the owner started. Closing settings or Stop must revoke the capture session and any preview/local-response lease in runtime.

## Settings and bounded diagnostics

`bot_audio_set_controls()` queues a copy; the caller performs no codec/model I/O. Only the capture owner applies volume (0..60), mute and native wake threshold (40..99). Equal requests remain no-ops after errors; an explicit edit produces a new revision. `CONTROLS_APPLIED` reports revision/error; status exposes requested/applied values and revisions. Platform owns NVS persistence. `night_gain_percent` and `spoken_answers` are retained here but acted on by the composing response player/Mac policy.

No UI code calls codec APIs. Mute preserves volume; output checks pending/applied mute each block and writes zeros with PA off. ES8389 1.5.11 `set_out_mute` changes ADC mute bits, so it must never be used for runtime speaker mute. Codec close may mute ADC after capture has stopped.

Diagnostics contain bounded counters, levels and last/max timings, never recordings. `read_us` includes blocking capture, `afe_us` includes feed/fetch/keyword/event work, `keyword_us` measures native detect, and `processing_us` is post-read wall time. `capture_overruns` means processing exceeded a frame duration, not measured hardware lost samples. Buffer and output queue watermarks, read errors, native feed rejections, keyword timeouts/rejections, playback underruns and last wake/capture-ready/playback stage times expose concrete bottlenecks.

AFE gets at most the configured automatic reset attempts (default two, spaced 1.5 s and outside playback suppression). `bot_audio_retry_afe()` queues one explicit existing-buffer/VAD reset. Neither path restarts I2S or loads a missing model; an AFE that was never created cannot retry. Persistent failure exposes degraded manual/raw operation. Initial codec or required buffer allocation failure returns a start error.

## Playback sources and actual progress

One bounded queue accepts 16 kHz mono signed PCM16LE, 1..320 samples per write. BEGIN reserves silently; write/end validate the playback ID. Queue-full `ESP_ERR_TIMEOUT` is backpressure, so a producer must retry only while its original lease remains valid.

| Source | Begin API | Authorization |
| --- | --- | --- |
| SESSION | `bot_audio_playback_begin` | Must match the retained open session token |
| LOCAL_RESPONSE | `bot_audio_playback_begin_local` | Runtime's independent operation token; nonzero epoch/session_id |
| PREVIEW | `bot_audio_playback_begin_preview` | Explicit settings preview epoch; event token `{epoch,0,0,0}` |

Local/preview playback opens no microphone permission. All events retain original token/id/source. Runtime validates those three fields; settings closure/Stop explicitly cancel preview and local leases. Session changes only auto-cancel SESSION. No competing BEGIN can replace an existing reservation.

STARTED follows the first successful PCM codec write. PLAYBACK_PROGRESS uses the same token/id/source and is emitted about once per second only after successful PCM writes. Network bytes, empty queues, starvation padding and DMA flushing do not emit progress. Runtime may renew an already matching SPEAKING operation from this event; it creates no capture permission and does not extend the protocol's maximum reply duration. Muted logical playback writes zeros and completes normally.

END drains prior audio before PA-off/zero purge and DONE. `bot_audio_playback_cancel(audio, 0)` sets a capacity-independent latch and gates PA immediately; the worker purges the queue/DMA and emits CANCELLED. Errors emit FAILED; failed drain prevents subsequent playback until successful stop/restart. These are software/driver completion observations, not measured acoustic end times.

Every successful BEGIN, including pre-first-frame reservation, blocks wake/VAD/ambient/PCM and clears retained onset. After completion, default tail is 800 ms, then one RX DMA capacity plus one capture frame is discarded and AFE/VAD/MN state reset before release. Runtime waits for completion and this tail-clear state before authorizing subsequent listening. There is no AEC or voice barge-in; explicit touch/cancel remains available.

## Official dependencies and model configuration

- BSP `espressif/esp32_s31_korvo_1 =1.1.0`; codec `espressif/esp_codec_dev =1.5.11`; DOA `espressif/esp_apa_doa =1.0.0`; speech `espressif/esp-sr =2.6.1`.
- Target is ESP32-S31 / IDF >=6.1; `CONFIG_CODEC_ES8389_SUPPORT=y`, PSRAM, and `CONFIG_BSP_ERROR_CHECK=n` for recoverable BSP errors.
- Model defaults: `CONFIG_MODEL_IN_FLASH=y`, `CONFIG_SR_VADN_WEBRTC=y`, `CONFIG_SR_MN_CN_MULTINET7_QUANT=y`, `CONFIG_SR_MN_EN_NONE=y`.
- Project defaults: model label `model`, dedicated WakeNet name empty, full-phrase MN fallback enabled, wake threshold80, local-command threshold85, native window3000 ms.
- The 6 MiB raw model partition is not mounted/formatted as SPIFFS. Use the generated partition table and model image when flashing.

The actual pinned `mn7_cn` plus `fst` resources are packaged. Passive graph is command1/phrase0 `xiao dai xiao dai`; no standalone name is registered. Explicit listening adds only fixed commands10..18 listed in README. The single aligned MN input staging frame accommodates AFE/MN frame sizes, both 16 kHz mono. Dedicated compatible S31 WakeNet with exact whole-keyword metadata remains preferred. Keep MN enabled for active commands with WakeNet; it is idle during passive WakeNet mode. Disabling fallback also disables those commands. No stock wake word becomes an alias.

The one-mic AFE path requires equal feed/fetch sizes and serializes reset/feed/fetch on its capture worker. Keep the official 50-frame native ring: minimum speech plus VAD delay reserve 256 ms before room for new PCM. Reducing it to four/eight 32 ms frames previously stalled feeding. Zero feed return still permits bounded fetch to drain temporary backpressure. This internal history is never exported.

ESP-SR 2.6.1 CMake hardcodes partition name `model`, generates `build/srmodels/srmodels.bin`, and adds it to full `flash`. A complete flash without correct external-model configuration can overwrite a previously provisioned dedicated model. Generation depends explicitly on sdkconfig; changed external weights alone need regeneration. Keep vendor source weights outside generated `build/srmodels`. README links fixed official API/model commits and the XiaoZhi continuous-MN precedent.

## Verification scope

The historical first real S31 build/flash on 2026-11-06 is recorded in [the board record](../../../docs/board-bringup-2026-11-06.md); it does not validate later additions. The 2026-11-13 MN/controls/active-command/onset/preview/progress changes have no ESP-IDF build, S31 run, flashing, model inference or acoustic acceptance in this session. All four audio C files passed an off-board C11 `-Wall -Wextra -Werror -fsyntax-only` check using exact ESP-SR 2.6.1 headers plus minimal platform declarations. It cannot validate linking, native model behavior, S31 execution, timing or acoustics. CPU/RAM peaks, graph-switch cost, false/missed commands, runtime codec controls, self-playback, long-answer progress and first-word capture still require physical testing. No CI was added.

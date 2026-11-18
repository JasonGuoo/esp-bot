# Household companion firmware integration — 2026-11-16

This record describes source integration for #10, HC-06/#19, HC-08/#21 and
HC-14/#27. No ESP-IDF build, model inference, flashing, real-board audio/display
run or CI was performed in this delivery environment. Regression cases were
added to the existing optional runtime test source; they have not been run.

## Live spoken turns on the paired Mac

On November 18 the installed App and the S31 exercised the live protocol over their paired connection. The gateway advertises `conversation.live.v1`. The device requests `output.live_turns` with segmented presentation and requires the server to confirm it. An older peer continues to use the existing presentation protocol.

- `audio.pause` names the current stream, runtime token and count of drained PCM frames. The device keeps a bounded, same-token microphone prefix during the 200 ms final drain. A resumed utterance invalidates the speculative ASR result. Only `audio.commit` starts a turn.
- Each independent sentence may arrive as an immutable content segment while the producer is still working. New plan indices advance only after segment completion. A final segment, or `presentation.end` naming the last completed plan, lease, index, sequence and runtime token, releases the device for follow-up. A progress acknowledgement can precede the first content segment.
- `assistant.supplement` retires the previous output and reserves its generating Luna turn when available. Only the immediately succeeding microphone token in that activation can use the reservation. The touchscreen exposes 补一句, 重新说 and 停止. The waiting-mode MultiNet graph also includes 补一句 while output is silent.
- `audio.playback.report` distinguishes received frames and bytes from `sample_position`, the number of samples successfully written to the codec. Device monotonic timestamps at VAD release, commit and first write support end-to-end timing without synchronizing clocks. Only a fully played narrated sentence contributes to the spoken-answer context.

The [installed-App and S31 acceptance record](../docs/review-live-turns-2026-11-18.md) includes timings and the acoustic command result.

## Single owner and cancellation

`bot_audio` remains the sole ES8389/I2S input/output owner. `bot_runtime` owns
activation and interruption. `brain_secure` prepares authenticated, bounded
media and receives actual hardware/display receipts. A household/day/task ID
never grants microphone or display authority. Every asynchronous operation keeps
its original runtime token and its presentation generation; stale data is dropped.

Playback purpose is separate from playback source. Purposes are `wake_ack`,
`task_ack`, `progress`, `content`, `final`, `preview`. Sources still distinguish
SESSION, LOCAL_RESPONSE and PREVIEW. A managed segment's hardware DONE clears
its speaking state and returns to THINKING. It does not arm follow-up, including
when its purpose is `final`: the local presentation receipt collector must first
finish all required outputs. Only `BOT_COMMAND_PRESENTATION_DONE` then arms the
next capture token. Legacy single replies retain their previous behavior.

An interruption retires the output lease, stops the current hardware stream,
and begins a new capture token with RX/AFE fences. It does not grant authority
to cancel a persistent Mac printing or background task; the server owns those
separate lifecycles. Old onset samples are never relabelled as a new turn.

## Settings close causality (#10)

Each UI settings visit has a monotonic page `settings_lease` and edit revision.
An accepted edit is copied by the UI owner. The safety-priority CLOSE event carries
that last accepted snapshot. Runtime commits it before retiring the page, even
when CLOSE overtakes a normal-queue VOICE_SETTINGS event. Old edits and closes
cannot mutate a newer page. Stop remains a separate capacity-independent latch.
No queue-priority downgrade was used to fix the race.

## Segmented presentation protocol

Gateway welcome advertises `presentation.segments.v1`. Session output advertises:

```json
{
  "presentation": {"version":1,"max_segments":64,"max_duration_ms":120000,"frame_ack":true},
  "video": {"mime":"video/x-motion-jpeg","transport":"jpeg_frames","max_width":480,
    "max_height":272,"max_frame_bytes":65536,"max_fps":5,"max_duration_ms":15000}
}
```

The existing `audio.reply.v1` and `display.reply.v1` remain required for their
respective media. The default spoken-answer preference remains unchanged.
A successful commit response may contain `payload.presentation_pending=true`;
it must not also finish the legacy single-reply path for that response.

The WSS event `presentation.segment` carries the original `runtime` and payload:

```json
{
  "presentation_id":"plan-1", "output_lease_id":"lease-1", "presentation_index":1,
  "segment_id":"segment-1", "seq":1, "purpose":"task_ack", "final":false,
  "hold_ms":200, "scene":{"speech":"好，我来看看。","expression":"thinking","cards":[]},
  "audio":null
}
```

`audio` uses the existing immutable PCM descriptor; image cards use the existing
`asset` descriptor (JPEG, dimensions, byte size and SHA-256). Device-side limits
are 16 kHz mono PCM16, 320 samples/frame, one active segment, one image per segment,
64 total segments per runtime turn, and 120 seconds per frozen presentation.
`seq` starts at 1 and increases strictly. After a completed `task_ack`/`progress`
plan, a new immutable plan and output lease may start at `seq=1` with a strictly
incremented `presentation_index`; previous indices cannot be replayed. The UI
uses `(presentation_index - 1) * 64 + seq` as its generation, so repeated text or
a first segment in a later plan still receives an independent real render receipt.

For motion JPEG frames include `media_kind:"mjpeg_frame"`. They use the same
bounded JPEG decoder and LVGL image ownership path, with a 200 ms minimum visible
dwell and a 15 second continuous clip budget. This is a backpressured JPEG-frame
player, not an MP4/H.264 decoder or an assertion of achieved frame rate. Mac must
prepare source videos/animations into supported frames or present other formats
on its own richer viewer. No full clip is allocated on the device.

Narration waits for the text/image LVGL flush before `audio.playback.ready`.
A failed required display produces a failed segment without starting its frozen
narration. PCM remains on the current authenticated
plain TCP/HMAC channel negotiated by `audio.begin`; do not close that channel at
an intermediate segment's E frame or hardware terminal. Every audio segment has
its own stream ID, sequence and existing ready/report handshake. The channel is
closed at terminal presentation/follow-up/cancellation.

After all requested media have completed and the hold has elapsed, device sends
`presentation.segment.report` with `runtime`, presentation/output lease/segment
IDs, `presentation_index`, `seq`, and `status:"completed"|"failed"`, plus `code`.
The gateway must wait for this receipt before sending the next segment. A failed
receipt is not a successful render. Unsupported descriptors, sequence violations
and revoked tokens are never admitted. The existing image failure captions and
text fallback remain available. No remote control event can claim a hardware
playback terminal or a physical display flush.

## Real playback reference / AEC source integration

Official references inspected before changes:

- [ESP-SR S31 AFE](https://docs.espressif.com/projects/esp-sr/en/latest/esp32s31/audio_front_end/README.html):
  interleaved M/R/N format, feed/fetch channels and 16 kHz constraints.
- [Espressif GMF S31 AEC example configuration](https://github.com/espressif/esp-gmf/blob/main/elements/gmf_ai_audio/examples/aec_rec/sdkconfig.defaults.esp32s31):
  `CONFIG_GMF_AI_AUDIO_AEC_CH_ALLOCATION="MRMN"` at 16 kHz.
- [Official ES8389 codec](https://github.com/espressif/esp-adf/blob/master/components/esp_codec_dev/device/es8389/es8389.c):
  `no_dac_ref=false` enables the internal DAC reference. The project continues to
  pin esp_codec_dev 1.5.11 rather than adopting unrelated current-main APIs.
- [Official S31 BSP audio owner construction](https://github.com/espressif/esp-bsp/blob/master/bsp/esp32_s31_korvo_1/src/bsp_audio.c).

`CONFIG_BOT_AUDIO_HARDWARE_REFERENCE_AEC` defaults off. When enabled and resources
permit, the same codec owner captures all four 16-bit TDM slots: microphone,
DAC reference, second microphone, unused. The actual same-frame MRMN buffer feeds
ESP-SR AFE with AEC enabled. The microphone pair remains available to the existing
DOA consumer; authorized PCM uplink stays mono. Output writes fill the matching
four-slot format. There is no second codec, guessed software-reference delay, or
claim of hardware capture timestamps.

The AFE checks its effective configuration, native feed/fetch sizes, input/output
channel counts and sample rate. Memory/format/optional-path initialization failure
falls back to the existing mono-AFE/two-selected-slot half-duplex path before any
workers start. Default playback/tail suppression and post-wake-only PCM upload
remain unchanged.

`CONFIG_BOT_AUDIO_AEC_BARGE_IN` is a second default-off opt-in. An active SESSION
playback, matching capture/playback token, fresh unclipped nonzero hardware
reference, 300 ms reference stabilization, bounded AFE backlog, recent hardware
writes and 200 ms sustained speech in the actual AFE output are all required.
Missing reference, capture errors, mute, cancellation, playback changes or token
changes close that path. Detection emits a local BARGE_IN event with the original
playback token/id. Runtime retires that output, establishes a fresh token/fence,
and only subsequent authorized microphone samples can be uploaded.

Status exposes actual AEC availability, live reference validity/peak, observed
reference frames/rejections and barge-in count. These are diagnostics, not measured
recognition accuracy, echo-return-loss, clock-drift or acoustic calibration results.
Real-board validation must cover near/far double-talk, self speech, volume/mute,
clip/underrun, prolonged silence, repeated cancel, long-run drift, and capture-ready
latency before either opt-in is enabled for normal use. Touch/Stop remains the
available interruption path with the default build.

## Follow-up static review — 2026-11-16

Three source-level ordering/failure defects from the initial integration are
corrected; these changes have not been built or exercised on hardware:

- A turn-scoped cancellation after foreground generation now sends a matching
  terminal event even when the device is showing a silent segment or waiting
  for a durable task. Its output lease, audio transport and current image are
  retired without cancelling the separately owned durable task.
- A download, JPEG decode or display failure prevents unstarted narration from
  referring to the missing picture. The device sends an audio-unavailable
  terminal before its failed aggregate segment receipt.
- The final presentation command may race the runtime's separate hardware
  event queue. If the matching playback is still recorded as active, runtime
  keeps the completion intent until its genuine hardware DONE arrives. A failed
  playback, cancellation or new turn cannot reuse that pending intent.

Pending manual checks: cancel a silent segment and a task-wait; reject an image
after its descriptor while audio is ready; and check final completion when the
presentation command is consumed before the matching hardware DONE event.

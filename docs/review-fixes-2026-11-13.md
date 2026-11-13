# Review fixes and response latency — 2026-11-13

This change addresses the nine findings from the static review of
`9404018a6eaf49a3207315fa12ff98a7a8c0ae33`. It also removes unnecessary automatic
speaker matching and adds foreground processing timings. The existing model
priority, tool permissions, local wake authority and audio authorization boundary
remain in effect.

**Delivery status:** source changes and manual source/diff inspection only. No
tests, builds, typechecks, lint, application launch, model/API calls, migration,
flashing or CI were performed for this change. The measurements below are
previously recorded observations, not measurements of this patch.

## 1. The nine fixes

| Finding | Changed behavior | Main implementation |
| --- | --- | --- |
| Microphone can outlive a cancelled initialization | Opening the microphone is a separate state. Ending it cancels the pending start; any stream returned later is stopped. A failed finish also releases capture resources. | [capture.ts](../server/bot-server/src/capture.ts), [companion.ts](../server/bot-server/src/companion.ts) |
| Fresh setup writes to the obsolete data root | `configure_mac.py` defaults to `~/小呆 Server`; new local models use `models/local`. Existing model-directory settings remain intact unless explicitly overridden. An existing legacy installation or incomplete migration cannot be silently replaced by fresh setup. | [configure_mac.py](../server/bot-server/scripts/configure_mac.py), [setup guide](mac-bot-server.md) |
| Auto-start races with Stop/Reload | Application setup enters `BrainHost::auto_start`. All lifecycle operations use one mutex; an explicit lifecycle action suppresses a still-pending automatic start. The current runtime is resolved inside the serialized operation. | [BrainHost](../server/bot-server/src-tauri/src/brain/mod.rs), [application setup](../server/bot-server/src-tauri/src/lib.rs), [RPC](../server/bot-server/src-tauri/src/brain/rpc.rs) |
| Isolated XDG roots hide existing global OpenCode tools | The original global configuration directory is passed as `OPENCODE_CONFIG_DIR`, respecting an explicit override, the parent XDG config root, then the account home. Bot data/cache/state stay separate. A named JSON/JSONC overlay still works. | [harness](../server/bot-server/src-tauri/src/harness/mod.rs), [OpenCode configuration](mac-opencode.md) |
| Ordinary memory edits discard queued extraction work | Invalidating upstream conversation context no longer deletes the durable local conversation/job ledger. Schema v4 adds job-scoped content digests so existing jobs cannot restore the same deleted/replaced fact. Unrelated facts and future jobs remain eligible. Explicit forgetting and targeted profile deletion still remove their records. | [RPC](../server/bot-server/src-tauri/src/brain/rpc.rs), [store](../server/bot-server/src-tauri/src/memory/store.rs), [memory guide](mac-memory.md) |
| Migration rejects valid JSONC | The helper supports comments and trailing commas without stripping text inside strings. Inherited configuration is parsed before the destination is created; the original file remains untouched. Recovery instructions preserve an incomplete destination instead of forcing the app past its marker. | [migration helper](../server/bot-server/scripts/migrate_data_root.py), [setup guide](mac-bot-server.md) |
| A retained answer hides progress, approval or failure | The retained result has a visible current-status area, so an old answer can remain readable while a new request reports its state. | [Face renderer](../firmware/components/face/face.c), [main integration](../firmware/main/app_main.c) |
| Image transfer/decode failure is silent on the device | A local, token-bound failure indication accompanies the Mac delivery report while preserving the answer and image caption. Allocation, request, transfer and decode failures share this behavior. | [secure link](../firmware/components/brain_link/brain_secure.c), [runtime](../firmware/components/bot_runtime/bot_runtime.c) |
| Image receipt can precede any visible image pixels | A positive receipt requires an actual visible image-widget draw and its corresponding display refresh. Assigning an image source or refreshing unrelated screen content is insufficient. | [Face renderer](../firmware/components/face/face.c), [main integration](../firmware/main/app_main.c) |

The image receipt still uses the existing result/token lifetime. An image first
viewed after that lifetime may remain **display unconfirmed** on the Mac. This
change does not turn a stale receipt into authority over a newer turn. Device
failure reports may include a bounded diagnostic `code`; old clients can omit it.

## 2. What the existing latency records establish

The [November 12 real-board acceptance record](specs/server-voice-tasks/acceptance.md)
contains several different runs and measurement boundaries:

| Recorded run | Observation | Interpretation |
| --- | --- | --- |
| Arithmetic and follow-up, 21:13 | Approximately 7 seconds and 6 seconds from submission to text receipt | The delay occurs for simple questions too. |
| Arithmetic and follow-up, 21:22 | 10.1 and 8.0 seconds of Mac processing; zero TTS calls and zero reply PCM bytes | Speech synthesis is not the explanation for these silent-device delays. |
| Arithmetic and follow-up, 21:50–21:51 | 29.7 and 9.1 seconds of Mac processing; zero TTS calls and zero reply PCM bytes | First and subsequent turns can differ substantially. No stage breakdown was recorded, so the 29.7-second outlier cannot be attributed precisely. |
| Reading and drawing an observation chart | Examples range from about 23 seconds to around 56 seconds for completed displays | These include actual tool/file work and image preparation. They are not simple-question latency samples. |
| Separate provider/network trace on November 12 | Qwen waited 30.0 seconds before interruption; GLM then completed the tool task in 25.7 seconds; approximately 56 seconds to device receipts | This recorded long turn includes a measured preferred-route timeout before fallback. See [network trace](mac-opencode.md#模型连接与-mac-网卡). |

Do not combine these rows into a benchmark median: they use different versions,
inputs and timing boundaries. The earlier [November 8 observation](server-integration-2026-11-08.md)
of approximately 40 seconds to playback also included a different audio-output
path and was explicitly not a stable baseline.

The [local ASR record](mac-local-models.md) separately reports about 1.5 seconds
for the first RMA worker load/warm-up/transcription and about 0.1 seconds for a
subsequent transcription of one roughly three-second synthetic sample. That
does not establish the ASR duration for any of the device turns above.

## 3. Why a simple answer can take so long

### An unavailable preferred route can add a large wait

The [November 12 network record](mac-opencode.md#模型连接与-mac-网卡) reports
the 30.0-second Qwen wait above, a separate TLS handshake timeout, and tested
cloud connections routed through the Mac's Astrill WireGuard interface `utun6`.
That is evidence of a connection/fallback penalty in that run, not proof that
the VPN caused every slow response or that the 29.7-second arithmetic turn had
the same cause.

The generated provider settings currently allow 30 seconds for response headers.
After a recognized header timeout, [routing](../server/bot-server/src-tauri/src/harness/routing.rs)
backs the route off for five minutes; safe fallback also requires proof that no
output/tool action occurred. A failed preferred route can therefore cost time
again when its cooldown expires. Missing credentials, by contrast, are skipped
before making the provider request.

The app's `NO_PROXY` exceptions affect application proxy selection. They cannot
change macOS/VPN packet routing. Repairing the affected endpoint's connection
path on the Mac, or explicitly selecting a healthy route while investigating,
is the first practical action when this timeout appears. This patch preserves
the requested route priority and existing timeout/fallback policy.

### Serial work before the answer is delivered

The foreground path in [Brain::execute](../server/bot-server/src-tauri/src/brain/mod.rs)
is complete-utterance ASR, optional voice matching, memory lookup, an OpenCode
turn, optional image preparation, optional TTS, then result delivery. Before this
patch, every audio turn attempted speaker embedding even with an empty
enrollment store. The [worker](../server/bot-server/local-models/worker.py) loads
its ECAPA model lazily and requires at least three seconds for speaker samples.
That work cannot identify anybody when no voice has been enrolled, and short
samples are rejected anyway.

This patch skips automatic voice matching in those two cases, recording why.
Explicit enrollment and recognition still work through their existing APIs.
When enrolled speakers exist and the sample is long enough, matching is still
performed; recognition remains a candidate, never a profile authorization.
The saved milliseconds or seconds have not been measured.

### The device waits for a completed OpenCode answer

[Harness::attempt](../server/bot-server/src-tauri/src/harness/turn.rs) consumes
OpenCode's existing event stream, but a successful result is returned only after
`session.execution.succeeded` and fetching the final assistant message. The
host then validates the [structured scene](../server/bot-server/src-tauri/src/brain/scene.rs).
Streaming text events are not an early user-visible answer on the device.

When speech output is enabled in the Mac console, the current result also waits
for local TTS before returning its text to the UI. Requested image preparation
likewise precedes the device's final result. These can add latency in those modes;
the silent arithmetic runs above explicitly exclude TTS.

Even arithmetic therefore goes through session/context setup, the selected
provider, its reasoning/generation, and complete structured output. Tool tasks
can also involve multiple model/tool steps, retries, or human approval. The
[default GLM route](../server/bot-server/src-tauri/src/harness/config.rs) requests
thinking with low reasoning effort. That setting is a possible contributor,
not proof of the active private Mac configuration or of how many seconds it
consumed. This patch does not change provider priority or reasoning settings.

### The clock starts before Mac processing from the user's perspective

The device [AFE configuration](../firmware/components/bot_audio/bot_audio_afe.c)
uses a 600 ms minimum noise interval for speech-end detection. The
[secure audio worker](../firmware/components/brain_link/brain_secure.c) then
requires more than 200 ms after the observed utterance end and an empty queued
PCM buffer before committing. These are configured waits, not a measurement
of total endpointing time. Transport, scheduling and drain time also matter.

Consequently, Mac processing time is only part of **finished speaking to readable
answer**. The new Mac timings do not include device endpointing, upload, image
download or LCD drawing. Existing device receipts remain a separate observation.

### First-use and visibility effects

The [local model supervisor](../server/bot-server/src-tauri/src/brain/worker.rs)
starts its process lazily, and individual models also load lazily. First-use
loading or a restart after cancellation is a plausible contributor to first-turn
spikes; the old aggregate logs cannot prove it caused the 29.7-second run.

The background memory extractor is already a separate preemptible process. Its
ten-second polling interval is not a ten-second sleep in every foreground turn.
Resource contention is possible but unmeasured. Similarly, a configured timeout
is a maximum wait, not a fixed delay added to every question.

Finally, the retained-result bug hid progress and approval requests. Fixing it
improves feedback immediately at the source level; it does not make the cloud
model finish earlier.

## 4. New timing contract and where to read it

[timing.rs](../server/bot-server/src-tauri/src/brain/timing.rs) wraps the existing
operations with Rust's monotonic `Instant`. No inference library or telemetry
dependency was added. Successful `TurnOutput` and `device.turn.completed` payloads
include:

| Field | Meaning |
| --- | --- |
| `timings.total_ms` | Total time inside foreground `execute`, on the Mac. |
| `timings.stages_ms` | Measured durations for `memory_register`, `asr`, `speaker_identification`, `memory_enqueue`, `memory_context`, `harness`, `image_prepare`, `tts`, and `history`, when called. |
| `timings.skipped` | A reason for each skipped stage, such as `no_enrolled_speakers`, `audio_under_3_seconds`, `text_input`, or `output_disabled`. |
| `timings.input_audio_ms` | Input duration reported by the local ASR quality check, or `null` for text/no duration metadata. |

The stages cover the operation boundary, including a worker's lazy startup when
it happens in that operation. `harness` includes session preparation, provider
work, tools, fallback attempts and approval waits; it is not pure model compute
time. `image_prepare` may be near zero when the validated visual requires no
image. Totals can exceed the sum of stages because small orchestration and
validation work is outside those spans.

The existing `processing_ms` device metric remains, with its broader gateway
task boundary. In the parent console, both direct replies and device results
provide **查看 Mac 分段耗时**. The device task detail also appends completed-stage
durations as they arrive, including failed or interrupted stages.

Events `turn.stage_started`, `turn.stage_completed`, and `turn.stage_skipped`
carry the turn ID, stage, duration/status or skip reason. A dropped wait emits
`interrupted`; this describes the Rust wait ending, not a guarantee that every
already-submitted blocking read has physically stopped. `turn.completed` /
`turn.failed` include the aggregate elapsed duration; successful completion also
includes the timing summary. Timing fields contain no audio, face image,
transcript, embeddings or provider secrets.

The harness also adds `prepare_ms` to `harness_route_selected` and `elapsed_ms`
to route failure/success events. Route duration includes setup and, on failure,
the interruption/no-output checks needed before fallback. `harness_first_text`
records time from opening the attempt's event stream to its first nonempty model
text delta. It is neither HTTP time-to-first-byte nor the time to a validated
answer; a tool-using turn can continue afterward. Route selection, fallback,
first-text and completion timings appear in device task details and the direct
conversation activity log. Raw deltas do not become device speech or scene data.

## 5. Next performance decisions, based on future measurements

No new benchmark is claimed in this delivery. On the next authorized Mac/device
run, compare a first turn and several subsequent identical simple questions,
using the same provider and recording whether output speech is enabled. Read the
stage timings alongside the existing route, tool, retry and approval events.
Keep device elapsed durations separate from Mac durations; do not subtract clocks
from different machines.

If `harness` dominates, the next substantial changes are an explicit short-answer
mode within OpenCode and a protocol for delivering validated answer text before
optional pictures or spoken audio. That requires preserving permission/tool
completion semantics and avoiding partial JSON or unconfirmed action claims on
screen. If ASR or voice matching dominates, address that stage's model lifecycle
and scheduling. If Mac processing is short but the display is late, investigate
endpointing, transfer and the device's actual draw receipts. Those decisions
should use the new measurements rather than guessing from one overall number.

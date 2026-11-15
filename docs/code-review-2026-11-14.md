# Code review: firmware and Mac server — 2026-11-14

A static review of the ESP32-S31 firmware (`firmware/main`, `firmware/components`)
and the Mac side (`server/bot-server`, legacy `server/espbot`, `preview/`, `tools/`,
`shared/`), with a plan for improving them.

**Status of this review:** source reading only. Nothing was built, flashed, run or
tested, no model or API was called, and no source file was changed. Findings that
depend on timing or stack depth on the real board are marked as needing a
measurement.

## 1. Coverage and confidence

| Area | How it was read | Confidence |
| --- | --- | --- |
| Device ↔ Mac link: `brain_link`, `brain_secure.c`, `peer_path.c`, Mac `gateway.rs`, `plain_audio.rs`, `media.rs`, `credentials.rs`, `discovery.rs`, `pairing.rs`, `lan/` | Line by line, both ends compared | High |
| Firmware audio, runtime, bus, voice, responses, `app_main.c` | Line by line; the top findings were re-checked against source | High for re-checked items, medium for the rest |
| Firmware face, vision, attention, platform, debug, LAN v1, build configuration | Line by line; the top findings were re-checked | Same |
| Console frontend, `usb_debug.rs`, legacy Python server, preview, tools, shared contracts | Line by line; the top finding was re-checked | Same |
| Server core: `brain/mod.rs`, `rpc.rs`, `worker.rs`, `memory_worker.rs`, `harness/routing.rs`, `harness/secrets.rs`, the start/permission/cancel parts of `harness/mod.rs`, `memory_extract.py`, the main loop of `worker.py` | Read directly | High for what is listed |
| `harness/turn.rs`, `harness/config.rs`, `memory/store.rs`, `memory/identity.rs`, `brain/scene.rs`, `brain/display.rs`, most of `worker.py`, the setup and migration scripts | Searched for specific risks only (panics, SQL construction, transactions, subprocess use, hash checks) | **Low — not reviewed in depth** |

The last row matters: the absence of findings there is not evidence of quality.
Those files hold the fallback logic, the memory schema and the migration tools,
and deserve a dedicated pass.

In the tables below, **Checked** means I confirmed the cited code myself.
**Single read** means one careful reading reported it with line references and
I did not re-verify it. **Measure** means the mechanism is in the source but
the effect depends on board timing or stack use.

## 2. Overall assessment

The code is careful where it matters most. The session token discipline, the
privacy gate on microphone audio, the whole-phrase wake rule, the default-deny
tool permissions and the handling of secrets are all implemented consistently,
and I found no path that uploads pre-wake audio or relabels a turn.

The weaknesses are of three kinds:

1. **A few real defects** where one part's assumption does not hold for another:
   a console action that cancels the robot's conversation, state events that can
   be dropped for good, and a diagnostic link that parses untrusted input on a
   small stack.
2. **Operability.** A fault usually ends in "restart the device" or an opaque
   error. The server discards the diagnostics its own child processes write, and
   the firmware has no recovery path for a latched fault, no crash dump and
   almost no free space in the app partition.
3. **Accumulated weight.** Several very large single files, a legacy server and
   a diagnostic protocol that no longer do anything useful, and documents that
   contradict each other on what is finished.

## 3. Priority list

| # | Severity | Area | Finding | Status |
| --- | --- | --- | --- | --- |
| 1 | High | Console → device | Routine console actions cancel an active device conversation | Checked |
| 2 | High | Firmware runtime | State events older than 2 s are dropped and never re-sent | Checked; boot timing needs a measurement |
| 3 | High | Firmware LAN v1 | Deeply nested JSON from any LAN host is parsed on a 4 KB stack | Checked; overflow itself needs a measurement |
| 4 | Medium | Firmware runtime | A latched fault has no recovery other than a reboot | Single read |
| 5 | Medium | Firmware audio | Voice-activity end is not reported when permission is withdrawn | Single read |
| 6 | Medium | Firmware audio | Speech resuming just after submit cancels the submitted turn | Single read |
| 7 | Medium | Firmware | `display_lock(0)` waits forever on the runtime task; flash writes run there too | Checked |
| 8 | Medium | Server | Child-process diagnostics are discarded; there is no log file | Checked |
| 9 | Medium | Server routing | Provider 5xx and connection failures never fall back to the second route | Checked |
| 10 | Medium | Console | Permission cards are rebuilt every 3 s; an approval can land on a different request | Single read |
| 11 | Medium | Firmware build | App partition about 96% full; no OTA, rollback or crash dump | Checked |
| 12 | Medium | Firmware build | Dependency lock file is ignored by git; version ranges float | Checked |
| 13 | Medium | Privacy | Per-turn audio crosses the LAN unencrypted (a recorded user choice) | Checked |
| 14 | Medium | Repository | Dead legacy server, dead firmware client, functionless LAN v1, contradictory documents | Checked |

## 4. Findings: device ↔ Mac link

### 4.1 Routine console actions cancel the robot's conversation (High, checked)

- `stopEverything()` in [companion.ts](../server/bot-server/src/companion.ts)
  sends `assistant.cancel` with an empty payload when the console has no turn of
  its own.
- [rpc.rs](../server/bot-server/src-tauri/src/brain/rpc.rs) passes that to
  `brain.cancel(None)`, which in
  [brain/mod.rs](../server/bot-server/src-tauri/src/brain/mod.rs) broadcasts
  "cancel everything" and also kills any local inference in flight.
- The gateway loop in
  [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs) treats that
  as cancel-all: it aborts the device turn, drops playback, and clears the
  session, sending `session.cancelled`.

The same function is called when the parent changes the profile dropdown,
starts a new conversation, saves any settings form, or closes the window. A
child talking to the robot loses the turn with no explanation on either side.
Several memory operations also call `cancel(None)` on the server side; that is
deliberate, but the device still gets no reason.

**Fix:** give cancellation a scope (console turn, device session, everything).
Send a `turn_id` when the console has one and skip the call otherwise. Keep
"everything" for the explicit Stop button and for memory mutations, and send the
device a reason code it can show.

### 4.2 Per-turn audio is unencrypted on the LAN (Medium, checked, recorded choice)

Microphone PCM and reply PCM travel over a separate TCP connection with an
HMAC per frame and no encryption
([brain_secure.c](../firmware/components/brain_link/brain_secure.c),
[plain_audio.rs](../server/bot-server/src-tauri/src/brain/plain_audio.rs)).
[mac-gateway.md](mac-gateway.md) and [mac-brain-v3.md](../shared/mac-brain-v3.md)
record this as the user's choice, so it is not a defect. It is still the
largest privacy exposure in the system: anyone who can capture traffic on the
Wi-Fi network can hear a child's speech.

The integrity design is sound: a fresh 32-byte key per turn delivered over the
authenticated WSS connection, a direction byte, stream and sequence checks, and
a constant-time comparison on the device.

**Improvement:** the per-turn key already exists, so switching the frame from
HMAC-SHA256 to an AEAD (AES-GCM or ChaCha20-Poly1305, with the sequence number
as nonce) adds confidentiality without bringing back the TLS memory cost that
presumably motivated the plain transport. Negotiate it as `aead_tcp_v1` beside
`plain_tcp_v1` so old firmware keeps working.

### 4.3 LAN v1 is a liability with no remaining function (Medium, checked)

- The firmware answers every LAN v1 request with `AUTH_REQUIRED`
  ([brain_link.c](../firmware/components/brain_link/brain_link.c)), so the
  server's `face.set` command hub
  ([lan/cmd.rs](../server/bot-server/src-tauri/src/lan/cmd.rs)) can never
  succeed.
- The firmware has to pause this link before every conversation because it
  uses internal RAM the audio path needs (`secure_task` in
  [brain_secure.c](../firmware/components/brain_link/brain_secure.c)).
- The server side listens on `0.0.0.0` with no connection limit
  ([lan/mod.rs](../server/bot-server/src-tauri/src/lan/mod.rs)). Device IDs are
  self-claimed, and every new ID is written to `devices.json` with no cap
  ([pairing.rs](../server/bot-server/src-tauri/src/pairing.rs)), so any LAN host
  can grow that file and flood the console with pairing events. A second
  connection claiming an existing ID replaces the real one.
- The console has no approve/deny control for pending devices, although
  [lan-discovery-v1.md](../shared/lan-discovery-v1.md) describes one.
- It is also the entry point for finding 5.1.

**Fix:** move the two useful things (online status and device diagnostics) onto
the authenticated v3 connection, then remove LAN v1 from both sides. Until
then, cap the pairing store (for example 32 records), cap connections, and
compile the firmware side out by default.

### 4.4 Smaller link items

| Item | Detail | Status |
| --- | --- | --- |
| Reply images and transcripts in UI events | `device.turn.completed` carries the transcript and the full image as base64 to the webview. Acceptable for a local UI; make sure it is never written to a log when logging is added (8.1). | Checked |
| Gateway certificate lifetime | The leaf is valid 365 days and the script refuses to overwrite it. The device does not check dates (`CONFIG_MBEDTLS_HAVE_TIME_DATE` is off), so nothing breaks today, but there is no documented renewal procedure. | Checked |
| Credentials at rest on the device | The bearer token, CA and Wi-Fi password are plain NVS blobs. The code comments say so honestly. See 6.3. | Checked |
| Two implementations of one protocol | Method names, error codes and limits are string literals repeated in C and Rust. Nothing checks that they agree. See 9.3. | Checked |
| `serve_peer` | One 400-line `select!` loop holding all per-connection state as local variables. Hard to test and to change safely. See 9.1. | Checked |

## 5. Findings: firmware

### 5.1 Nested JSON on a 4 KB stack (High, checked; overflow needs a measurement)

[bot_lan.c](../firmware/components/lan_link/bot_lan.c) gives the WebSocket
client task a 4096-byte stack, places a 513-byte buffer on it, and parses the
frame there with cJSON, whose nesting limit is 1000. A 512-byte frame of `[[[[…`
recurses about 500 levels. The parse happens before the `welcomed` check, and
the device connects to any host advertising the diagnostic service. If the
stack overflows the device reboots, rediscovers, and can be hit again.

**Fix:** reject frames with bracket depth above about 8 before parsing, or copy
the bounded frame to the worker task (8 KB stack) and parse there. Removing LAN
v1 (4.3) removes the problem.

### 5.2 State events dropped by the age filter (High, checked; boot timing needs a measurement)

`event_order_valid` in
[bot_runtime.c](../firmware/components/bot_runtime/bot_runtime.c) rejects any
event older than `event_max_age_ms` (2000). That is right for edges such as a
wake detection. It is wrong for events that carry state and are posted only on
change: audio health, wake status, link state, camera health.

`bot_audio` posts health and wake status once, shortly after it starts. The
runtime task is created later in
[app_main.c](../firmware/main/app_main.c), after Wi-Fi, the secure link, debug
and vision start. If that takes more than two seconds the runtime never learns
that audio is available, and rejects every wake until reboot. Any later stall
of the runtime task over two seconds can lose a state change the same way.

**To confirm:** a boot log whose first runtime status line shows audio
unavailable.

**Fix:** exempt state-carrying kinds from the age check, or have the runtime
task re-read `bot_audio_get_status()` on its first step and whenever its
snapshot disagrees. Starting the runtime task before the producers also helps.

### 5.3 No recovery from a latched fault (Medium, single read)

`bot_runtime_clear_fault` and `bot_audio_retry_afe` have no callers. Almost
every event kind is "critical", every producer posts with zero wait, and one
overflow of the 24-slot critical queue latches a fault that blocks wake and
touch until someone power-cycles the device. [local-runtime.md](local-runtime.md)
lists this as a known limitation.

**Fix:** classify UI and progress events as non-critical. Let the runtime clear
the fault once the queues are drained and a touch or Stop arrives, or restart
automatically after a quiet period. Retry the audio front end on a slow timer.

### 5.4 Voice-activity end is not reported when permission is withdrawn (Medium, single read)

[bot_audio.c](../firmware/components/bot_audio/bot_audio.c) clears its speech
flag silently when voice-activity permission drops (playback begins, the audio
front end fails, a codec read fails). The runtime's `utterance_active` then
stays true: it rejects playback-started, thinking and reply commands, and the
transport gives up with "Speech capture timed out" after 29 s.

**Fix:** emit the end event with the original token whenever speech was active
and permission drops.

### 5.5 Speech resuming just after submit cancels the turn (Medium, single read)

The transport submits 200 ms after the utterance ends, but the runtime stays in
listening. If the user resumes speaking or coughs, a new speech start is
accepted, the server's "thinking" command is rejected, the start buffer
overflows about a second later, the runtime arms a follow-up with a new token,
and the reply to the submitted question is discarded as stale.

**Fix:** add an explicit "submitted" state that closes capture and
voice-activity for that token.

### 5.6 Blocking work on the runtime task (Medium, checked)

- `set_voice_theme` in [app_main.c](../firmware/main/app_main.c) calls
  `bot_platform_display_lock(0)` as if it were a try-lock. In the LVGL port a
  timeout of 0 means wait forever, so the runtime task can block behind a whole
  render. The same misuse is in
  [bot_platform.c](../firmware/components/bot_platform/bot_platform.c), where
  the timeout branch can never run.
- Voice settings, theme and camera orientation are written to flash from the
  same task (single read).

Both stall the task that services Stop and the event queues, and are the likely
feeders for 5.2 and 5.3.

**Fix:** use a short finite timeout (5 ms, as `reconcile` already does). Move
persistence to a low-priority worker with a one-slot overwrite queue.

### 5.7 Gaze sign and mirroring (Medium, single read; needs a board check)

[attention.c](../firmware/components/attention/attention.c) maps positive yaw
to positive screen x, which is the wrong direction for a screen facing the
viewer. It is masked by flipping the camera sensor in `app_main.c`. The result
looks right on screen, but the tracker receives a mirrored image, the left and
right eye labels are swapped, there are two independent mirror switches, and a
future pan/tilt adapter using the head yaw target would turn away from the
person.

**Fix:** keep the sensor unmirrored and put the camera-to-screen sign in one
documented transform. Check on the board before changing defaults.

### 5.8 Vision timing constants leave no margin (Medium, single read; needs a measurement)

The frame interval is 200 ms, the tracker resets confirmation above a 330 ms
gap, and the attention manager treats a snapshot as current for 350 ms from a
timestamp taken before the frame copy and inference. If capture-to-publish
latency exceeds about 150 ms, focus flickers between tracking and lost on every
frame; above 330 ms no face is ever confirmed and nothing reports why.

**Fix:** derive the gap and maximum age from the frame interval plus measured
latency, and validate the relation at start.

### 5.9 Lower-severity firmware items

| Item | Detail | Status |
| --- | --- | --- |
| Capture task does slow work | The command graph is rebuilt on every session open and close, and float logging runs on the capture thread. The receive buffer holds 90 ms. Check `processing_max_us` and overruns on the board. | Single read, measure |
| Debug log hook loses logs | With USB preview enabled, logs go through a 32-slot queue and are lost on bursts and at a panic. | Single read |
| Full-screen redraw | The EVE renderer invalidates the whole 800×480 screen on every gaze step. | Single read, measure |
| Rejected local command leaves the mic closed | Audio latches "command consumed"; if the runtime rejects the event the token is not replaced for up to 60 s. | Single read |
| Stale mute after a failed controls apply | Mute and keyword threshold are applied together; a threshold failure keeps the old mute. | Single read |
| Start buffer is PSRAM-only | Allocation failure aborts all audio, including wake, although the code can run without the buffer. | Single read |
| Local hour is UTC when the night schedule is off | The time zone is set only under that option; reply scenes then use UTC hours. | Single read |
| Kconfig options with no effect | The MultiNet threshold and output volume options are overridden by voice settings at start. | Single read |
| Stack margins unmeasured | `wake_reply` 4096, `bot_audio_out` 6144, vision worker 8192, each with large locals. Log high-water marks. | Single read, measure |
| Dead code | An unreachable `LOCAL_STOP` case, a never-produced `BORED` state, unused `feed_lock`, unused `pill` renderer, unused pupil candidate, OV3660 naming on an SC101IOT board. | Single read |

## 6. Findings: firmware build and configuration

### 6.1 App partition nearly full; no update or crash path (Medium, checked)

[partitions.csv](../firmware/partitions.csv) gives the app 0x800000 bytes. The
built image in `firmware/build` is 8,054,368 bytes, about 96% (that build is
dated 12 November and may be behind the source). The 1.94 MB `storage`
partition is not referenced anywhere. There are no OTA slots, no rollback, and
core dumps are disabled, so a field crash leaves nothing to inspect.

**Fix:** move the fonts and the 104 response clips out of the app image into a
data partition, and turn part of `storage` into a core-dump partition. Any
layout change that moves `model` needs a deliberate model reflash, so do it
once.

### 6.2 Dependencies are not reproducible (Medium, checked)

`.gitignore` excludes `firmware/dependencies.lock`, and the component manifests
use caret ranges. A fresh checkout can resolve different LVGL, mDNS and
WebSocket versions from the ones that were tested. The face component also
includes a private LVGL header, which is the kind of thing a minor version
breaks (single read).

**Fix:** commit the lock file.

### 6.3 No hardware protection of stored secrets (Medium, checked)

Secure boot, flash encryption and NVS encryption are all off, which
[sdkconfig.defaults](../firmware/sdkconfig.defaults) states plainly. Anyone
with USB access can read the Wi-Fi password and the gateway token, and the USB
console accepts new provisioning without any confirmation. The Kconfig Wi-Fi
password is also compiled into the image. Acceptable on a development board;
it should be an explicit gate before the enclosed device leaves the lab.

**Fix:** provision Wi-Fi only through NVS and leave the Kconfig password empty.
Before any non-lab use, enable flash and NVS encryption and build with the USB
debug options off.

## 7. Findings: console frontend and USB debug

All single read unless marked.

| # | Severity | Finding | Fix |
| --- | --- | --- | --- |
| 7.1 | Medium | Permission cards are rebuilt every 3 s in list order. If an earlier request expires, a different one moves under the pointer, and the confirmation shows only the tool name. | Re-render only on change, keyed by request ID (the questions list already does this). Show the target in the confirmation. |
| 7.2 | Medium | `refreshStatus` returns immediately if a refresh is in flight, so after Start or Stop the UI can show the old state for up to 15 s. | Queue one re-run, or use a sequence number. |
| 7.3 | Medium | Any error containing "Permission denied" is shown as a microphone or camera permission problem, including file errors from the backend. | Map only at the capture call sites, by exception type. |
| 7.4 | Medium | Enrolment and recognition results are dropped if the user switches panel during the operation; the backend has stored the enrolment but the UI never says so. | Use a per-operation token rather than the capture generation. |
| 7.5 | Medium | Two edited settings forms block each other; the only way out discards both. | Drop the cross-form guard; each save already patches only its own keys. |
| 7.6 | Medium | The USB debug poll serialises the whole snapshot (two base64 frames, 300 events, crash log) every 750 ms on the main thread while holding the lock the serial reader needs. | Make the command async and return deltas; poll slowly when hidden. |
| 7.7 | Medium | USB debug notices are overwritten by the next poll, so an export path is visible for under a second. | Keep action results on a separate line. |
| 7.8 | Low | Hiding the window discards collected enrolment samples and the audio draft. | On hide, stop capture and playback only. |
| 7.9 | Low | The production CSP still allows the dev-server origin; the capability set is wider than the one event listener needs. | Move dev origins to the dev CSP; narrow the capability. |
| 7.10 | Low | IPC types are hand-copied from the Rust structs and cast without checks. | Generate them from Rust. |
| 7.11 | Low | Accessibility: focus is lost on every re-render and after each save, errors are announced politely, some text is 9 px. | Restore focus; use `role="alert"` for errors. |

No HTML injection path was found: model text, memory text, device names and
errors all go through `textContent`, and web cards use sandboxed frames.

## 8. Findings: server core

### 8.1 Diagnostics are discarded (Medium, checked)

- The OpenCode child, the model worker and the memory extractor are all
  spawned with `stderr` set to null
  ([harness/mod.rs](../server/bot-server/src-tauri/src/harness/mod.rs),
  [worker.rs](../server/bot-server/src-tauri/src/brain/worker.rs),
  [memory_worker.rs](../server/bot-server/src-tauri/src/brain/memory_worker.rs)).
- [worker.py](../server/bot-server/local-models/worker.py) goes to some trouble
  to write privacy-safe diagnostic lines (operation, result code, elapsed time,
  exception type only). They go nowhere.
- The Rust side has no logging framework. Events reach the webview, and
  `eprintln!` prints the event kind to a terminal that a packaged app does not
  have.

So "local inference failed" or "process exited during startup" cannot be
diagnosed after the fact, and the latency questions in
[review-fixes-2026-11-13.md](review-fixes-2026-11-13.md) have to be answered
from memory of what the UI showed.

**Fix:** add a small rotating log under the data root (owner-only
permissions). Record event kinds, error codes and stage timings, never
transcripts, images or keys. Pipe each child's `stderr` into it with a line
length limit. This is the cheapest change with the largest effect on every
future investigation.

### 8.2 No fallback on provider 5xx or connection failure (Medium, checked)

In [routing.rs](../server/bot-server/src-tauri/src/harness/routing.rs),
`permits_route_change` allows a second route only for quota, rate limit
(optional), header timeout, authentication and configuration failures. A 500,
502 or 503 from the first provider, or a failed connection, is classified as
upstream or transport: no fallback and no cool-down. During a provider outage
every turn fails on the first route even though the second is healthy, and
keeps trying it.

The "never replay a turn that used tools or produced output" rule is separate
and should stay. A failure before any output or tool call is safe to retry
elsewhere.

**Fix:** treat 5xx and connect failures before first output as eligible, with a
short cool-down (30–60 s). Classification also depends on English substrings in
provider messages; prefer status and code, and keep the substrings as a last
resort.

### 8.3 Latency is structural (Medium, checked)

`Brain::execute` in
[brain/mod.rs](../server/bot-server/src-tauri/src/brain/mod.rs) runs strictly in
series: transcription, optional speaker matching, the full model reply, image
preparation, then synthesis of the whole reply, and only then is anything sent
to the device. The reply audio is delivered as one buffer of up to 120 s. The
measurements in [review-fixes-2026-11-13.md](review-fixes-2026-11-13.md) (7–30 s
for simple questions) follow from this shape as much as from provider speed.

**Improvement:** send the text to the device as soon as the model finishes, and
synthesise and stream speech sentence by sentence while the device is already
showing the text. Run speaker matching concurrently with the model call, since
its result does not feed the prompt. The stage timings already collected give
the baseline.

### 8.4 Smaller server items

| Item | Detail | Status |
| --- | --- | --- |
| Memory extraction cost | A new Python process loads the 4B extraction model for every job. A foreground turn pre-empts it correctly, but a backlog reloads the model each time. Process several queued jobs per launch. | Checked |
| Cancel kills the model worker | Any cancel bumps the worker generation and kills an in-flight inference process, so the next call pays the model load again. With 4.1 fixed this becomes rare. | Checked |
| Synchronous SQLite inside async code | Several short memory writes run on the async runtime while the turn gate is held. This is deliberate, to avoid a detached writer after "forget", and the writes are small, but `synchronous = FULL` makes each a disk sync. Acceptable; note it. | Checked |
| API keys in the child environment | Keys go from the Keychain into the OpenCode child's environment, where an approved shell tool could read them. The ask-by-default permission is the control. Worth stating in [mac-opencode.md](mac-opencode.md). | Checked |
| Gateway default bind | `0.0.0.0:7443`, limited to private address ranges and 4 connections. Reasonable. | Checked |

## 9. Structure and maintainability

### 9.1 Oversized units

| File | Size | Suggested split |
| --- | --- | --- |
| [brain_secure.c](../firmware/components/brain_link/brain_secure.c) | 1,422 lines, 13 phases, very dense lines | Provisioning; WSS message parsing; plain-audio transport; image download; the phase machine as a table |
| [bot_audio.c](../firmware/components/bot_audio/bot_audio.c) | 1,554 lines, about 60 fields in one struct | Gate (session, fences); capture; detection; playback — each owning its own sub-struct |
| [bot_runtime.c](../firmware/components/bot_runtime/bot_runtime.c) | 1,348 lines | One `reset_turn_state` used by the four transitions; per-domain event handlers; separate state events from edge events |
| [app_main.c](../firmware/main/app_main.c) | 1,090 lines | `reconcile` into camera, display, face, receipts, diagnostics; USB console and audio-event mapping into their own files |
| [gateway.rs](../server/bot-server/src-tauri/src/brain/gateway.rs) | `serve_peer` about 400 lines | A `PeerSession` struct with one method per protocol method |
| [companion.ts](../server/bot-server/src/companion.ts) | 1,419 lines, one closure, about 45 mutable variables | A typed request layer; a state store; one module per panel; an event reducer |

The state machines are the part of the system that most needs tests, and their
current shape is what makes them hard to test. `brain_secure.c` in particular
polls shared state every 10–30 ms rather than reacting to events, so each new
feature adds another flag to the loop.

### 9.2 Remove what no longer does anything

- [server/espbot](../server/espbot) and its test: the only client is
  [brain_client.c](../firmware/main/brain_client.c), which is not in the build
  and refers to Kconfig options that do not exist. The Python server also
  accepts unauthenticated requests from any web page when run on loopback.
- [protocol.md](../shared/protocol.md), which documents that legacy HTTP
  polling.
- `usb-debug.html`, which is built but has no window or capability.
- LAN v1 on both sides, once its status reporting is moved (4.3).

### 9.3 One protocol, two hand-written implementations

The v3 contract exists as prose in [mac-brain-v3.md](../shared/mac-brain-v3.md)
and as string literals in C and Rust. The two ends agree today. Add a small
set of recorded message vectors (JSON files in `shared/`) that the Rust tests
replay and the existing host-side firmware tests parse, so a change to one end
fails a local test on the other. This is a local check, not a CI job.

### 9.4 Documents contradict each other

- [README.md](../README.md), the bot-server README and
  [architecture.md](architecture.md) say the firmware WSS client is still to
  come; [mac-bot-server.md](mac-bot-server.md) and the acceptance record say it
  is working on a real board.
- The server README describes menuconfig options that do not exist.
- Palette and expression counts differ between documents and data.
- [mac-console.md](mac-console.md) describes playback stopping on page change
  and permission polling only during turns; the code does neither.

**Fix:** keep one dated status file and link to it from the others instead of
repeating status in each.

## 10. What is done well

- **Token discipline.** Epoch, session and turn only move forward; a follow-up
  token is allocated before any next-turn audio; both ends check the full token
  before accepting content.
- **Microphone privacy gate.** PCM leaves through one function that rechecks
  session, token and readiness under the lock; new tokens discard the receive
  backlog; buffers are wiped.
- **Whole-phrase wake.** The passive graph holds exactly one phrase and is
  verified after each update.
- **Stop path.** A capacity-independent latch checked between every event.
- **Gateway authentication.** Pinned CA, per-device random token stored only
  as a hash, constant-time comparison, revocation that closes live connections,
  browser origins refused, every write under a deadline.
- **Tool permissions.** Default deny, ask for everything else, approvals bound
  to the live turn and never persisted.
- **Secrets.** Keychain storage, zeroised buffers, error strings that never
  include upstream text or keys, a worker that never prints exception text.
- **Child processes.** Own process groups, killed on drop, startup reclaimed
  after an interrupted start.
- **Camera frame lifetime** and the refusal to invent gaze, eye contact or
  head pose that the hardware cannot provide.
- **Generated face data.** One generator from `shared/*.json` to all five
  targets, with a check mode.

## 11. Improvement plan

Ordered so that each step makes the next one safer. None of it needs CI.

### Step 1 — correct the defects (small, independent changes)

1. Scope console cancellation (4.1).
2. Exempt state events from the age filter, or re-read status in the runtime
   task (5.2).
3. Bound JSON depth in LAN v1, or compile LAN v1 out (5.1).
4. Replace `display_lock(0)` with a finite timeout (5.6).
5. Emit voice-activity end when permission drops; add a "submitted" state
   (5.4, 5.5).
6. Stop rebuilding permission cards on every poll (7.1).

### Step 2 — make failures visible and recoverable

1. Server log file with child `stderr` (8.1).
2. Firmware: wire fault recovery, enable a core-dump partition, log stack
   high-water marks once per status interval (5.3, 6.1, 5.9).
3. Commit `dependencies.lock` (6.2).
4. Measure on the board, using the diagnostics that already exist: boot-to-
   runtime time, capture `processing_max_us`, vision latency, display refresh
   time. Several findings above are marked "measure" and this resolves them.

### Step 3 — remove weight

1. Delete the legacy Python server, `brain_client.c`, `protocol.md` and the
   unused USB debug page (9.2).
2. Move device status to v3 and remove LAN v1 (4.3).
3. Free app-partition space by moving fonts and clips to a data partition (6.1).
4. Reconcile the documents around one status file (9.4).

### Step 4 — restructure the state machines, with tests

1. Split `bot_runtime.c` event handling and add host tests for the sequences in
   5.2–5.5. The existing host test layout already supports this.
2. Split `brain_secure.c`; make the phase machine a table that a host test can
   drive with recorded server messages (9.3).
3. Extract `PeerSession` from `serve_peer` and test each method.
4. Split `companion.ts`; generate IPC types from Rust.

### Step 5 — product improvements

1. Encrypt per-turn audio with the existing per-turn key (4.2).
2. Fall back on provider 5xx and connection failures before first output (8.2).
3. Deliver text first and stream speech by sentence (8.3).
4. Fix the gaze transform before any pan/tilt hardware is attached (5.7).
5. Before the enclosed device leaves the lab: flash and NVS encryption, USB
   debug off, no compiled-in Wi-Fi password (6.3).

### Still to review

A dedicated pass over `harness/turn.rs`, `harness/config.rs`,
`memory/store.rs`, `memory/identity.rs`, `brain/scene.rs`, `brain/display.rs`,
the rest of `worker.py`, and the setup and migration scripts (see section 1).

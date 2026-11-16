# Integration contract (API defined before main wiring)

## Household additions, 2026-11-16

See [household firmware integration](../../HOUSEHOLD_INTEGRATION.md) for settings
page leases, playback purposes, bounded presentation receipts and the optional
real ES8389 MRMN reference/AEC path. The following November 13 notes describe
the legacy single-reply path, which remains compatible.


## Voice additions, 2026-11-13

Load persisted `bot_voice_settings_t` into `config.voice_settings` before runtime creation. UI submits `BOT_EVENT_VOICE_SETTINGS` or `BOT_EVENT_SOUND_PREVIEW`; it does not call playback directly. `play_wake_response` and `play_sound_preview` receive an explicit relative PCM gain. A preview token has a nonzero epoch and zero session/turn, uses the PREVIEW audio source, and grants no capture permission. Closing settings or Stop cancels it.

Map AUDIO `CAPTURE_READY.available` and LINK `CAPTURE_READY.data.capture.ready` independently to the bus, preserving the current capture token. `capture_ready` and `transport_ready` are different observed facts. Pass `retain_start`, `waiting_for_followup`, `local_commands`, and separately authenticated `deliver_pcm` through `bot_audio_set_session_ex`; preserve the same token when only transport readiness changes. The normal WAITING phase already owns its next-turn token and has `followup_armed=true`; VAD START consumes it without another token change. Never transfer cached PCM between tokens.

Map `LOCAL_COMMAND` with its original token and native score. The audio owner immediately consumes that capture lease; runtime invalidates the remote turn and applies only its fixed known action. Keep passive wake separate. Map PREVIEW and LOCAL_RESPONSE playback source flags explicitly. Match real `PLAYBACK_PROGRESS` by token/id/source before extending progress; no network PCM event may impersonate it.

`BOT_COMMAND_REPLY_PRESENT` stores text while a spoken reply is pending. Hardware completion, in either order with PRESENT, arms the next turn. Text-only `REPLY_DONE` arms it immediately. Keep `reading_token` and image/display receipts independent during prepared follow-up; do not clear the old result merely because the capture token changed. IMAGE_FAILED may target this retained result lease while follow-up is armed.

These changes have targeted host runtime checks, not S31 IDF or physical-device acceptance. The current application wiring is in `main/app_main.c` and the user test guide is [voice-interaction.md](../../../docs/voice-interaction.md).

Create `bot_bus_t` via `bot_bus_create()`, then `bot_runtime_create(bus, config, ops, ctx, now_ms, &runtime)`.
Call `bot_runtime_step(runtime, now_ms)` from a single owner task every ~20 ms; use local `esp_timer_get_time()/1000` low 32 bits.

The public headers are now written in `bot_bus/include/bot_bus.h`, `attention_manager/include/attention_manager.h`, and `bot_runtime/include/bot_runtime.h`.

`ops.apply_state(ctx, snapshot)` receives requests and permissions. Main reconciles `display_awake_requested`, `camera_requested`, `audio_session_requested`, `audio_uplink_permitted`, expression/caption and focus. Always-on capture/wake stays in audio; audio_session_requested toggles only post-wake speech delivery. `ops.stop_output` performs immediate local playback stop before optional `ops.cancel_remote` is called. Runtime has no LVGL or BSP dependencies.

## Monotonic clock integration

**Wire `ops.now_ms` whenever producers run asynchronously.** A timestamp captured at the start of the owner loop can already be older than an event enqueued while that same step drains its queues. The optional callback reads the same local monotonic millisecond clock as the producers; the runtime samples it after each event/command dequeue and signal capture, after copying the latest vision snapshot, and before final timeout decisions. It never advances its clock from an event's timestamp or grants a future-time allowance: observations later than the refreshed clock still fail freshness checks.

```c
static uint32_t runtime_now_ms(void *context)
{
    (void)context;
    return bot_platform_now_ms();
}

/* Add to the existing bot_runtime_ops_t initializer: */
/* .now_ms = runtime_now_ms, */
```

This callback runs on the runtime owner, must return immediately, and must not reenter runtime APIs. When `ops.now_ms == NULL`, `step(runtime, now_ms)` retains the explicit supplied timestamp; that mode requires synchronously supplied input whose timestamps do not exceed `now_ms`. It is not the mode for live audio/camera/UI producers. `clear_fault` also refreshes time after collecting pending stop/fault signals when the callback is present.

After `bot_runtime_step()`, the main owner loop should pass a fresh `bot_platform_now_ms()` to its downstream `reconcile()`/gaze processing as well. Reusing the loop's earlier time could otherwise make a newly accepted focus appear to come from the future downstream.

## Input and output ownership

Audio posts `BOT_EVENT_WAKE_STATUS` and `BOT_EVENT_AUDIO_HEALTH`; wake needs `BOT_EVENT_WAKE_DETECTED`, source AUDIO, full `BOT_WAKE_PHRASE_XIAODAI_XIAODAI`, current monotonic observed_ms and explicit confidence_available. AFE has no calibrated score: pass confidence_available=false, do not invent 1.0; available scores must be finite in [0,1]. VAD/playback events need the exact token copied when a capture/playback operation began. **Each PLAYBACK_STARTED/DONE/FAILED event must additionally copy `source->playback_id` into `event.data.playback.id`; zero is rejected.** Never stamp a stale completion with the newest token. `data.utterance.echo_safe` controls VAD barge-in. Vision only publishes `vatt_snapshot_t` through `bot_bus_publish_vision`; runtime owns attention_manager and returns `snapshot.focus`. UI posts intentional TOUCH_WAKE, SETTINGS_OPEN/CLOSE or calls capacity-independent `bot_bus_request_stop`.

LAN ingress stamps `command.received_ms` locally and queues `bot_command_t`. Link health has separate connected/authenticated/encrypted booleans. Existing discovery/hello cannot assert authentication. Remote expression/caption commands require the current session/turn/epoch; focus generation is optional zero for focus-independent content. No remote raw gaze field exists. Every dequeued command gets command_result; enqueue failure must also be reported by ingress.

Critical queue overflow latches a separate fault signal. Runtime invalidates the session, revokes uplink and stops output. A subsequent explicit local recovery is required; old queued wake/commands do not restart it.

`runtime.token.focus_generation` stays zero because audio/session authority is independent of visual focus. Focus-bound commands use `runtime.focus.generation` explicitly. Barge-in is off by default; ordinary VAD reports echo_safe=false. The optional hardware-reference path emits a separate token/id-bound BARGE_IN event only after its live reference gates. The next turn is prepared before WAITING; VAD_STARTED must already belong to this new token and is never relabelled. Only a matching END may complete it. Explicit wake/barge-in cancellation still establishes a fresh DMA/AFE fence.


## Source-level integration follow-up

- Set `ops.now_ms` to an owner-safe wrapper of `bot_platform_now_ms()`, and refresh the clock again for downstream reconciliation after `step()`. Otherwise the caller must synchronously bound all input by the supplied step timestamp.
- `main.audio_event` must copy nonzero `source->playback_id` into `event.data.playback.id` for all three local playback event kinds (CANCELLED maps to FAILED with the same id). Runtime checks both the original token and playback id, so an old stream cannot complete a newer stream within the same turn.
- `runtime.token.focus_generation` stays zero: the current main `set_session` mapping will therefore not reset VAD when only visual focus changes.
- The audio source resets native AFE/VAD and its local speech state on a new gate epoch. Prepare the next epoch before waiting for follow-up speech; same-token transport changes preserve the bounded authorized onset window.
- `session_timeout_ms` is a **conversation progress gap**. Default 60s is bounded; capture/network PCM and cosmetic updates do not extend it. Only matching real hardware playback progress does. `Kconfig` exposes this as `BOT_RUNTIME_PROGRESS_TIMEOUT_MS`.
- Runtime automatically expires per-kind ordering history every step and clears the stopped-camera latest snapshot; sparse events and long camera sleeps cannot retain comparison anchors across the 32-bit half-range.
- New lifecycle defaults live in component `Kconfig`; callers may still override `bot_runtime_config_t`. Voice preferences use validated `bot_voice` defaults and platform NVS. Verification of the current source is described above, separately from earlier delivery records.

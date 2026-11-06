# Integration contract (API defined before main wiring)

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

`runtime.token.focus_generation` stays zero because audio/session authority is independent of visual focus. Focus-bound commands use `runtime.focus.generation` explicitly. Barge-in is off by default; the current audio adapter reports echo_safe=false and remains in that mode. VAD_STARTED in WAITING creates a new turn but does not mark the old-token utterance active. `apply_state` immediately calls `bot_audio_set_session` with the new token; its AFE/VAD fence emits a fresh START under that token if speech continues. Only that fresh START may be completed by a matching END. A very short utterance entirely before the fence is dropped, never re-labelled or uploaded as a new operation.


## Source-level integration follow-up

- Set `ops.now_ms` to an owner-safe wrapper of `bot_platform_now_ms()`, and refresh the clock again for downstream reconciliation after `step()`. Otherwise the caller must synchronously bound all input by the supplied step timestamp.
- `main.audio_event` must copy nonzero `source->playback_id` into `event.data.playback.id` for all three local playback event kinds (CANCELLED maps to FAILED with the same id). Runtime checks both the original token and playback id, so an old stream cannot complete a newer stream within the same turn.
- `runtime.token.focus_generation` stays zero: the current main `set_session` mapping will therefore not reset VAD when only visual focus changes.
- The existing audio source resets native AFE/VAD and its local speech state on a new gate epoch. Runtime now deliberately waits for that new-token START after WAITING/automatic barge-in creates a new turn.
- `session_timeout_ms` is a hard **conversation progress gap**, including ongoing continuous utterance and playback, not an inactivity-only timeout. Default 60s is intentionally bounded; neither PCM frames nor cosmetic network updates extend it. `Kconfig` exposes this as `BOT_RUNTIME_PROGRESS_TIMEOUT_MS`.
- Runtime automatically expires per-kind ordering history every step and clears the stopped-camera latest snapshot; sparse events and long camera sleeps cannot retain comparison anchors across the 32-bit half-range.
- New defaults live in component `Kconfig`; callers may still override `bot_runtime_config_t`. No test/build/device verification has been performed.

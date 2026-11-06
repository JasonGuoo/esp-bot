# Vision integration handoff

The public API is in `include/bot_vision.h`; `bot_vision.cpp` contains the real
V4L2 capture → packed upright RGB image → official HumanFaceDetect →
vision_attention path. No test, compile, flash or CI command was run.

## Platform and runtime

1. Call `bot_vision_default_config()` and set `prepare_camera`, `on_snapshot`,
   `on_status`, and `context`. Start with `start_enabled=false`.
2. `prepare_camera(context)` calls the platform's shared-I2C BSP preparation.
   **Attempt `bsp_camera_start(NULL)` at most once per boot and cache success OR
   failure.** The current BSP does not unwind its local XCLK handle after
   `esp_video_init` failure. This component also caches its first preparation
   result. A failed prepare needs a reboot; repeated V4L2 open/STREAMON is allowed
   after successful preparation. Never create a second SCCB bus or camera owner.
3. `on_snapshot(ctx, const vatt_snapshot_t *)` copies into
   `bot_bus_publish_vision`. Never retain the pointer. No frame bytes escape.
4. `on_status` publishes camera health; `stream_active && observation_valid`
   means a usable observation. Darkness, invalid frames and pause are unavailable,
   never evidence that the room is empty. `calibration_valid` remains false until
   the actual camera orientation and screen mapping have been checked.
5. Runtime alone calls `bot_vision_set_enabled(handle, camera_requested)`.
   `camera_requested` must remain independent of camera health so failure can
   recover. Vision never wakes the bot. Night/pause stops the V4L2 stream and
   inference; it does not claim sensor power or BSP XCLK is off.
6. Optional `bot_vision_stop(handle, timeout_ms)` waits for safe cleanup. A timeout
   retains the handle/resources; retry stop later. ESP_OK destroys the handle.
   Serialize stop with other lifecycle/status calls; never stop from a callback.

Status callback precedes its matching snapshot. Callbacks run on the vision
worker and must copy, enqueue, and return promptly. `attention_manager` chooses
focus; ignore legacy `vatt_snapshot_t.primary_track_id` for actuation.

After successful STREAMOFF/close and the paused/unavailable publication, vision
destroys its temporary tracker and clears its publication timestamp anchor.
Resume allocates a fresh tracker before opening the stream; an allocation failure
reports unavailable and retries. The model stays loaded. This prevents a pause
longer than the signed 32-bit clock comparison window (about 24.8 days) from
permanently rejecting new frames as old. Track IDs can be reused after pause and
must not imply continuity or identity. Runtime retires inactive bus snapshots
and reacquires focus for each session. Availability-only stop/error reports remain
valid even while no tracker is allocated.

## Exact direct dependencies / configuration

Main BSP: `espressif/esp32_s31_korvo_1 =1.1.0` (platform-owned).
This component pins `esp_video =2.4.1`, `human_face_detect =0.5.2`,
`esp-dl =3.3.14`; it requires ESP-IDF >=6.1 for the S31 target. Transitive driver
ranges remain those in the official manifests; no dependency resolution was run.

Required sdkconfig values (owned by root):

```ini
CONFIG_CAMERA_XCLK_USE_LEDC=y
CONFIG_ESP_VIDEO_ENABLE_DVP_VIDEO_DEVICE=y
CONFIG_CAMERA_OV3660=y
CONFIG_CAMERA_OV3660_AUTO_DETECT_DVP_INTERFACE_SENSOR=y
CONFIG_CAMERA_OV3660_DVP_RGB565_BE_640X480_10FPS=y
CONFIG_CAMERA_OV3660_DVP_DEFAULT_FMT_RGB565_BE_640X480_10FPS=y
CONFIG_FLASH_HUMAN_FACE_DETECT_MSRMNP_S8_V1=y
CONFIG_HUMAN_FACE_DETECT_MSRMNP_S8_V1=y
CONFIG_HUMAN_FACE_DETECT_MODEL_IN_FLASH_RODATA=y
```

PSRAM is required. The default 3 RGB565 capture buffers + one upright owned
copy alone need 2,457,600 bytes, excluding driver backup buffers, model tensors,
model loading, display/audio and stack. Model is flash-rodata: no additional
`human_face_det` partition is required. App partition fit is not measured.

## Freshness and capabilities

`esp_video 2.4.1` DVP DQBUF does not populate V4L2 timestamps. Current output is
`BOT_VISION_TIME_DEQUEUE_ESTIMATE`, `capture_timestamp_exact=false`: queued frames
are drained, then a monotonic stamp is taken after dequeue and before copy/model
inference. A configurable 200 ms uncertainty budget is included in the 750 ms
result-age filter. This budget is a conservative initial setting, not a measured
sensor exposure bound. No exact capture-time claim. Unavailable snapshots carry
a fresh availability timestamp with no faces, not re-stamped detections.

Five landmarks provide face boxes and two eye positions for temporary tracking.
There is no identity embedding, family recognition, gaze estimator, iris
estimator, or valid eye-contact evidence. Full source provenance and remaining
hardware work are in `README.md`.

## Main integration source review

The current `main/app_main.c` wiring matches this API: shared platform preparation,
copying snapshots into `bot_bus`, health from `stream_active && observation_valid`,
initially disabled capture, and runtime-owned `camera_requested`.
`CONFIG_ESPBOT_GAZE_CALIBRATED` records physical orientation/screen mapping only;
allowing approximate visual following before calibration is a runtime policy,
and must not create valid gaze or eye-contact evidence. The detector and its
tracker both default to a face-confidence floor of 0.5. Downstream focus/eye
controllers may choose a higher floor; keep those two thresholds consistent.

`ESP_VIDEO_DVP_DEVICE_NAME` is a public unconditional define in the upstream
`esp_video_device.h`. Private ioctls used here are declared by the pinned
`esp_video_ioctl.h`; zero dequeue timeout is converted to zero FreeRTOS ticks.
Required DVP/sensor/model Kconfig settings are listed above for explicit project
defaults. No test, build, compiler, dependency installation, or hardware check
has been performed.

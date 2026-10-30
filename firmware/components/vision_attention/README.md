# vision_attention — independent camera-side multi-person perception

ESP32-S31-Korvo-1 target. Standalone pure-C component, **not** the existing single-target `attention` motor/face coordination component.

## What is implemented in this branch

- `vision_attention.c`: bounded **four-face** temporal tracker, short-occlusion ID association, independent fixation clocks, looking begin/end events, primary-target selection and frame/camera health state. Coordinates are normalized and timestamps are monotonic.
- `pupil_candidate.c`: bounded full-resolution GREY8 eye-ROI dark-iris **candidate** localizer for a supplied left/right ESP-DL eye landmark; returns candidate location, relative offset and contrast quality. This cannot identify pupil reliably under glasses, eyelash shadows, low resolution, dim lighting or reflections and MUST NOT alone authorize `LOOKING_AT_BOT`.
- `include/vision_attention.h`: stable value-copy event/snapshot interface. No dependencies on LVGL, Wi-Fi, Brain, servo or ESP-DL. ESP-IDF CMake component registered.
- `tests/test_vision_attention.c`: deterministic host tests for multiple IDs/eye contact, independent gaze durations, debounce, blink/UNKNOWN, no face, camera down and pupil candidates.

**Not yet implemented / not claimed:** real board camera `esp_video` stream, ESP-DL MSR/MNP inference adapter, robust iris segmentation model, calibrated gaze-on-Bot estimator, TFLite/ESP-DL eye model, servo-to-screen transform, real-device FPS or measured accuracy. The existing single-target `attention` module remains intact.

The very first hardware iteration must connect these adapters before `LOOKING_AT_BOT` can be genuinely assessed. A detected face or dark pupil candidate must never be labeled `eye_contact_valid=true` without independent calibration.

## Host tests (from repo root)

~~~sh
cc -std=c11 -O2 -Wall -Wextra -Werror -pedantic \
  -Ifirmware/components/vision_attention/include \
  firmware/components/vision_attention/vision_attention.c \
  firmware/components/vision_attention/pupil_candidate.c \
  firmware/components/vision_attention/tests/test_vision_attention.c \
  -lm -o /tmp/vision_attention_test
/tmp/vision_attention_test
~~~

## Integrate camera / ESP-DL later, without touching this core

1. Use S31 official BSP / `esp_video` camera and timestamp frames with monotonic capture time; acquire full-resolution original image for eye ROIs.
2. Run `HumanFaceDetect::MSRMNP_S8_V1` on a resized detection image; transform its five landmarks into full-resolution normalized upright coordinates. Upstream MNP landmark order in the example: `left_eye, left_mouth, nose, right_eye, right_mouth`; do not confuse them with iris centers.
3. For each face, convert an eye ROI to GREY8; call `vatt_pupil_candidate()` on **both** eyes. Copy accepted pupil centers and quality into the observation, convert pixel coords to normalized coords. For poor eyes/low interpupillary pixels, set `*_pupil_valid=false`.
4. Run a **separate calibrated pupil/head-pose gaze estimator**; set `eye_contact_valid=true` only when it has measured enough information to estimate intersection with the physical screen region, not just the camera or frontal face.
5. Call `vatt_tracker_update(tracker, observations, face_count, capture_time_ms, true, &snapshot)`. Call with count zero on real frames with no faces. On camera fault call with `camera_ok=false`. The tracker keeps a face stable over brief tracking losses.
6. Motion: use snapshot `primary_track_id` to select the one human target for the **existing** `firmware/components/attention` coordinator. Compute **person bearing from the average observed human eyes**, using sensor intrinsics:
   - `yaw = atan2(eye_px - cx, fx)`
   - `pitch = atan2(cy - eye_py, fy)`
   Then use `attention_controller_observe()` with actual measured pan/tilt pose. The motor target is the person's **eye location**, NOT their pupil gaze direction. A snapshot also supplies `eye_midpoint` and `eye_velocity_uv_s` for bounded (e.g. <100ms) display/motor prediction, but this is **camera-relative** motion and must be corrected for the Bot's own head rotation before driving a motor. The gaze score only determines the eye-contact interaction event.
7. The existing coordinator runs its own `contact_dwell_ms` timer. **Avoid stacking two 350ms dwell delays**: if `vision_attention` already confirmed gaze, configure coordinator's contact dwell to a minimal safety check and keep servo/eye tracking fast. Do not mix image pixels with world angles. Send one `face_set_gaze()` vector to both rendered eyes only on LVGL owner task/under BSP display lock.

### Typical data flow

~~~text
camera -> MSR/MNP face detector -> multi-face boxes & five landmarks
                                   |
                       high-res eye ROI -> dark-iris / learned eye model
                                   |
                              gaze estimator
                                   |
                         vision_attention tracker
                          /                 \
       all faces + per-person history      primary human target
                           |                |
                     interaction policy  attention controller
                                            /       \
                                      head motors   face_set_gaze()
~~~

Full design, calibration, timing and acceptance criteria: [docs/visual-attention.md](../../../docs/visual-attention.md).

### Operational caution

If two people cross/fully occlude one another, box-only association is fundamentally ambiguous. The tracker flags close association costs; it does **not** provide re-identification guarantees. Avoid attaching long-term identity or private user profiles to `track_id`. Multiple people may look at Bot concurrently; state and durations remain per person except where the association is ambiguous.

Times are tunable defaults, not measured board latency. For actual eye gaze, the **eye crop must have sufficient pixels**; a face detector operating at 160×120 alone is inadequate.

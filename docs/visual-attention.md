# Visual Attention v0.1 — multi-person eyes, gaze and eye contact

**Target:** ESP32-S31-Korvo-1 V1.1 (esp32s31), onboard camera; local processing independent of network, Brain, LVGL and motors. **Status:** standalone multi-face tracker and pupil candidate code are on main; LIVE camera/ESP-DL capture and true calibrated eye-gaze integration have NOT been implemented or device-tested. See [architecture.md](architecture.md) for the canonical system architecture.

## Non-negotiable behavior

- Recognize **0 to N faces**, with default maximum **4 active face tracks** (configurable at compile time). Every visible person has an independent ephemeral `track_id`.
- For **each person**, report face box, face center, **left/right eye centers**, **left/right iris/pupil centers if reliable**, head orientation, gaze estimate/direction, gaze uncertainty, current look status, current continuous fixation duration, cumulative look duration, and last update timestamp.
- Persist an ephemeral ID while a face moves, briefly disappears, or other faces enter; never confuse two people’s fixation clocks. Tracker IDs are NOT biometric identity.
- Global states: `NO_FACE`, `FACE_PRESENT`, `LOOKING_AT_BOT`, `UNAVAILABLE`. Person states: `FACE_PRESENT`, `LOOKING_AT_BOT` (plus `UNKNOWN` gaze evidence). Global `LOOKING_AT_BOT` means **at least one** person has confirmed attention, not that everyone is looking.
- Keep updating eye/face positions while eye contact is *not* established; target acquisition should never wait for the fixation timer.
- Expose ordered events (`FACE_ENTER`, `FACE_EXIT`, `LOOK_BEGIN`, `LOOK_END`, `PRIMARY_CHANGED`) and immutable snapshots. Consumers decide when to animate eyes, speak, or drive pan/tilt.
- A missing frame, blink, poor eye crop or camera fault is `UNKNOWN`/**`UNAVAILABLE`**, never automatically “not looking”; conversely, a frontal face is **NOT** evidence of pupil-based eye contact.

## Processing chain

1. **Camera source:** official S31 board BSP + `esp_video` DVP camera (OV3660 / SC101IOT, detect populated module). Acquire frame and monotonic frame timestamp. Orient/mirror frames once into canonical camera coordinates; keep high-resolution frame for eye ROIs.
2. **Multi-face detector:** use Espressif **ESP-DL `HumanFaceDetect::MSRMNP_S8_V1`**. MSR finds candidate face boxes; MNP refines boxes and returns **5 landmarks**: left eye, left mouth corner, nose, right eye, right mouth corner. **These are eye landmark points, NOT iris or pupil centers.** Official S31 model benchmarks ~19 ms (MSR) + ~3.5 ms (MNP) per proposal, excluding image capture/copy and other load.
3. **Association:** match each face to previous frame tracks using predicted box center, IoU, face scale and velocity. Keep IDs through short occlusion; mark ambiguous crossings as uncertain instead of silently swapping identities. Never imply these ephemeral IDs identify a person across sessions.
4. **Dedicated eye/iris processing:** for each tracked face, get both eye ROIs from the **original-resolution image** and face landmarks; handle tilt/scale; run a bounded local pupil/iris segmentation or tiny dedicated eye-landmark model. Distinguish pupil/iris from eyelash, shadow, glint, glasses and eyelids. Output independent per-eye **absolute image coordinates**, local eye-relative `pupil_u/v`, confidence and `valid`. Disagreeing eyes, closed eyes, insufficient resolution or failed quality gate = **unknown**, not a fabricated gaze.
5. **Gaze estimation:** fuse pupil/iris offset **relative to eye corners/eyelids**, estimated head pose, camera intrinsics, physical eye-to-screen geometry and calibration. Estimate a **3D gaze direction** and check whether its uncertainty cone intersects the **Bot display/face region**, not just the camera optical center. Eye contact is a **separate evidence signal**, not a trivial threshold on nose-centered frontal pose. A heuristic pupil displacement estimator is only a **prototype** until calibrated/validated.
6. **Temporal stabilizer:** independent per-track enter/exit hysteresis, blink/occlusion grace, fixation intervals, look-away, current and total look duration.
7. **Publish:** broadcast an immutable snapshot to the future LVGL expression coordinator and head-servo coordinator. Vision cannot touch LVGL or servo I/O.

## Core data contract

All image coordinates are **canonical, unmirrored, upright** and **normalized 0..1**, origin top-left, +x right, +y down. The frame size/camera calibration must be supplied for conversion to a camera ray.

Each `face` provides:

| Field | Purpose |
| --- | --- |
| `track_id`, `visible`, `seen_at_ms`, `last_seen_ms` | Multi-person association and freshness |
| `bbox`, `left_eye`, `right_eye`, `left_pupil`, `right_pupil` | 2D localization; pupils are valid only with their own confidence flags |
| `head_yaw_deg`, `head_pitch_deg`, `gaze_yaw_deg`, `gaze_pitch_deg` | Separate head and **estimated** gaze directions |
| `eye_contact_score`, `eye_contact_valid`, `gaze_quality` | Model-derived/geometry-calibrated evidence for *looking at Bot*; cannot be set by face detector alone |
| `status`, `continuous_look_ms`, `cumulative_look_ms`, `look_count` | Individual temporal interaction behavior |
| `eye_midpoint`, `eye_velocity_uv_s` | Smoothed human eye target and camera-relative motion; bounded short-horizon prediction (only after compensating robot-head ego-motion) |
| `stale_ms` | Consumers must stop driving a moving target when old |

The public multi-person perception C API lives in `firmware/components/vision_attention/include/vision_attention.h`. The existing `firmware/components/attention/` is a **different single-target motion + face-eye coordinator**; do not overwrite it. Camera capture, ESP-DL model loading and an optional learned gaze classifier belong to separate adapters.

### Multi-face example

~~~json
{
  "state": "LOOKING_AT_BOT",
  "timestamp_ms": 138250,
  "primary_track_id": 12,
  "faces": [
    {"track_id": 12, "status": "LOOKING_AT_BOT", "continuous_look_ms": 2450, "cumulative_look_ms": 6100,
     "left_eye": [0.31, 0.38], "right_eye": [0.40, 0.38], "gaze_direction_deg": [2.5, -1.5], "gaze_quality": 0.84},
    {"track_id": 15, "status": "FACE_PRESENT", "continuous_look_ms": 0, "cumulative_look_ms": 0,
     "left_eye": [0.70, 0.35], "right_eye": [0.77, 0.35], "gaze_direction_deg": [19.0, -7.0], "gaze_quality": 0.72},
    {"track_id": 18, "status": "FACE_PRESENT", "continuous_look_ms": 0, "cumulative_look_ms": 900,
     "left_eye": [0.49, 0.57], "right_eye": [0.56, 0.57], "gaze_direction_deg": null, "gaze_quality": 0.0}
  ]
}
~~~

*Illustrative values only; not a claim of actual sensor accuracy.*

## Temporal policy (initial *tuning targets*, not measured capability)

- Person confirmed after **2 compatible detections**; continue scanning while visual sensing is policy-enabled, even when the current frame has no face. In SLEEPING/NIGHT_DARK, camera capture/inference is deliberately paused and local full-phrase wake remains active.
- Looking begins after **~350 ms** valid gaze-on-Bot evidence. This avoids starting a conversation from a single accidental glance.
- Looking ends after **~450 ms** of continuous valid look-away; `UNKNOWN` gaps (blinking or camera frame lost) tolerated for only **~250 ms**. Confirmed look-away and uncertainty use different timers.
- Consecutive short looks count as separate fixations once separated by confirmed look-away; maintain `continuous_look_ms` and `cumulative_look_ms` **per track**, and emit `LOOK_END` duration.
- `NO_FACE` after a presence debounce window **~550 ms** without a new face. Expire tracked IDs after **~800 ms** lost. `UNAVAILABLE` immediately on camera failure/stale stream.
- Additional semantics for future interaction policy: short glance (<1s), engagement (~1–2s), sustained attention (>2s), look-away, return glance. Thresholds are configurable.
- Times must be computed from monotonic **capture timestamps**; never from frame counts. If inference is late, drop stale frames rather than building a queue.

## Fast and accurate visual motor interaction

**Two distinct coordinate spaces must not be mixed:**

- Sensor: `(u,v)` from the high-resolution source and camera optical rays. An eye landmark is a **human target point** used by Bot to look toward the human.
- Eye contact: the person's **gaze direction** is an estimated ray **from human eyes toward the Bot display**. It is **not** the same as their eye position and cannot be used unmodified as servo direction.
- Actuation: target ray transformed through calibrated **camera → moving head → pan/tilt axis** transforms; combine with motor encoder/command feedback. Apply screen-camera offset and optical-axis mounting yaw/pitch.
- Rendering: face eye-pupil position is driven by the **current target ray minus current physical head orientation**; clamp both rendered eyes symmetrically and account for mechanical lag. Don't send camera x directly to `face_set_gaze` while servos turn.
- vision_attention may suggest a primary_track_id using a local heuristic, but **attention_manager** is the SOLE authority for the active interaction target and target-switch hysteresis. Prefer maintaining the current conversation participant over switching to someone who happens to stare at Bot. A detected gaze never initiates a conversation by itself.
- Motion interpolation/prediction must use observed target velocity with bounded prediction horizon, otherwise it will oscillate or “stare past” a moving person.

### Suggested scheduler — subject to real device profiling

| Task | Initial target |
| --- | --- |
| Camera capture | 15 fps or best sustainable |
| Full-scene face detect | 5–10 Hz |
| Per-track eye ROI analysis | 10–15 Hz with quality budget |
| Tracking publish/short prediction | 20–30 Hz |
| LVGL gaze animation | Existing ~30 Hz |
| Pan/tilt servo control | Controller-dependent, e.g. 50 Hz |
| Freshness for motion commands | <= 200 ms (else freeze/return neutral) |

For N faces, cap work per frame but schedule **fairly across people**. Never permanently run gaze estimation on only one primary person: another person may become the new engaged target. Profile 1/2/3/4 face latency, image size, memory, LVGL interference, and audio coexistence. Full HD crops at 160×120 detector resolution cannot contain reliable pupils; eye ROIs must originate from the full sensor image.

## Runtime and hardware constraints

- **Passive policy:** idle face detections or human eye contact MUST NOT wake or initiate a conversation. Voice wake is the complete phrase **小呆小呆**, handled exclusively by local audio + bot_runtime; see [behavior-lifecycle.md](behavior-lifecycle.md).
- **Fixed, non-actuated base V1:** the whole robot can be turned/moved, and displayed eyes can follow a human only while face is still inside FOV. There is no physical camera re-aiming and no installed IMU. The eventual pan/tilt+IMU mode belongs to [eye-contact.md](eye-contact.md).
- **Focus ownership:** retain all person tracks independently; attention_manager selects the one current human. A visual primary_track_id in the current core is a sensor-side *recommendation*, not an instruction to talk or move.
- **Timestamp and confidence:** camera-relative eye velocity changes when Bot itself moves. Clamp any prediction to a short horizon; preserve observed capture timestamps and mark old snapshots stale.
- **Visual loss vs interaction loss:** invalid vision means cannot claim eye contact, but runtime may preserve an ongoing audio conversation for a bounded interval. Night pauses frames and should not be misrepresented as NO_FACE.
- **No-CI project rule:** no new CI/automated verification jobs. Camera, gaze and wake integration require actual hardware/model resources; absence of those resources is not grounds to fake behavior.

## Camera/eye calibration and physical limitations

1. Record sensor orientation, `fx, fy, cx, cy`, distortion and mirror configuration.
2. Record camera center and optical-axis offset from screen center; also its offset from the pan/tilt pivot.
3. Optional face size + an anatomical interpupillary-distance prior estimates subject depth **coarsely**; mark low confidence; do not claim precise 3D distance from a single view.
4. Measure the location of the screen/face rectangle relative to the camera. A user looking at the left or right eye of the displayed Bot should count as **LOOKING_AT_BOT**, even if not staring at the camera lens.
5. On a stationary fixture, test at varying left/right and near/far positions, including two people, glasses, reflections, looking at phone, face frontal with eyes averted, blinking, moving heads, dim light.
6. If an eye is only a few pixels across, exact pupil localization is impossible; fall back to face tracking and `eye_contact_valid=false` instead of claiming accurate mutual gaze.

### Acceptance gates

- Demonstrate 0/1/2/3 people each with distinct `track_id`, face/eye positions and independent gaze durations; moving/crossing subjects must not silently corrupt the other person’s history.
- Frontal face + eyes pointed away **must not** be classified as confirmed eye contact.
- Looking at Bot screen but not exactly at camera should be supported by calibrated geometry.
- Blink should not trigger `LOOK_END` every time; sustained 2 s gaze must be reported as ~2 s from timestamps.
- Initial response to a new face must not wait for `LOOK_BEGIN`; face position updates stay fresh even if gaze is unknown.
- Motor/display integration can consume the same snapshots, and gaze remains centered after pan/tilt motion; calibration allows adjustable camera-screen alignment.
- On camera failure global status is `UNAVAILABLE`, not `NO_FACE`. No image is uploaded to Brain, and track IDs/frames are not persisted.

## Implementation phases

**A — core, independent of board (merged into main; NOT a live perception pipeline):** multi-face temporal tracker, value-copy output contract, per-person event/duration accounting, provisional grayscale pupil-candidate localizer and optional host-side example tests. These are software building blocks, not camera/eye-gaze hardware verification.

**B — S31 acquisition + ESP-DL adapter:** official ESP-IDF 6.1, BSP, `esp_video`, MSR/MNP face detection and five landmarks, then high-res eye crops. Check all source API versions and memory budgets on real hardware.

**C — real gaze quality:** collect consented/locally processed short test sequences and annotate on-screen vs off-screen; evaluate classical pupil segmentation first, then a tiny quantized eye-landmark/gaze model if the classical method fails. Choose thresholds from held-out people/glasses/distance, not anecdotal demos. Eye gaze could remain `UNKNOWN` on small, dark or occluded faces.

**D — interaction:** coordinate calibration, motion coordinator and face-expression coordinator as separate modules; only events/target snapshots cross module boundaries.

## References

- [ESP32-S31-Korvo-1 V1.1 Board Manager](https://docs.espressif.com/projects/esp-board-manager/en/latest/references/boards/esp32_s31_korvo_1.html)
- [ESP-DL Human Face Detect S31 support and timing](https://github.com/espressif/esp-dl/blob/master/models/human_face_detect/README.md)
- [ESP-DL MNP model landmark output](https://github.com/espressif/esp-dl/blob/master/esp-dl/vision/detect/dl_detect_mnp_postprocessor.cpp)
- [ESP-DL result_t bbox/keypoint schema](https://github.com/espressif/esp-dl/blob/master/esp-dl/vision/detect/dl_detect_define.hpp)
- [Survey on gaze calibration and head-motion issues](https://www.sciencedirect.com/science/article/pii/S0031320322004241)
- [Appearance-based gaze evaluation with and without calibration](https://ut-vision.org/publication/2019-zhang-evaluation/)

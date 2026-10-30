# Gaze coordination and visual servoing — fixed base first, moving head later

**Canonical gaze-control specification.** Existing code: firmware/components/attention (single-target geometry planner), firmware/components/vision_attention (multiple face/eye tracks) and firmware/components/face (LVGL eyes). This document defines how those pieces **should be integrated**, not a claim of running camera-to-face code. Product behavior/wake policy is specified separately in [behavior-lifecycle.md](behavior-lifecycle.md).

## 1. Distinguish the three meanings of “gaze”

1. **Human eye location**: midpoint between two human eye landmarks in camera coordinates. This is the BOT's spatial target for display eye tracking or future motor pointing.
2. **Human looking at Bot**: separate, uncertain estimate of where the human eyeballs/head point, based on eye ROIs, calibration and display intersection. A face aimed forward or a dark pupil is NOT proof.
3. **Bot appearing to look at human**: one shared symbolic eye vector shown by both rendered eyes. This is expressive 2D UI, not a mechanically steerable optical sensor.

Only one official target is selected by **attention_manager**, under bot_runtime permission. Perception can observe multiple people simultaneously; its primary_track_id should be treated as a *suggestion* when attention_manager exists, not another authority.

## 2. V1: fixed (non-actuated) camera and head, manually movable whole robot

- No head actuator and no gyroscope installed. **Stationary mode is an explicit runtime hardware capability**, not a fake 0-degree measured servo state.
- The whole device can still be carried, turned or tilted. The screen, camera and mic rotate together, maintaining a known **rigid camera-to-screen** relation.
- Use **latest observed camera-relative** eye midpoint for rendered gaze. No North, tabletop level, world map or absolute yaw/roll is needed to update the eyes when the human remains in view.
- Human moves OR Bot is moved → face location in new camera frame changes → update gaze target. No need to decide which one moved to close this simple relative feedback loop.
- **NO physical camera re-aiming possible.** If Bot is turned so the target leaves FOV, only retain engagement as a separate policy state and optionally try bounded association on subsequent frames. Do NOT declare stable eye contact with an unseen person or silently assign the same track ID to a different face.
- **DO NOT recenter eyes just because time passes** while the active human remains off-center and the head has not actually turned. The gaze remains comfortably sideward, within rendering limits.
- If no valid target / low quality / stale: stop trusting previous position, ease towards neutral and await new visual evidence, while runtime may preserve conversation state.
- Human may look away while still speaking; conversation target is not automatically dropped because looking_at_bot=false.

### Geometry and calibration in stationary mode

Use an upright, unmirrored, normalized camera image convention (u,v in 0..1, top left origin). Convert human-eye midpoint to optical bearings with camera intrinsics, e.g. under pinhole approximation:

~~~text
pixel_x = u * width, pixel_y = v * height
camera_yaw   = atan2(pixel_x - cx, fx)
camera_pitch = atan2(cy - pixel_y, fy)
~~~

Then account for camera-to-visual-eye placement and near-field parallax. Camera lens above/beside the screen midpoint means a person looking at the display might not look directly into the lens. A calibrated empirical mapping to Face's -100..100 gaze range is appropriate; monotonic clamping and smooth interpolation prevent an unnatural extreme appearance. Save lens orientation, mirror state, camera/display offsets and field-of-view calibration in the platform config.

On high roll/tilt, camera image orientation and screen geometry must be handled explicitly. With no IMU, do not claim to know gravity-relative roll; tracking remains relative to observed eye landmarks. Apparent human face roll/eye-line can give a limited visual cue, not a robust gravity measurement.

## 3. Motion handling and target continuity

**Current without IMU:** use new visible face/eye observations, short-lived per-track motion prediction and optional full-frame visual ego-motion estimates. Do not blindly treat a camera-wide pixel shift as the human walking; don't promise perfect identity continuity under crossings or complete occlusion.

Snapshots should include track ID, focus_generation, capture timestamp, visible/unknown, current eye midpoint, velocity and confidence. Eye velocity in current code is **camera-relative**; when the head itself moves in a future build, it must be compensated using measured joint/camera pose before extrapolating target motion.

- Light visual tracking update must not wait for look-at-Bot confidence/dwell timer.
- Reuse only fresh target data; bounded prediction horizon, e.g. <=100ms after last solid observation, is a tuning suggestion, not a verified guarantee.
- Separate short localization loss (~hundreds of ms) from longer conversation/focus retention (~seconds). A retained relationship is NOT a valid gaze measurement.

## 4. Future: actively moving pan/tilt head

Future physically actuated head introduces frames:
- C camera optical frame
- H articulated head frame
- B device base frame
- optional local fixed reference W (not geographic North)

Use actual actuator pose, camera extrinsics and 3D rotations to transform a target camera ray into a stable mechanical aiming task. At small angles the current C attention planner approximates target_base_yaw = measured_head_yaw + camera_offset + target_camera_yaw. This is not enough for arbitrary body tilt or substantial pan/tilt, which need full rigid transforms and actuator reachability checks.

**Eye–head coordination**:
1. New target appears: rendered eyes lead slightly; head begins turning almost simultaneously when required.
2. While head moves: eyes compensate so the **combined gaze** stays toward the same human eye midpoint.
3. As head physically aligns: eye residual decreases, eyes return to near center.
4. Once aligned: head holds comfortable orientation with rate limits / dead zone to prevent mechanical hunting.
5. If joint limit or user holds device: clamp safely; no false "eye contact aligned" state.

The current attention code sets illustrative defaults such as eye_max_yaw_deg=12, eye_comfort_yaw_deg=5, head_speed_deg_per_s=55 and contact_dwell_ms=320. Those are unverified tuning values, NOT servo or human-gaze specifications. In future FACE_TO_FACE mode head should converge close to human eye target (small deadband) so the eyes truly recenter; TRACKING mode may leave larger comfortable residual. Fixed mode has **no mechanical recenter**.

Physical head pose must be measured or honestly estimated; commanded motor setpoint is not proof of movement. Add safety around collision, hands holding the head and extreme base tilt before real actuation.

## 5. Optional future head-mounted IMU

An added 6-axis gyro/accelerometer may rapidly estimate rotational disturbance and gravity-relative tilt, improving short-term eye/head stabilization. It does **not** directly identify people, give accurate translation or provide drift-free absolute heading. Vision corrects long-term target bearing when visible. Full 3D transforms are needed under tilting; pan/tilt motors cannot independently cancel arbitrary roll.

No IMU or motor is required to implement the **current camera-relative rendered-eye follow**.

## 6. Acoustic cue is not visual re-aiming

The two-mic array may later estimate sound direction after a valid 小呆小呆 wake. DOA is a spatial hint with front/back and elevation ambiguity (see [audio-direction.md](audio-direction.md)). With no motor it cannot cause the camera to turn. Once a face is acquired, its eye midpoint is the fine target; human iris direction only judges whether they look back at Bot.

## 7. Core API relationship

~~~text
vision_attention  -- all tracks + eye midpoint + look evidence -->
attention_manager -- exactly one active focus + freshness -------->
attention         -- one common gaze_x/gaze_y + optional head plan -->
face              -- LVGL: both eyes render common gaze; expressions independently

head_motion (future) --> actual joint pose --> attention
audio DOA (future)   --> candidate speaker direction --> attention_manager
bot_runtime          --> policy/capability/permissions; never per-frame pupil math
~~~

Only the Face GUI task or a holder of the BSP/LVGL display lock may call face_set_gaze. Remote/manual face.set gaze requests are renderer debug commands, NOT authoritative live tracking. Avoid duplicate gaze dwell timers in vision_attention and attention: looking-at-Bot recognition and mechanically achieved mutual alignment are different stages, but two arbitrary delays should not accumulate unnoticed.

## 8. Failure / manual observation criteria

- Face merely present → may follow location when engaged; cannot mark human-looking-at-Bot true without gaze evidence.
- Engaged A turns head/eyes away → retain target as policy permits; no fake mutual eye contact.
- Bot manually turned while A still in FOV → visually remeasure and adjust both rendered eyes consistently.
- Bot manually turned so A leaves FOV → localization lost; cannot physically recenter camera in stationary mode.
- A and B cross or hide each other → association may be ambiguous; no unbounded continuity claim.
- Bot sleeping / night dark → camera/inference paused; only full local wake phrase reactivates. Eye gaze is not a wake trigger.
- Two-axis motor later → head follows; rendered eyes recenter only if confirmed actual pose aligns, and safe joint constraints permit.
- No camera, wake model, motor or IMU validated on the real board is implied by this document.

See [visual-attention.md](visual-attention.md) for the multi-face perception model and [hardware-power.md](hardware-power.md) for device capabilities and screen sleep limitations. No CI (AGENTS.md).

# Human eye contact: shared 3D target, head pose, and pupil gaze

## User-facing goal

The bot should appear to notice the **actual person** in the room: turn or lift its head smoothly, then meet the user's eyes with a calm, centered gaze. Avoid independent pupil convergence, hunting between people, staring after a person leaves, and the false impression of eye contact when the robot actually faces elsewhere.

**A face detected by the camera is not the same as the person looking at the bot.** Mutual eye contact is a distinct state that requires an independently trained/calibrated head-pose/eye-gaze estimator, sufficient confidence, a shared physical target direction, stable alignment and dwell. The current firmware does NOT contain the vision inference or actuator drivers.

## One target, three consumers

```text
Camera -> person/face detection -> target track ID + confidence
       -> (later) head pose + human pupil gaze -> looking-at-robot confidence
       -> 2D pixel + camera intrinsics -> angular yaw/pitch relative to camera
       -> camera/head calibration + measured head joint angles
                    |
                    v
             ATTENTION COORDINATOR     firmware/components/attention
                    |
          +---------+---------+
          |                   |
    shared eye vector     head yaw/pitch setpoints
          |                   |
  Face API -> Renderer   future motor driver
     (one gaze)          (rate/angle limits)
```

The **Attention** module is the owner of target acquisition, alignment and control policy. The **Face** module is only a renderer/controller consuming a normalized gaze vector and expression. The **Motor** module will own servo hardware, feedback, limits and safety. The **Vision** module will own face selection, direction and human look-at-bot estimation. The Mac/Linux **Brain** can host heavyweight vision inference if needed, but must send timestamped target directions instead of directly steering each pupil.

## Geometry and synchronization

- Standard convention: **positive yaw = robot's right**, positive pitch = **up**. LCD eye y is positive downward; a target above the robot maps to a negative eye y. An x/y target from the camera is NOT directly usable after the head turns.
- Vision converts pixel offsets into camera-relative bearings using camera intrinsics (focal length/field of view); at large off-axis angles use calibrated projection, not a naive normalized fraction.
- Moving camera bearings transform into fixed robot-base bearings using measured head yaw/pitch and calibrated camera-to-face boresight offsets. Head actual/estimated angle is mandatory; a motor command is not proof of motion.
- For accurate 3D geometry, implement rigid camera extrinsics, full rotations, and distance/parallax correction (camera is not at the midpoint of the drawn eyes). The current controller uses a small-angle additive yaw/pitch approximation. Validate limitations on the real prototype before claiming precise eye contact.
- Assign one track ID to the active person. The future Vision target selector should use confidence and hysteresis so the bot does not rapidly switch between multiple faces.
- In local firmware use a monotonic millisecond clock. Observations arriving from the Brain MUST be converted into local time/age; do not directly compare unsynchronized Mac and ESP32 wall clocks.

## How head and eyes cooperate

1. Aim head toward the user when required, within measured safe yaw/pitch joint limits. A small angular comfort zone means micro face movement is absorbed by pupils rather than causing servo jitter.
2. The pupils correct the **residual angle between the user's bearing and the current actual head orientation**. Both eyes receive the **identical gaze vector**; never steer pupils to two different tracked pixel coordinates.
3. Eye movement is intentionally limited to the inner 75% of the normalized range. When the user is far to the side, the head begins turning while the eyes look modestly in the same direction. Do not claim aligned eye contact until the actual pose permits it.
4. On lost/stale detection, smoothly recenter the eyes and rate-limit the head back toward neutral. No random hunting or tracking a frozen bounding box.
5. Only set an internal mutual-contact flag when the user is independently estimated to be looking toward the robot, the observations are fresh, the angular residual is reachable, and the state persists for a dwell interval. Even then, it is an estimate, not a guarantee of what a human is attending to.

**Current illustrative defaults** (not calibrated servo settings): head yaw -45°..45°, pitch -18°..25°, head setpoint slew 55°/s, comfortable pupil residual ~5° yaw / 4° pitch, max visible tracking ~12° yaw / 9° pitch, report expiry 800ms, contact report freshness 180ms, dwell 320ms. They MUST be tuned against real hardware and user observations. The module does not drive a motor.

## Code interface

```c
attention_config_t cfg = attention_default_config();
attention_controller_t tracking;
attention_controller_init(&tracking, &cfg);

// Vision module delivers timestamped camera-relative person bearing +
// separately estimated look-at-robot flag. Use measured joint pose.
attention_controller_observe(&tracking, &obs, &measured_head);

// Run 20-50 Hz in attention/motor task, NOT inside an LVGL render callback:
attention_output_t plan = attention_controller_step(
    &tracking, &measured_head, esp_millis, motors_available);

// Future motor driver checks physical end stops and servo feedback:
// pan_tilt_request(plan.head_target_yaw_deg, plan.head_target_pitch_deg);
// Face update on LVGL task / under BSP lock:
face_set_gaze(face, plan.eye_x, plan.eye_y);
```

## Acceptance tests before claiming real mutual gaze

- A person appears left, right, high and low: head tracks the correct direction; both pupils move the same way and never cross.
- Person moves while the head rotates: target does not drift as coordinate frames change.
- A person is detected but looks away: bot may look at the person but must not report **mutual** gaze.
- Loss and reacquisition under variable lighting, glasses, camera occlusion, and multiple-person switching; require continuity/hysteresis.
- Person beyond joint limits: no servo collision or sustained maximum pupil deflection; no false aligned status.
- Test at near and far distances (camera/display parallax), seated/standing heights and off-axis positions; manual qualitative review by actual observers.
- Micro-movement filters avoid servo buzzing, fast ping-pong and unnatural over-following; maintain natural blinks.

## Status

Implemented and native-C-tested: geometry coordinator, one shared gaze vector, confidence and dwell gating, lost-target recovery, rate-limited head setpoints and safety limits. **Not implemented:** camera inference, gaze-to-robot classifier, timestamp sync for remote observations, target arbitration, physical servos/feedback and real-world calibration.

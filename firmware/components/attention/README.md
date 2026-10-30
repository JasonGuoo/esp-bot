# Attention coordinator — gaze, pan/tilt and human eye contact

Independent C11 component with no LVGL, camera driver, servo driver, Wi-Fi or Brain dependency. The code generates a single target direction and a coordinated head/eyes plan. **This is NOT a working visual sensor or motor driver; fixed-base V1 and future moving-head modes are separated in [docs/eye-contact.md](../../../docs/eye-contact.md).**

**Current scope:** geometry, confidence/dwell gates, stale-target fallback, joint limits and servo-setpoint rate limits. The current code is a geometric planner, NOT an integrated camera/eye-gaze pipeline. **V1 hardware has no actuators or IMU:** camera-relative gaze only updates the screen eyes while a selected human stays in view; actual head turns, joint pose and eyes recentering after alignment are FUTURE capability. The existing attention controller API is not yet wired to vision_attention or a stationary-mode application adapter.

1. Camera observation: 2D angular bearing of detected person from the moving camera optical axis, face-detection confidence, plus *independent* person-looking-at-robot estimate (head pose + ocular gaze).
2. Convert to the robot base frame using measured yaw/pitch and the calibrated camera-to-display optical offset. A static pixel x/y is insufficient when the head moves.
3. Turn head (within calibrated yaw/pitch limits) toward the target; leave a small comfortable residual for pupils; rate limit the motor setpoints.
4. Send ONE x/y gaze vector to both rendered eyes. Never independently steer pupils to screen centers (cross-eyed effect). Head moves in the same coordinate system as gaze.
5. Do not claim mutual eye contact merely because a face is detected: require adequate confidence, calibrated look-to-robot classification, alignment within eye travel, and ~320ms of continuous fresh observations.
6. If camera reports become stale, center gaze and rate-limit the head's return to neutral; never stare at an absent person.

Coordinates: positive yaw = robot-right, positive pitch = up. Eye x positive right, eye y negative up. Default head servo limits in the code are illustrative ONLY; must be replaced by actual physical range + collision tests.

## Example integration (not yet connected to camera or motors)

```c
attention_config_t config = attention_default_config();
attention_controller_t controller;
attention_controller_init(&controller, &config);
// Camera vision module: call attention_controller_observe() with the
// current measured head pose and timestamped face bearing.
attention_output_t output = attention_controller_step(
    &controller, &measured_head, now_ms, motors_available);
// Motor module: request head_target_yaw_deg/pitch_deg setpoints.
// GUI task + BSP LVGL lock: face_set_gaze(face, output.eye_x, output.eye_y);
```

Important: `measured_head` must come from actual servo feedback or a calibrated motion estimate; do not feed commanded angles back as measured pose without caveats. Physical calibration must include camera/screen axis misalignment and close-range parallax. The eventual human-looking-at-robot classifier is independent of simple face detection.

**Project delivery:** no CI workflows or CI gates. Optional local sample tests remain in the repository, but code implementation does not depend on running them without a suitable environment (see [AGENTS.md](../../../AGENTS.md)).

## Integration responsibility

- **vision_attention** observes multiple people, including two eye landmarks, provisional pupil features and per-person looking-back evidence.
- **attention_manager** (not yet implemented) owns the one official human conversation target, not this controller or the visual primary_track_id heuristic.
- **bot_runtime** authorizes active versus passive tracking. Seeing a person or being looked at does NOT wake 小呆; only complete local phrase 小呆小呆 (or deliberate user action) allows conversational engagement.
- This Attention module computes a common two-eye gaze output; **face** renders it and must not override it with an idle animation while tracking.
- Future **head_motion** will consume optional motor setpoints and provide actual pose. Do NOT treat commanded angles as actual feedback; in V1 no mechanical correction is available.

Overall architecture: [docs/architecture.md](../../../docs/architecture.md); contracts: [docs/module-contracts.md](../../../docs/module-contracts.md).

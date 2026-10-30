# Attention coordinator — gaze, pan/tilt and human eye contact

Independent C11 component with no LVGL, camera driver, servo driver, Wi-Fi or Brain dependency. The code generates a single target direction and an anatomically coherent head/eyes plan.

**Current scope:** geometry, confidence/dwell gates, stale-target fallback, joint limits and servo-setpoint rate limits. This is a planning algorithm, **not implemented physical tracking**; camera gaze detection and the motor driver are future modules.

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

Native CI compile: `cc -std=c11 -Wall -Wextra -Werror -pedantic -Ifirmware/components/attention/include firmware/components/attention/attention.c firmware/components/attention/tests/test_attention.c -lm -o /tmp/a && /tmp/a`.

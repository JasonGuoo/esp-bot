# Hardware profiles, camera exposure, power and capability gating

**Board today:** ESP32-S31-Korvo-1 V1.1, target esp32s31 / ESP-IDF 6.1+, LCD 800x480 touch, built-in camera and two microphones. **No installed head motors, no on-board IMU.** Physical robot/base might be picked up, tilted, moved or rotated by a person at any moment.

Product requirement: passive companion with **local always-listening full phrase 小呆小呆**, application sleep/black screen at night, vision-first gaze follow and optional future pan/tilt and IMU.

## 1. Board evidence (do not confuse S31 with S3)

- Espressif [S31 BSP README](https://github.com/espressif/esp-bsp/tree/master/bsp/esp32_s31_korvo_1) documents OV3660 camera and the audio codec / dual microphone path. Confirm the populated sensor module before choosing registers/driver; real boards/revisions may differ.
- Official [S31 BSP header](https://github.com/espressif/esp-bsp/blob/master/bsp/esp32_s31_korvo_1/include/bsp/esp32_s31_korvo_1.h) declares **BSP_CAPS_CAMERA=1**, **BSP_CAPS_AUDIO_MIC=1**, **BSP_CAPS_IMU=0**; I2C mapping includes SDA=GPIO0, SCL=GPIO1.
- Espressif [2-mic DOA live example](https://github.com/espressif/esp-apa/blob/main/components/esp_apa_doa/examples/live_example/main/app_main.c) uses 43 mm spacing and synchronized 16 kHz 2-mic PCM; details in [audio-direction.md](audio-direction.md).

Do not assume exposed, free, dedicated GPIO connectors solely because a BSP GPIO macro exists. Check carrier board wiring, voltage, existing shared-I2C address and physical access before attaching additional modules.

## 2. Current stationary profile

**Hardware capability flags** (values at current development stage; availability/health is a separate runtime dimension):

| Capability | V1 | Notes |
|---|---|---|
| display, touch, Face LVGL | Present | Screen sleep/physical blackout not yet validated |
| camera / face/eye sensing | Camera present; pipeline not yet wired | Existing tracker is offline/standalone code, not a live camera implementation |
| two-microphone audio | Physical hardware present | Local wake/DOA pipeline not yet integrated |
| wake model for 小呆小呆 | **NOT provisioned** | Name in JSON ≠ acoustic model |
| pan/tilt motors and joint feedback | ABSENT | No camera re-aiming, no physical head raise |
| IMU/gyroscope/accelerometer | ABSENT | Cannot infer absolute roll/heading from an uninstalled IMU |
| macOS/Linux server LAN discovery | Standalone code | Not full authenticated AI/audio integration |

Represent **capability installed**, **driver initialized**, **health**, and **policy enabled** separately. Never advertise camera gaze or wake capability as working solely because the interface exists.

## 3. Vision-first with no motors and no IMU

If Bot is moved/rotated while user stays inside FOV:
- vision_attention detects current eye midpoint at a new camera-relative coordinate.
- attention_manager preserves the human interaction focus if observations can still be associated.
- attention calculates *rendered-eye* aim using current camera-relative target and camera-to-screen calibration.
- face updates both eyes coherently, within natural display limits.

If user leaves FOV after base rotation:
- **No hardware can turn the camera back**. The software can preserve conversation state for a bounded grace period but must mark visual localization and eye contact as uncertain/lost.
- Eye movement is not a real camera reorientation. Do not claim successful mutual gaze while the person is out of frame.
- Acoustic DOA may be available as future search cue, but cannot reposition a stationary camera.

No absolute compass, level tabletop or global room frame is assumed. The current fixed camera-to-screen transform is **rigid within the robot** and remains valid when the entire Bot is relocated; near-field parallax and ambiguous visual association still limit accuracy.

## 4. Future motion and optional IMU upgrade path

- Add a **head_motion** actuator adapter with actual measured/estimated joint pose (clearly labeled), mechanical limits and collision/current/thermal protection. Servo command angle is not measured pose.
- Pan/tilt stage: vision error closes the loop on the user's eye midpoint; fast rendered eye movement is complementary. Eyes return near center **only when the head truly aligns**. Do not reuse a stationary-only calibration as a global room orientation.
- Optional head-mounted 6-axis IMU: rate gyro + accelerometer helps fast camera/robot rotation estimates and pitch/roll relative to gravity in appropriate conditions; 6-axis cannot furnish drift-free absolute yaw or position.
- With motion/IMU, use per-observation sensor timestamps and proper transforms between camera, head joints, base and optional local reference. For arbitrary tilt, full 3D rotations and mechanical reachability matter; small-angle additive yaw/pitch isn't enough.
- A two-axis head cannot guarantee correct screen roll or exact gaze after arbitrary 90° tilt. Report unreachable, clamp safely.
- Never force an automatic motor correction while the robot is being picked up without a safe carried/held policy. No lifting/carry detection is available in V1.

## 5. Camera sensor light exposure and life

Ordinary long-duration indoor illumination does not itself burn a CMOS image sensor. Avoid direct sun focused through optics, strong laser light, physical overheating, condensation and environments outside the sensor/board limits. Prolonged operation may consume power and raise device temperature; use camera on demand to reduce unnecessary load, not because normal room light would inherently damage it.

**Separate three states:** (1) stopping camera frames/inference, (2) camera sensor low-power standby, (3) sensor power rail physically off. They are NOT automatically equivalent; implement according to supported hardware driver/power circuitry.

In dim light, sensor gain/exposure/noise and tiny eye crops may make pupil/gaze estimation unusable. Report CAMERA_IMAGE_UNUSABLE or gaze_unknown, not a false person/eye-contact decision. A powered-off camera cannot provide continuous ambient-light observations.

## 6. Screen off and always-on voice

At SLEEPING and NIGHT_DARK:
- Screen/LVGL animation can be suspended; request BSP panel sleep if supported.
- Camera and heavy visual inference are paused.
- **Audio codec and on-device keyword engine must remain active** to detect full 小呆小呆 without Wi-Fi/Mac.
- ESP deep sleep is NOT equivalent to application-level passive sleep. Full wake model needs live audio sampling and processor time.
- Panel sleep is **not proof of full-black physical backlight**. BSP interfaces and actual light leakage may differ; document measured limitation rather than assuming PWM or power gating.
- TTS/audio playback and microphone always-on share codec resources; avoid multiple overlapping driver owners. On robot speech, own-speaker wake false positives need AEC/echo handling or explicit suppression.

Night schedule is a wall-clock/user preference policy when local time is known. A poor camera image is a vision-quality signal, not proof of time-of-day. If a future ambient light sensor exists, it can improve this decision.

## 7. Hardware / firmware integration checkpoints (manual when equipment exists)

1. Confirm exact board revision, populated camera sensor, firmware IDF/BSP/esp_video dependencies and actual camera frame orientation.
2. Record camera intrinsics, lens distortion, camera-to-screen center offset and orientation/mirror flags; include usable distance/eye pixel size.
3. Confirm panel sleep/resume behavior and whether backlight physically turns completely off.
4. Confirm local keyword engine keeps functioning with LCD/vision idle and without network. Provision a **real 小呆小呆 model** first; missing model means not wake-ready.
5. Evaluate thermal load/power and true simultaneous camera + LCD + two-mic audio resources.
6. Future motors: validate joint feedback/limits and safe moved-by-hand behavior. Optional IMU wiring/voltage and shared bus conflicts must be checked separately.

**Current status:** these are design and hardware gates, not completed hardware tests. Project policy is code-first with **no CI** (AGENTS.md).

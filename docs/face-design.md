# Face design v0.1 — minimalist, expressive eyes

## Decision

**2026-11-07:** [EVE theme](eve-themes.md) is now the default, following the user's
[20-pose reference](eveface_design.png). It uses cyan filled silhouettes, scan lines
and a black background without pupils or a mouth. The previous design below remains
the **Classic Outline / 经典线条** family with all five original palette IDs preserved.
Settings now provides a candidate preview and 24-expression navigation before Apply.

**2026-11-07 expressive update:** the shared EVE catalog now offers 24 selectable
poses plus four lifecycle/compatibility poses. Eye contours interpolate and blinks
close and reopen; the daytime idle scheduler occasionally uses short curious,
discovery, smile, wink, laugh, gentle and affection poses between neutral pauses.
Sideways gaze translates the paired silhouettes by up to 92 px at 800 px
screen width without changing either eye's proportions. Both eyes use one
shared gaze vector; the existing animation smooths motion and holds each idle
glance. With a fresh
face box above 20% of the camera image, the automatic
reaction is a centered smiling blink, without conversation or idle glances.
`overjoyed` alone adds white reflected highlights, fading with the blink; it is
not selected by idle, normal happiness or the near-face reaction. It can be
explicitly previewed over USB; meaningful automatic triggers require a future
semantic emotion source. Classic Outline maps it to its nearest happy pose.

Albert.io-inspired *simplicity only*, not imitation of artwork. Two large eyes dominate the 800×480 landscape display; no body, eyelashes, highlights, realistic iris, gradients, nose or permanent mouth. Strokes have round ends. A mouth is permitted only for `surprised` (one dot) or `shy` (one very short curve). No mouth in `happy`.

**Theme means an entire appearance pack**: renderer ID, coherent colors and motion parameters.
Six installed themes live in `shared/themes.json`: EVE (`eve`, default) and the five
Classic Outline palettes (`mint`, `sakura`, `lavender`, `butter`, `sky`). Background,
both eyes, subtitles and settings chrome change together. Both eyes share one color.
`tools/generate_face_themes.py` generates native, browser and Mac catalogs from shared manifests.

## Behavior

**Authority rule (2026-10-30):** bot_runtime owns passive/engaged/waking/sleeping/night behavior; attention_manager owns the active human target; attention alone calculates spatial gaze. Face only renders expression and a single shared gaze vector. Camera face presence or a person's stare does NOT authorize Face to wake/speak. See [behavior-lifecycle.md](behavior-lifecycle.md) and [architecture.md](architecture.md).

- **idle**: symmetrical open rounded eyes; soft tiny pupil motion / occasional quick blink.
- **listening**: eyes attentive, pupil shifts respond to attention angle; no listening badge.
- **happy**: two upside-down U arcs, no mouth.
- **curious**: subtly asymmetric eye sizes/height; two pupil dots.
- **thinking**: one eye open and one narrowed, slight sideways gaze.
- **surprised**: two large open eyes and a *single dot* mouth.
- **sleepy**: two soft horizontal lowered lines.
- **shy**: asymmetry with a tiny, optional curved mouth; no permanent cheek blush.
- **blink**: transient two short lines, normally 130ms.

Interpolation and expression state are part of the Face controller, while *specific eye geometry lives exclusively in replaceable renderer drivers*. Consumers set a high-level expression; they do not directly style pixels. `face_set_gaze()` accepts a single -100…100 normalized vector shared by both pupils. When real tracking is added, an independent Attention coordinator computes this vector from person bearing and actual head orientation, not from two separate eye targets. Network callbacks must call the module only while the BSP display lock is held.

## Screen regions (800×480 reference)

- 0–380px: expressive eye stage. Eye centers around x=260 and 540, y=215.
- 384–464px: subtitle strip. Only reveal an actual subtitle when text is available; reserve the visual region otherwise.
- Top-right: hidden gear revealed on touch, hidden again after five seconds without another tap. Gear opens a full-screen Settings view with first menu **外观 / Appearance**, which opens a dedicated appearance picker. Choose → Apply and return directly to the expression face; Back discards selection. Firmware application persists applied choice to NVS. Network discovery is future work.
- Subtitle text: legible high contrast, max two lines; no scrolling chat bubble; short rolling captions can replace the text as speech proceeds.
- Font warning: ESP-IDF LVGL default font may not contain Chinese glyphs; add/enable CJK LVGL font before using Chinese subtitles on hardware. Preview uses browser fonts.

## Animation budgets and integration

Animation tick ~33ms when display is awake (initial design budget, not board-validated). Blinking only occurs when allowed by runtime behavior state; no normal blinking animation in NIGHT_DARK. Daytime IDLE can use locally generated eye glances, with occasional short DROWSY periods, but it must not initiate speech or target acquisition. When tracking an engaged human, expression changes may change eye shape but never replace the spatial gaze vector with an idle look. Camera-relative tracking may keep the eyes off-center with a fixed head; physical head motion is not available in V1. Avoid GPU-like rendering and large sprites; no audio or motor control inside Face.

Touch behavior: a screen tap reveals the corner gear for five seconds; gear opens Settings, then Appearance, then selection/Apply. Settings is not an always-visible bar. Touching a candidate card is not the same as applying it. Appearance tiles are rendered using the theme's own selected drawing driver. The module should remain callable from remote commands, speech/sentiment module, visual-attention module and motion coordinator.

## Acceptance criteria

1. Theme changes repaint *both* eyes and whole interface together; no split-eye palettes.
2. At rest, **only eyes** are visible, plus a subtitle only when speaking; no permanent mouth.
3. `face_set_expression` and `face_set_theme` can be called by another component.
4. `face_set_subtitle("")` hides the subtitle strip.
5. On-device version targets S31 board with official BSP; preview is browser-only and is not proof of physical hardware functionality.
6. Layout stays usable at 800×480; touch and CJK font are tested on real board in the next integration phase.


## Gaze and passive lifecycle constraints

**V1 fixed camera, no motors/IMU:** a single calibrated, camera-relative gaze vector from attention drives both rendered eyes when a selected engaged human remains visible. If the Bot or the human moves, updated camera observations adjust eyes; if the human leaves camera FOV, eyes cannot physically re-aim the camera. While the target is still off-axis the eyes should not recenter merely because the passage of time or a 'thinking' expression changed the shape. Human looking-at-Bot evidence is separate from where Bot looks.

**Future moving head:** attention integrates measured joint pose and camera calibration, moves eyes promptly and head more slowly, recentering rendered eyes as physical alignment is achieved. Two rendered eyes share one direction; no independently crossing pupils, stale positions or arbitrary model-controlled gaze.

**Passive/sleeping:** eye contact and face presence do not wake the robot. Only the complete local 小呆小呆 wake phrase (or deliberate user action) can initiate engagement. In NIGHT_DARK, Face rendering is suspended and LCD sleep is requested, while local wake audio remains active. Physical full-black backlight is not yet verified.

See [eye-contact.md](eye-contact.md), [behavior-lifecycle.md](behavior-lifecycle.md) and [module-contracts.md](module-contracts.md). Current code contains renderers and a standalone geometric planner only; there is no board-tested live camera or motor integration.

# Face design v0.1 — minimalist, expressive eyes

## Decision

Albert.io-inspired *simplicity only*, not imitation of artwork. Two large eyes dominate the 800×480 landscape display; no body, eyelashes, highlights, realistic iris, gradients, nose or permanent mouth. Strokes have round ends. A mouth is permitted only for `surprised` (one dot) or `shy` (one very short curve). No mouth in `happy`.

**Theme is a complete full-screen palette**: background, both eyes, pupil dots, subtitle treatment, and settings chrome change together. **Never give different colors to the left and right eye.** Five independent themes live in `shared/themes.json`: mint, sakura, lavender, butter, sky. `tools/generate_face_themes.py` generates the LVGL header from this single source.

## Behavior

- **idle**: symmetrical open rounded eyes; soft tiny pupil motion / occasional quick blink.
- **listening**: eyes attentive, pupil shifts respond to attention angle; no listening badge.
- **happy**: two upside-down U arcs, no mouth.
- **curious**: subtly asymmetric eye sizes/height; two pupil dots.
- **thinking**: one eye open and one narrowed, slight sideways gaze.
- **surprised**: two large open eyes and a *single dot* mouth.
- **sleepy**: two soft horizontal lowered lines.
- **shy**: asymmetry with a tiny, optional curved mouth; no permanent cheek blush.
- **blink**: transient two short lines, normally 130ms.

Interpolation is part of the face component. Consumers set a high-level expression; they do not directly style pixels. `face_set_gaze()` accepts -100…100 normalized coordinates and can later be driven by camera-based gaze estimation. Network callbacks must call the module only while the BSP display lock is held.

## Screen regions (800×480 reference)

- 0–380px: expressive eye stage. Eye centers around x=260 and 540, y=215.
- 384–464px: subtitle strip. Only reveal an actual subtitle when text is available; reserve the visual region otherwise.
- Top-right (touch-revealed only): subtle settings button. Opens theme picker; network discovery is a separate future phase.
- Subtitle text: legible high contrast, max two lines; no scrolling chat bubble; short rolling captions can replace the text as speech proceeds.
- Font warning: ESP-IDF LVGL default font may not contain Chinese glyphs; add/enable CJK LVGL font before using Chinese subtitles on hardware. Preview uses browser fonts.

## Animation budgets and integration

Animation tick ~33ms. Occasional blink ~3.5 seconds during idle/listening; pupil gaze interpolation smooth over successive ticks. Avoid GPU-like rendering and large frame sprites. Use simple LVGL shapes to keep RAM/CPU budgets low. No audio or motor control is assumed by the face module.

Touch behavior: a screen tap reveals settings for several seconds; tapping gear opens a lightweight palette selector. This is **not** a persistent top bar. The module should remain callable from remote commands, speech/sentiment module, visual-attention module and motion coordinator.

## Acceptance criteria

1. Theme changes repaint *both* eyes and whole interface together; no split-eye palettes.
2. At rest, **only eyes** are visible, plus a subtitle only when speaking; no permanent mouth.
3. `face_set_expression` and `face_set_theme` can be called by another component.
4. `face_set_subtitle("")` hides the subtitle strip.
5. On-device version targets S31 board with official BSP; preview is browser-only and is not proof of physical hardware functionality.
6. Layout stays usable at 800×480; touch and CJK font are tested on real board in the next integration phase.

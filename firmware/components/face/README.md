# Reusable Face component (LVGL 9)

Independently owns expression state, blinking, gaze interpolation, subtitles and settings navigation. **Does not** import camera, servo, Brain, NVS or Wi-Fi libraries. Public API in `include/face.h`.

```c
face_view_t *face = face_create(lv_screen_active());
face_set_theme(face, "sky");
face_set_expression(face, "listening");
face_set_gaze(face, 45, -20);    // a single direction shared by both pupils
face_set_subtitle(face, "Hello!");
face_reveal_settings(face);
```

## Appearance drivers

`face_renderer.h/.c` defines the abstract driver (`create`/`render`/`destroy`) and registry, independent of `face.c`. Built-ins: `renderer_outline.c` (macaron stroke eyes) and `renderer_pill.c` (solid capsule eyes). Both accept identical expression and gaze state. To install a new eye family, add another renderer and manifest entry; do not modify the controller or Brain protocol. Pack ID, renderer, palette and blink/gaze parameters live in `shared/themes.json`, compiled into `include/face_themes_generated.h`.

## UI and persistence

Default is just eyes (and optional subtitle). Screen tap → gear visible **five seconds** → full-screen Settings → **外观** → choose candidate → **应用** → return to face, repaint entire UI. Back or close does not commit. App registers `face_on_theme_applied()` and saves to NVS; the face component never accesses NVS itself. Firmware `app_main.c` loads last saved appearance on startup.

## Eye contact integration

The face must never receive separate left/right pupil targets. A separate Attention module owns the common target direction and plans combined head yaw/pitch and eye offsets using actual measured/estimated head angles. Rendering styles must respect this shared direction. See `firmware/components/attention/` and `docs/eye-contact.md`.

**Verification status:** Python/JS/native-C tests run in CI; physical S31 firmware build, touch behavior, LCD brightness, performance, exact CJK glyph set, camera and motors still need testing on real hardware. The component calls require LVGL task context or the BSP display lock.

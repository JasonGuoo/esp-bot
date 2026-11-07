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

`face_renderer.h/.c` defines the abstract driver (`create`/`render`/`destroy`) and registry,
independent of `face.c`. Built-ins: `renderer_eve.c` (default cyan silhouettes with scan lines),
`renderer_outline.c` (preserved Classic Outline family) and `renderer_pill.c` (capsule example).
They accept the same expression and gaze state. Packs live in `shared/themes.json`;
EVE's 20 reference poses and four compatibility poses live in `shared/eve-expressions.json`.
Run `python3 tools/generate_face_themes.py` after editing either manifest.
See [EVE implementation and verification](../../../docs/eve-themes.md).

## UI and persistence

Default is EVE's eyes (and optional subtitle). Screen tap → gear visible **five seconds** →
Settings → **外观** → select a candidate and preview its 20 poses with `<` / `>` → **应用** →
return to face. Back or close does not commit. App registers `face_on_theme_applied()` and
saves to NVS; the face component never accesses NVS itself. Firmware `app_main.c` restores
any valid saved ID; new/invalid preferences use the manifest default. Existing users retain
their explicit Classic Outline selection until they choose EVE.

## Runtime UI hooks

`face_on_event()` emits deliberate surface touch, stop, Settings opened and Settings closed. Callbacks run on the LVGL task/under its lock and must only enqueue intent. Main uses a capacity-independent stop latch; the UI does not make behavior decisions. `face_set_stop_visible()` controls the explicit stop button during a local session. `face_set_animation_active(false)` pauses the animation timer behind logical blackout without touching board-level touch hardware.

Added local expressions: `bored`, `drowsy`, `sleeping`, `speaking`. Reapplying an unchanged expression is idempotent, so the 20 ms runtime reconciliation does not continually restart blinking. Speaking's session state is driven by actual audio events; it does not claim phoneme lip sync.

## Eye contact integration

The face must never receive separate left/right pupil targets. A separate Attention module owns the common target direction. The current fixed-head path uses camera-relative observations without a pose measurement; future actuated paths require actual measured/estimated head angles. Rendering styles must respect this shared direction. See `firmware/components/attention/` and `docs/eye-contact.md`.

**Delivery status:** this runtime integration was not compiled, tested or run on hardware, and no CI was added. Hardware is awaiting delivery. Exact CJK glyph coverage, touch and display performance remain unmeasured. Component calls require LVGL task context or the BSP display lock.

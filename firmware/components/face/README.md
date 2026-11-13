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
EVE's 24 preview poses and four compatibility poses live in `shared/eve-expressions.json`.
Run `python3 tools/generate_face_themes.py` after editing either manifest.
See [EVE implementation and verification](../../../docs/eve-themes.md).

## UI and persistence

Default is EVE's eyes (and optional subtitle). Screen tap → gear visible **five seconds** →
Settings → **外观** → select a candidate and preview its 24 poses with `<` / `>` → **应用** →
return to face. Back or close does not commit. App registers `face_on_theme_applied()` and
saves to NVS; the face component never accesses NVS itself. Firmware `app_main.c` restores
any valid saved ID; new/invalid preferences use the manifest default. Existing users retain
their explicit Classic Outline selection until they choose EVE.

## Runtime UI hooks

`face_on_event()` emits deliberate surface touch, stop, Settings opened and Settings closed. Callbacks run on the LVGL task/under its lock and must only enqueue intent. Main uses a capacity-independent stop latch; the UI does not make behavior decisions. `face_set_stop_visible()` controls the explicit stop button during a local session. `face_set_animation_active(false)` pauses the animation timer behind logical blackout without touching board-level touch hardware.

Added local expressions: `bored`, `drowsy`, `sleeping`, `speaking`. Reapplying an unchanged expression is idempotent, so the 20 ms runtime reconciliation does not continually restart blinking. `happy` also uses the existing automatic blink timer, allowing the runtime's close-face smile to blink. Speaking's session state is driven by actual audio events; it does not claim phoneme lip sync.

## Retained result view

`face_set_result_status()` keeps the current task status and a separate image
failure notice above the scrollable answer. Progress, Mac approval prompts and
errors stay visible while an earlier answer remains open. The application supplies
runtime-owned text; Face does not infer task state or overwrite the answer. Notice
labels use fixed view-owned buffers with `lv_label_set_text_static()` so storing
an image memory-failure message requires no new text allocation.

`face_on_result_draw()` records `DRAW_MAIN_END` only for the visible text/image
widget, with a visible-area check and the mounted image source. Main accepts these
callbacks only inside a normal display refresh; a USB snapshot is not a receipt.
The sequence is widget draw → `REFR_READY` → a subsequent
`FLUSH_WAIT_FINISH` → current token/generation check → queued receipt. Main requests
one further refresh to reach the flush wait even for a static result. An image
below the scroll viewport stays unconfirmed until it actually draws. Closing,
replacing or revoking the current result clears pending receipt generations.

This reuses the official [LVGL v9.6.0 draw-event example](https://github.com/lvgl/lvgl/blob/v9.6.0/examples/event/lv_example_event_draw.c),
[visibility API](https://github.com/lvgl/lvgl/blob/v9.6.0/include/lvgl/core/lv_obj_pos.h),
[label API](https://github.com/lvgl/lvgl/blob/v9.6.0/include/lvgl/widgets/lv_label.h) and
[refresh/flush event implementation](https://github.com/lvgl/lvgl/blob/v9.6.0/src/core/lv_refr.c).
`REFR_READY` and `FLUSH_FINISH` alone can precede an asynchronous driver's final
flush completion. The new status layout and receipt fence are source-only changes
as of 2026-11-13; they have not been built or exercised on the board.

## Eye contact integration

The face must never receive separate left/right pupil targets. A separate Attention module owns the common target direction. The current fixed-head path uses camera-relative observations without a pose measurement; future actuated paths require actual measured/estimated head angles. Rendering styles must respect this shared direction. See `firmware/components/attention/` and `docs/eye-contact.md`.

**Delivery status:** the newer EVE expression build was compiled and run on the
connected S31 board; see the [board record](../../../docs/board-bringup-2026-11-06.md)
for exactly what was observed and what remains unverified. Component calls require
LVGL task context or the BSP display lock. No CI was added.

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

Settings also includes **声音**. The separate `face_sound_settings.c` renderer
contains volume (0–60%), mute, response mode, three wake sensitivity presets,
relative night-response volume (10–100%), an explicit local preview and restore
defaults. Content scrolls beneath a fixed back button and title on the 800×480
display. It uses the current theme's palette without importing an eye renderer,
codec, runtime, network client or NVS API.

`face_on_sound_settings_request()` copies/enqueues one proposed
`bot_voice_settings_t`; `face_on_sound_preview_request()` only requests an
explicit preview. Callbacks run under the LVGL lock, return whether work was
queued and must not perform driver or persistence work. Sliders update their
visible number during dragging and submit only on release. Switches and preset
buttons submit once per change. Restore requests the shared defaults; it does
not erase preferences. Preview remains available in text mode, but mute or zero
volume disables it. Runtime retains sole authority for the playback lease and
keeps conversation capture closed while Settings is open.

The app supplies authoritative values with `face_set_sound_settings()`, read-only
metadata with `face_set_sound_diagnostics()` and brief status/error text with
`face_set_sound_notice()`. Identical snapshots do no UI work. A repeated old
settings snapshot does not pull back an in-progress slider or a queued choice.
Opening the page restores the latest authoritative snapshot. Status text is
copied and UTF-8 boundary-truncated at 191 bytes. Its full 30 px Chinese font
supports app-owned notices; fixed labels use the regenerated 22 px font.

Low/standard/high map to native thresholds 90/80/70, respectively. These are
initial product presets, not measured recognition accuracy; a higher sensitivity
can also increase false activations. A different saved threshold is retained and
shown as custom until the person selects a preset. The diagnosis distinguishes
microphone availability, wake-model readiness, conversation capture readiness,
Mac transport readiness and suppression of self-wake during playback/tail cleanup.
It does not describe that suppression as speaker failure or measured AEC.

The controls adapt the official LVGL v9.6.0
[slider event example](https://github.com/lvgl/lvgl/blob/v9.6.0/examples/widgets/slider/lv_example_slider_event.c)
and [switch event example](https://github.com/lvgl/lvgl/blob/v9.6.0/examples/widgets/switch/lv_example_switch_event.c).

**Sound-page verification, 2026-11-13:** actual component/font sources compiled
with host `cc` and LVGL 9.6.0 in a temporary directory. Six 800×480 software-rendered
frames were visually inspected: Settings home, sound controls, sensitivity/night
controls, preview feedback, diagnostics and return navigation. Actual widget-event
checks exercised release-only slider submission, repeated unchanged snapshots,
text-mode preview, mute gating, defaults and back navigation. LVGL resolved all
250 distinct non-ASCII characters in settings/theme/expression strings at 22 px
and all 108 distinct non-ASCII app/runtime strings at 30 px to nonempty glyphs.
This checks host UI behavior and font coverage, not physical touch, sound output,
NVS, S31 memory use or an ESP-IDF firmware build. No CI or permanent test harness
was added.

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

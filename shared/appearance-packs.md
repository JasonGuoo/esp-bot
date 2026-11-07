# Appearance packs (renderer + palette + motion)

Theme is a **pluggable appearance** rather than only a set of colors. Version 1 manifest lives in `shared/themes.json`. Every entry describes:

- Stable `id` used by the saved setting and Brain message, e.g. `mint`.
- User-visible names `name` / `name_zh`.
- `renderer`: a registered eye drawing strategy. `eve` draws cyan filled eyes with scan lines;
  `outline` retains the five Classic Outline palettes; `pill` is a compiled capsule example.
  `default_theme` is `eve`; valid saved choices take precedence.
- Colors: `background`, `eye`, `pupil`, `subtitle_bg`, `subtitle_text`, `chrome`, `chrome_text`.
- `motion`: `blink_interval_ms`, `blink_duration_ms`, `gaze_smoothing`. A future pack may add more expressive presets or asset sequences without editing the Brain protocol.

## Native implementation

```text
face.c                         Expression state + interpolation + subtitle + settings button
face_settings.c                Settings -> Appearance -> pending selection -> Apply
face_renderer.h/.c             Strategy interface + compiled driver registry
renderer_outline.c             Original macaron line eyes, no real eyeball detail
renderer_eve.c                 EVE silhouettes, cyan scan lines, no mouth or pupils
renderer_pill.c                Different geometry, same expression/gaze contract
face_themes_generated.h        Build-time manifest compiled from shared/themes.json
```

A new renderer implements `face_renderer_driver_t` (`create`, `render`, `destroy`) and registers itself in `face_renderer.c`. It receives a full `face_theme_t`, abstract `face_render_state_t`, and display bounds. No renderer calls NVS, network APIs, motor control or the LLM. Register its ID in the manifest generator and provide a matching browser preview driver. The manifest compiler validates supported IDs and numeric ranges; `python3 tools/generate_face_themes.py --check` prevents desynchronization.

## Settings contract

Touch face → temporary gear (5 seconds) → Settings → Appearance → choose a *candidate*
→ preview its expressions with previous/next → Apply → face. The candidate does not change
the live face until Apply. Back/close discards it. The application saves the applied ID
using `face_on_theme_applied()` and NVS; the reusable Face component owns no persistence.
Browser preview mirrors this with localStorage. See [EVE and the 20-pose catalog](../docs/eve-themes.md).

A programmatic `face_set_theme()` is allowed but does not persist, for example a remote command can temporarily override visuals. The next Settings Apply persists the user's explicit choice. If a future app needs a separate temporary overlay on top of a persistent user preference, implement that policy in the app (not renderer).

## Geometry constraints

A renderer can draw different eye outlines, sprites, assets or vector animations, but it must respect **one shared attention vector** for both eyes and report equivalent direction. No independent pupil tracking or simulated crossing. The head/eyes geometry is owned by the Attention module, not by theme packages.

Current screen: 800×480; top/right settings is only transient, subtitles remain in the lower region. Other screen resolutions may require a renderer-specific adaptation. If a theme is removed, the saved ID must safely fall back to default. Theme IDs should be stable across firmware upgrades.

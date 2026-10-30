# esp-bot

A stationary, expressive desktop companion: **ESP32-S31-Korvo-1** provides the eyes, touch, camera, microphone and speaker; a **macOS/Linux Brain** provides LLM reasoning and future tools.

> Hardware target is **ESP32-S31-Korvo-1 V1.1** (ESP32-S31), **not** ESP32-S3-Korvo-1. The S31 board has an 800×480 RGB display and GT1151 touch panel. Use ESP-IDF 6.1 / `esp32s31`, not `esp32s3`.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `firmware/` | ESP-IDF device program; official S31 Korvo-1 BSP, reusable LVGL face component, Wi-Fi Brain client |
| `firmware/components/face/` | Reusable expression controller, pluggable eye renderers, subtitles and Settings -> Appearance UI |
| `firmware/components/attention/` | Independent eye-contact geometry: shared gaze direction, head setpoints and confidence gating |
| `firmware/components/vision_attention/` | On-board-ready multi-face tracking core with separate per-person eye positions, gaze events, fixation times and provisional pupil localization; not camera-integrated yet |
| `server/` | Cross-platform Python Brain server (macOS/Linux), command queue, optional OpenAI-compatible LLM and safe tools |
| `shared/` | Canonical theme tokens and documented JSON/HTTP contract |
| `preview/` | Browser-based interactive face preview; no board required |
| `tools/` | Generate firmware color header from the shared theme JSON |
| `docs/` | Design rules, integration and staged roadmap |

## Theme gallery

![Five cohesive macaron themes — Mint, Sakura, Lavender, Butter, Sky](docs/theme-gallery.svg)

## Quick start

### Face preview (first)

From the repository root:

```sh
python3 -m http.server 8765 --bind 127.0.0.1
# open http://127.0.0.1:8765/preview/
```

Touch the simulated screen to show the corner gear (auto-hides after five seconds). Open **Settings → Appearance**, select one of the five installed pastel appearances (mint/sakura/lavender/butter/sky), then press **Apply** to return to the face. Selection does nothing until Apply, and the preview persists the choice in localStorage. All five shipped styles share an outline renderer; future appearance packs can choose entirely different eye geometries, assets and motion profiles. Use the debug panels below the screen to test expressions, gaze direction and subtitles.

### Brain server (Python 3.10+)

```sh
cd server
python3 -m espbot
# http://127.0.0.1:8766/health
```

No third-party Python dependencies. Local-only by default. For ESP32 Wi-Fi access, set `ESPBOT_BIND=0.0.0.0` **and** `ESPBOT_TOKEN` to a random secret; see [server/README.md](server/README.md). Do not expose this plain HTTP prototype directly to the internet.

### Firmware (S31 board)

Use Espressif **ESP-IDF 6.1+ with ESP32-S31 support**, then:

```sh
cd firmware
idf.py set-target esp32s31
idf.py menuconfig  # ESP Bot: optional Wi-Fi, Brain URL and shared token
idf.py build
idf.py -p /dev/ttyACM0 flash monitor
```

The face can run without a Brain connection. The ESP-IDF build and real-device testing still need to be performed on the actual board; these steps are **not** claimed as hardware-validated.

## Face module API

```c
face_view_t *face = face_create(lv_screen_active());
face_set_theme(face, "lavender");
face_set_expression(face, "curious");
face_set_gaze(face, 35, -20);        // normalized -100..100
face_set_subtitle(face, "Hello!");
```

One face API, multiple appearance packs. A pack selects a renderer, colors and motion timings (not just different colors in each eye). The installed macaron packs use matching colors for both eyes; the future alternative pill renderer is already implemented. The only optional mouth marks are a surprise dot and a tiny shy curve. The gear is touch-revealed for five seconds, then opens **Settings → Appearance → Apply**. Applied choices are persisted to ESP32 NVS by the application, not by the face component.

## Roadmap and boundaries

**In this baseline:** LVGL Face component with independent renderers and five installed macaron appearances; two-level Settings → Appearance → Apply navigation with NVS persistence; touch-revealed gear; blinking, shared pupil gaze and subtitle area; a host-testable Attention coordinator that calculates coupled head/gaze targets and refuses false mutual-eye-contact claims; Python Brain HTTP command queues, optional text-model adapter and allowlisted tool registry; optional ESP32 Wi-Fi polling.

**Vision core now available separately:** [multi-person visual attention specification](docs/visual-attention.md), [vision_attention C component](firmware/components/vision_attention/README.md). Multi-face tracking and candidate iris localization are implemented as independent cores; ESP-DL camera capture and genuine calibrated gaze recognition are still pending.

**Later:** full ESP-IDF and real-board build/flash verification; camera face detection and a *separate* classifier for whether the person looks at the robot; 3D camera-to-display/head calibration; servo feedback and pan/tilt actuation; speech streaming, TTS, Wi-Fi discovery, pairing and encryption. The current Attention module is a geometric **planner**, not a working camera or motor driver.

Read [face design](docs/face-design.md), [appearance pack contract](shared/appearance-packs.md), [human eye-contact architecture](docs/eye-contact.md), and [device/Brain protocol](shared/protocol.md) before integrating new components.

## Validation

GitHub Actions run Python Brain/palette checks, Node tests for the settings state machine and pluggable browser renderers, plus native C tests for attention geometry. No board-flash/servo/camera validation is claimed.

## References

- [Espressif ESP32-S31-Korvo-1 BSP](https://github.com/espressif/esp-bsp/tree/master/bsp/esp32_s31_korvo_1)
- [ESP32-S31 Korvo-1 board reference](https://docs.espressif.com/projects/esp-board-manager/en/latest/references/boards/esp32_s31_korvo_1.html)
- [ESP-IDF ESP32-S31 getting started](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32s31/get-started/index.html)
- [Albert.io mascot reference](https://dribbble.com/shots/26730038-Brand-Mascot-Design-for-Albert-io) — used solely as a high-level *simplicity* reference, not copied assets.

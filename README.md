# esp-bot

A stationary, expressive desktop companion: **ESP32-S31-Korvo-1** provides the eyes, touch, camera, microphone and speaker; a **macOS/Linux Brain** provides LLM reasoning and future tools.

> Hardware target is **ESP32-S31-Korvo-1 V1.1** (ESP32-S31), **not** ESP32-S3-Korvo-1. The S31 board has an 800×480 RGB display and GT1151 touch panel. Use ESP-IDF 6.1 / `esp32s31`, not `esp32s3`.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `firmware/` | ESP-IDF device program; official S31 Korvo-1 BSP, reusable LVGL face component, Wi-Fi Brain client |
| `firmware/components/face/` | Standalone face API: expressions, animation, gaze, subtitles, touch settings, themes |
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

Select any of five **independent full-screen themes** (mint/sakura/lavender/butter/sky). **Both eyes always use the same theme.** Change expressions, move the gaze and edit subtitle text in the preview.

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

One face, multiple palettes; **not** a different color in each eye. The only optional mouth marks are a surprise dot and a brief shy curve. No iris, gradients, highlights or other facial clutter. The settings icon appears when the screen is touched.

## Roadmap and boundaries

**In this baseline:** real LVGL face module, 5 themes, gaze/blink, touch theme selector, subtitle area; a Python HTTP Brain with device command queue, configurable OpenAI-compatible text model, an allowlisted tool registry, and an optional ESP32 polling client.

**Later:** board-validated full input/audio pipeline, camera-based attention, Wi-Fi discovery/mDNS and BLE pairing, microphone streaming, TTS playback and synchronized head-motor control, secure pairing and TLS. None of these are represented as already working.

Read [docs/face-design.md](docs/face-design.md) and [shared/protocol.md](shared/protocol.md) before integrating new components.

## References

- [Espressif ESP32-S31-Korvo-1 BSP](https://github.com/espressif/esp-bsp/tree/master/bsp/esp32_s31_korvo_1)
- [ESP32-S31 Korvo-1 board reference](https://docs.espressif.com/projects/esp-board-manager/en/latest/references/boards/esp32_s31_korvo_1.html)
- [ESP-IDF ESP32-S31 getting started](https://docs.espressif.com/projects/esp-idf/en/v6.1/esp32s31/get-started/index.html)
- [Albert.io mascot reference](https://dribbble.com/shots/26730038-Brand-Mascot-Design-for-Albert-io) — used solely as a high-level *simplicity* reference, not copied assets.

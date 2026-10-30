# Architecture v0.1: device + Brain

The software is split by trust boundary and compute budget, not just by directory.

**ESP32-S31-Korvo-1** (800x480, touch, microphones, speaker, camera):
- runs the face renderer locally through the official Espressif BSP and LVGL
- listens for local interactions and will eventually detect human presence/attention
- runs a lightweight Wi-Fi polling client (no cloud keys or LLM model on the board)
- holds an appearance-pack ID (renderer + colors + animation parameters) and abstract expression state
- runs an independent geometry planner for **shared pupil direction and pan/tilt setpoints**; no physical motor control yet
- future: microphone streaming, speaker playback, servo pan/tilt, camera attention events, BLE provisioning

**Mac/Linux Brain** (Python):
- maintains the per-device command queue and text/visual state contract
- later handles speech transcription, LLM conversation state, TTS, and tools
- can use an operator-selected OpenAI-compatible local or remote model
- enforces a limited tool registry; no arbitrary model-generated code execution
- future: discovery, pairing, encrypted transport, tool authorization, streaming media

Data flow:

    user speech / camera ---- future device media layer ----> Brain
                                                       (ASR / LLM / tools / TTS)
                                                                   |
                                                                   v
    Brain HTTP API ---- queued face.set / gaze / subtitle ----> Wi-Fi client
                                                                   |
                                                                   v
                                                            reusable face API
                                                                   |
                                                                   v
                                                       LVGL / official S31 BSP
                                                                   |
                                                              800x480 display

## Module ownership

The facial expression component has **no dependence** on networking, AI, audio, or motor control. Its API is defined in firmware/components/face/include/face.h.

- Display subsystem owns the LVGL lock.
- Future Vision module reports a **person bearing from the moving camera**, face-confidence, track ID and separately estimated whether the user looks back at the bot. A face box alone is not proof of mutual gaze.
- Attention coordinator (already implemented in `firmware/components/attention`) combines the visual target and measured head orientation into one eye-gaze vector and safe head motor setpoints; no independent left/right pupil steering.
- Future servo driver owns actual yaw/pitch motion and feedback, independently of Face and Vision. No motor GPIO wiring is assumed in current code.
- Future voice session manager issues face_set_expression(face,"listening") and face_set_subtitle(face,caption).
- Future conversation agent on the Brain issues a face.set JSON command through the device queue. Real attention tracking must use the attention coordinator rather than independently setting pupils or servo setpoints.
- Face controller routes expression updates through a renderer strategy registry. Current pack uses rounded-outline eyes; an alternate pill-eye renderer demonstrates shape replacement.
- Settings UI owns navigation and tentative appearance selection; the application owns nonvolatile storage. Renderers never own settings or NVS.
- Future motor coordinator consumes a different command family; never stuff servo logic into face.c.

The protocol is versioned in shared/protocol.md and the shared source of appearance metadata (renderer, colors, motion) is shared/themes.json. tools/generate_face_themes.py produces the firmware palette header. Browser preview is a visual simulator, not a replacement for hardware validation.

## Delivery stages

1. **Current:** renderer-aware appearance packs, reusable expression controller, Settings → Appearance → Apply, touch gear and NVS persistence, decoupled preview, face/gaze/head geometry planner, Wi-Fi command polling, Brain API, provider adapter, restricted tool registry, Python/Node/native-C tests.
2. **Next hardware gate:** build against ESP-IDF 6.1+ and official S31 BSP; flash; verify RGB timing, touch orientation, available heap and frame-rate; tune eyes to actual 4.3-inch screen; confirm Chinese glyphs and subtitle wrapping.
3. **Connectivity:** Wi-Fi scan + Brain discovery, BLE onboarding and pairing, retained device identity, reliable command delivery, encryption and updates.
4. **Speech:** local audio recording, Brain ASR/LLM/TTS pipeline, streaming captions, synced expression and lipless listening/talking states.
5. **Attention + motion integration:** actual camera face tracking, human-look-at-bot classifier, target arbitration and hysteresis, pose feedback, camera/face boresight calibration, pan/tilt servo driver and physical safety testing. The *planner* already exists but is not wired to sensors or motors.
6. **Tools:** permissioned tool calls, auditing, user confirmation and integration with Mac/Linux services.

## Risks and hard limits

- ESP32-S31 is **not** ESP32-S3; do not transplant its BSP, GPIO assignments or binary firmware.
- ESP-IDF >=6.1 needed. JSON is an external espressif/cjson dependency in IDF 6; LVGL 9 is explicitly selected.
- This version only *queues commands*; there are no delivery acknowledgements, retry queues or persistence.
- Current LAN HTTP carries subtitles and authentication token without encryption; use on trusted development LAN only.
- An LLM response may be created in the Brain when configured, but microphone streaming, automatic model tool use and TTS audio have not been implemented.


## Physical eye contact and replaceable eye renderers

Refer to [eye-contact.md](eye-contact.md) for the coordinate frame, residual-gaze model, uncertainty gates and test plan. Each renderer receives one shared gaze vector, while Attention owns person tracking and head/eye coordination; swapping theme never changes physical tracking policy. Refer to [appearance-packs.md](../shared/appearance-packs.md) for the native driver interface, theme manifest, motion profiles and renderer registration.

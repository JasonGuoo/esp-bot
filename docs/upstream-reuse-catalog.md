# Reuse-first upstream catalog — 小呆 (2026-10-30)

> Decision: **stop reinventing existing hardware/software subsystems**. Default to a proven upstream driver, protocol, media engine, voice pipeline, or complete product mechanism before writing a new implementation. This is the permanent upstream research register: preserve links, what to borrow, integration boundaries and license caveats. **Listing is not a claim that the dependency has been integrated or tested on our board.**

Canonical system decision: [local-first-hybrid-brain.md](local-first-hybrid-brain.md). Hardware target: ESP32-S31-Korvo-1 **V1.1**, not S3; primary AI computer: user's **Mac M4 Pro / 48 GB unified memory** with Rust/Tauri **bot-server** as product entry point.

## 0. Discovery that changes implementation priority

**XiaoZhi has an actual ESP32-S31-Korvo-1 V1.1 board port.** Prioritize a line-by-line reuse/adaptation assessment of its S31 board-specific startup, ES8389 clocking, dual NS4150B speaker amplifiers, twin microphone acquisition, 800×480 RGB + GT1151, OV3660 camera orientation, Wi-Fi onboarding, Opus audio and wake workflow. **Do not substitute ESP32-S3 board pin maps.** The port specifically notes that ES8389 recovers its clock from BCLK (`use_mclk=false`), and cites GPIO7 for shared speaker PA enable.

- [XiaoZhi exact S31 Korvo-1 port documentation](https://github.com/78/xiaozhi-esp32/blob/main/main/boards/espressif/esp32-s31-korvo-1/README.md)
- [XiaoZhi S31 board directory](https://github.com/78/xiaozhi-esp32/tree/main/main/boards/espressif/esp32-s31-korvo-1)
- [XiaoZhi firmware](https://github.com/78/xiaozhi-esp32) — MIT license ([LICENSE](https://github.com/78/xiaozhi-esp32/blob/main/LICENSE)).
- [Espressif official S31 BSP](https://github.com/espressif/esp-bsp/blob/master/bsp/esp32_s31_korvo_1/README.md)
- [Espressif S31 V1.1 board user guide, including speaker connectors and assembly](https://docs.espressif.com/projects/esp-dev-kits/en/latest/esp32s31/esp32-s31-korvo-1/user_guide.html)

**Decision:** upstream port is the primary reference for device peripherals; preserve our unique Face UX, passive lifecycle, complete "小呆小呆" wake requirement, attention target arbitration, permissions and appearance designs.

## 1. Prioritized reuse matrix

| Upstream | Proven functionality / why it matters | Proposed reuse | Decision / license caution |
| --- | --- | --- | --- |
| [78/xiaozhi-esp32](https://github.com/78/xiaozhi-esp32) | ESP32 voice robot; Opus/WS or MQTT+UDP, MCP commands; **exact S31 Korvo board adaptation** | Reuse/audit board audio/display/camera/network drivers, stream lifecycle and tool patterns; keep custom UI/runtime | **P0** source/adaptation candidate. MIT source license; any included third-party components/assets have separate terms. Do not wholesale fork whole product. |
| [xinnan-tech/xiaozhi-esp32-server](https://github.com/xinnan-tech/xiaozhi-esp32-server) | Self-hosted XiaoZhi backend; WS, MQTT+UDP, LLM, knowledge/MCP | Learn how the protocol routes media and tools; borrow reusable pieces where sensible | **P1** interoperability/reference, **not** replacement for Rust/Tauri bot-server. MIT ([license](https://github.com/xinnan-tech/xiaozhi-esp32-server/blob/main/LICENSE)). |
| [huggingface/speech-to-speech](https://github.com/huggingface/speech-to-speech) | VAD→STT→LLM→TTS and real-time WS; MLX LLM + MLX audio paths on Apple Silicon; local/cloud backend adapters | **Preferred initial Mac speech service** run as a replaceable local process, not hand-reimplement VAD/STT/TTS | **P0** Mac integration candidate. Apache-2.0 code license ([LICENSE](https://github.com/huggingface/speech-to-speech/blob/main/LICENSE)); each model/voice/weights has its own license. |
| [Hugging Face Reachy Mini local conversation](https://huggingface.co/blog/local-reachy-mini-conversation) | Runs voice intelligence on nearby laptop; realtime-compatible socket independent of robot client | Copy **architectural pattern**: robot thin client, local MLX brain, switchable provider | **P0** reference. [Reachy Mini SDK](https://github.com/pollen-robotics/reachy_mini) Apache-2.0; **its hardware design assets are CC BY-SA-NC** — not suitable to casually copy for a commercial physical shell. |
| [Home Assistant Assist + Wyoming](https://www.home-assistant.io/integrations/wyoming) / [OHF-Voice/wyoming](https://github.com/OHF-Voice/wyoming) | Local modular speech-to-phrase, Whisper, Piper, wake and binary-audio events | Optional **adapter** for local ASR/TTS processes; inspect event+binary framing, health/service discovery | **P1** adapter/reference. Wyoming on trusted LAN has no built-in auth/encryption; do not expose untrusted. Validate exact versions and licenses. |
| [OpenVoiceOS message bus](https://github.com/OpenVoiceOS/ovos-messagebus) | WS publish/subscribe skill components and service separation | Message-envelope design and command/topic naming patterns | **P2** architecture reference; do not add an extra device broker or duplicate bot_bus. Apache-2.0 for messagebus. |
| [Espressif esp-bsp S31](https://github.com/espressif/esp-bsp/blob/master/bsp/esp32_s31_korvo_1/README.md) | Board LCD, GT1151, ES8389, camera and speaker reference drivers | **Primary official board abstraction** | **P0**, compatible with S31 IDF target. Pin/resource ownership must stay in bot_platform/audio owner. |
| [Espressif ESP-SR](https://github.com/espressif/esp-sr) | S31 WakeNet/AFE/MultiNet, VAD, echo cancellation | **P0** on-device wake "小呆小呆" (requires a real trained/provisioned four-syllable model), optional simple offline commands | Firmware library/models have separate redistribution/weight/licensing requirements; NEVER claim the wake model already exists. |
| [Espressif esp-gmf](https://github.com/espressif/esp-gmf) / [gmf examples](https://github.com/espressif/esp-gmf/blob/main/gmf_examples/README.md) | Audio record/playback, AEC, simple player, JPEG/MJPEG, video renderer, BT audio examples | **P0** prefer standard audio/AV pipeline to new custom player; evaluate supported S31 combinations | Verify example board/target compatibility and component versions before copying. |
| [Espressif esp_codec_dev](https://github.com/espressif/esp-audio-dev/blob/main/esp_codec_dev/README.md) | Standard codec ownership; ES8389 playback/capture | Use alongside BSP/GMF; avoid writing another ES8389 driver | **P0**. Version 1.x→2.x interface changed; pin exact compatible versions. |
| [han0519/esp32-s31-vocat](https://github.com/han0519/esp32-s31-vocat) | S31 pet: round LVGL face, camera/MJPEG, local wake, SD/online media, cloud chat | S31-specific implementation samples for image/media scenes, firmware UX and audio concurrency | **P1** reference: *different board* and sensor/pin layout. Application source Apache-2.0 per README; dependencies have upstream licenses. |
| [M5Stack StackChan](https://github.com/m5stack/StackChan) / [product](https://docs.m5stack.com/en/StackChan) | Compact cute desktop bot with camera, dual servos, mic/speaker and stand | **Mechanical/UX reference** for attractive pan/tilt body and accessible speaker acoustics | **P1** hardware reference, **not** mechanically compatible with Korvo display. Review CAD/license before reuse. |
| [Pollen Reachy Mini](https://github.com/pollen-robotics/reachy_mini) | Mature miniature robot with motion SDK and a local daemon | Head-movement mechanics, calibration and device/daemon separation | **P1** reference. Hardware CC BY-SA-NC; don't incorporate shell files for a commercial product without appropriate rights. |

## 2. Mac M4 Pro 48 GB software selection

**Selected product host: Rust + Tauri bot-server**. It owns discovery, pairing, device/session registry, permissioned tools, service health, local-first provider routing, media control, Mac UI and logs. A proven Python or other process may provide STT/LLM/TTS behind loopback HTTP/WS; **Rust does not need to reimplement inference engines or model decoders**.

**Initial experiment, no final model lock-in:**

- Speech pipeline: HF Speech-to-Speech, preferably MLX on Apple Silicon; compare warm-idle latency and real streaming/barge-in. See [Mac/local instructions](https://github.com/huggingface/speech-to-speech).
- STT: project's supported Parakeet/Whisper-MLX/Qwen3-ASR adapters, or independently hosted Wyoming Whisper; test actual Chinese wake-free dialogue and languages on M4 Pro.
- LLM: an MLX quantized 4B–9B instruct model as *initial candidate*; upgrade only when quality/latency justify it. 48 GB does not imply unlimited simultaneous model residence; measure complete system memory.
- TTS: Qwen3-TTS via the project's MLX audio backend and simpler Piper/Kokoro alternatives; benchmark first-byte speech, Mandarin intelligibility and multi-stream echo.
- Internet search/tools: **on Bot Server only by default**, with explicit opt-in; local ASR/LLM/TTS do **not** mean the Mac itself must be disconnected from the internet. Search tool content is untrusted.
- Optional visual intelligence: local MLX-VLM server behind capability adapter; defer until actual need and memory budget known.

**Engineering policy:** warm model pool for frequently used local providers, streaming text→speech output, cancellation/barge-in propagation, latency histograms and cloud-invocation counters. No invented latency or no-cost claims: compute electricity, external search APIs and licensing still cost resources.

## 3. What not to reinvent / what we do keep

**Reuse** board BSP and XiaoZhi S31 adaptations, ESP-SR, audio codecs/GMF, existing realtime speech process, std Apple MLX tooling, standardized BLE provisioning, off-the-shelf servos/brackets, and officially released enclosure CAD whenever compatible.

**Own** the product experience (passive "小呆" behavior), attention/gaze selection, Face theme UI, a narrow transport adapter, local-first service policy, pairing/permissions, Tauri console and hardware-safe motor adapter. ESP runtime still owns wake and physical control; LLM never sends 20–50 Hz raw servo or gaze commands.

**Do NOT** fork and maintain an entire new audio codec, Bluetooth implementation, ASR/TTS framework, motor control stack, custom general-purpose event broker, or incompatible 3D model if an upstream implementation covers it.

### Provenance, legal and change-control checklist

1. Prefer an **adapter wrapping an upstream library** to copy/paste. Record upstream URL, revision/tag, dependent models and codec/patent constraints.
2. Verify **all** software/assets/weights licenses separately (SDK license != model weights != hardware CAD license).
3. Keep source NOTICE / attribution required by each license.
4. Test board **revision**, pin allocation and codec sample rates before importing a driver.
5. Document which features are **implemented, planned, validated on real hardware**, not just advertised.
6. Treat this catalog as a maintained decision record; review upstream versions before each adoption, without replacing canonical architecture.

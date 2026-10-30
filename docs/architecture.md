# ESP Bot system architecture V1 — 小呆 (2026-10-30)

**This is the canonical overall architecture.** Older descriptions of the first Python Brain/Face demo are historical implementation snapshots, not competing design authorities. For event schemas use [module-contracts.md](module-contracts.md); for passive UX use [behavior-lifecycle.md](behavior-lifecycle.md); for concrete sensor/actuator boundaries use [eye-contact.md](eye-contact.md) and [hardware-power.md](hardware-power.md).

## 1. Product / hardware decisions

- Name: **小呆**; ONLY full spoken keyword **小呆小呆** wakes from passive behavior. Single 小呆, mere face presence, eye contact and arbitrary human chatter do not initiate interaction. [wake-word.md](wake-word.md); [identity manifest](../shared/bot-identity.json).
- Personality: quiet passive companion. IDLE → BORED → DROWSY → SLEEPING, NIGHT_DARK when appropriate. Never give unsolicited greetings. Local wake audio must remain available with display/camera off.
- V1 physical configuration: **ESP32-S31-Korvo-1 V1.1; stationary non-actuated head** with board camera, two microphones, touch LCD, speaker; user may move or rotate entire Bot. No installed IMU, motor, absolute compass or pose feedback.
- **Vision-first:** when engaged, perceive multiple faces, retain one officially selected human, align **both displayed eyes** with human eye midpoint using the current camera-relative observation. No physical re-aiming of camera is possible in V1.
- Future: pan/tilt hardware and optional 6-axis IMU replace/extend adapters; one gaze controller and one target selector remain owners. No promise of arbitrary-orientation stabilization by an unactuated camera.
- **Mac/Linux target server:** Rust + Tauri Bot Server handles connection/UI and eventually ASR/LLM/TTS/tools. Device must wake/animate and follow a visually present target without depending on server availability.
- **No CI:** deliver modular code without artificial validation infrastructure when hardware/environment unavailable; never claim unverified features are working.

## 2. Authority: local behavior master, bus and server

**bot_runtime is the ONLY local behavior controller.** It owns mode changes, device capabilities, permissions, conversation state and arbitration of touch/server/autonomous expression requests. It does NOT run model inference or calculate pupil position on every frame.

**bot_bus is a communication mechanism, NOT a controller.** It transports bounded events, commands and replaceable latest snapshots. Continuous camera/audio media use separate owned buffers. The module producing a datum does not automatically acquire authority to act on it.

**bot_platform** is the common board support/resource layer: display/camera/audio codec/touch ownership, monotonic clock, NVS, low-level power/health. It is a supporting service, NOT a second behavioral controller.

**Bot Server** is a remote semantic/service executor. Remote LLM cannot directly own real-time eye or motor control, and an offline Mac cannot prevent local name-wake.

~~~text
                                     Mac/Linux
                              Rust + Tauri Bot Server
                           gateway | ASR/LLM/TTS | tools
                                        |
                                brain_link / lan_link
                                        |
                     ┌──────────────────▼──────────────────┐
                     │ bot_runtime — BEHAVIOR AUTHORITY     │
                     │ passive state, conversation, policy │
                     └──────────────────┬──────────────────┘
                                        │ commands / permissions
                     ┌──────────────────▼──────────────────┐
                     │ bot_bus — events / latest snapshots  │
                     └──────────────────┬──────────────────┘
                                        │
           camera → vision_attention ───┤       audio ← dual microphones
             (all faces/eye evidence)   │           (full keyword + future DOA)
                          \             │            /
                           \            ▼           /
                            └──── attention_manager ┘
                                (one human focus)
                                       |
                                       ▼
                                  attention
                        (one spatial gaze controller)
                             /                    \
                           gaze                optional joint plan
                             |                      |
                            face               head_motion
                        LVGL display       V1: stationary / future: motors

                     bot_platform under all hardware-facing code
~~~

In a running implementation bot_runtime manages lifecycle and priorities, **not every movement sample**. The perception→focus→attention→Face chain must update locally without server RTT or a new top-level policy decision every frame.

## 3. Eight functional modules

| # | Module | Owns | Does NOT own |
|---|---|---|---|
| 1 | **vision_attention** | All face/eye tracks, eye ROIs, gaze evidence quality, per-person fixation duration, capture timestamps | Official conversation target; screen expression; motors |
| 2 | **audio** | One synchronized mic/codec input, local name wake, session audio/TTS, optional acoustic DOA | Behavior state; choosing a person; direct Face changes |
| 3 | **attention_manager** | Exactly one official active focus, focus continuity, audio-visual arbitration and switching | Vision inference or motor control |
| 4 | **attention** | Calibrated camera-relative target → common rendered eye vector; future coordinated head/eyes plan | Determining whether speech is a wake; target selection |
| 5 | **face** | LVGL eye rendering, expression, subtitles, themes and Settings touch UI | Camera acquisition; deciding to wake; overriding tracking gaze |
| 6 | **head_motion** | V1 stationary capability adapter; future joint commands, feedback, limits and safety | Fake pose feedback; who to look at |
| 7 | **brain_link** | Secure session intent/transport, network/media routing, reconnection | Local wake, gaze loop, direct UI/servo writes |
| 8 | **bot_runtime** | Lifecycle, passive policy, conversation mode, permissions and command arbitration | Camera/ML inference or each frame's gaze geometry |

**Supporting infrastructure:** bot_bus and bot_platform. They should not be counted as additional independent brains. Names indicate modules, not thread counts.

## 4. Interaction state and autonomy

- bot_runtime has passive mode IDLE, BORED, DROWSY, SLEEPING, NIGHT_DARK, WAKING, ENGAGED. LISTENING/THINKING/SPEAKING are separate conversation substates; visual focus may be TRACKING, TEMPORARILY_LOST or REACQUIRING independently.
- *No engagement merely because a person is visible or looking toward Bot*. Human gaze is perception, not permission to initiate conversation.
- Full **小呆小呆** locally recognized or a deliberate user touch/manual action authorizes engagement. During an active session the name need not be repeated each sentence.
- Night screen-off is application-level low-activity mode: keyword/audio path stays alive, camera/inference stops, display sleep requested. Actual full physical blackout on current board is not yet proven.
- Pre-wake ambient audio is local/transient; no normal audio upload or persistent recording until authorized interactive session.
- If keyword model is missing, surface WAKE_MODEL_MISSING; **the model for 小呆小呆 is not provisioned**.

Details and suggested tuning times: [behavior-lifecycle.md](behavior-lifecycle.md), [wake-word.md](wake-word.md).

## 5. Visual tracking, no IMU, no active motor

- **Robot head physically stationary relative to its base; the entire object can be relocated or rotated.** A valid rigid camera→screen calibration moves with the robot.
- New camera frame provides human eye midpoint relative to the now-moved camera. As long as target remains in FOV, controller updates displayed eyes. In this V1 mode **eyes do not recenter because head is not turning**.
- When target leaves FOV, only conversational focus can be held temporarily; don't invent accurate gaze or a physical camera reorientation.
- The camera and display are not necessarily coaxial; calibration/near-field parallax matter. The iris/pupil evidence that **human looks at Bot** differs from the spatial point that **Bot looks toward**.
- Multi-person vision observes everyone, attention_manager selects one. Temporary unknown gaze does not mean look-away and cannot by itself drop active engagement.
- Future pan/tilt: eyes move first and head starts turning; as actual head pose aligns, eyes return to center. Future optional IMU improves quick body-motion estimation but cannot be a required dependency of V1. See [eye-contact.md](eye-contact.md), [visual-attention.md](visual-attention.md).

## 6. Audio and wake, direction

The board's two-mic array supports a referenced Espressif DOA example; it gives a directional clue with front/back ambiguity and is **not a speaker ID**. It cannot mechanically move a stationary camera. The local always-on keyword engine must recognize **complete 小呆小呆**, and local wake must never depend on DOA or server ASR.

Audio module owns a shared microphone stream; wake keyword and optional DOA subscribe without opening competing codec devices. Voice activity alone never authorizes engagement. See [audio-direction.md](audio-direction.md).

## 7. Deployment and security boundaries

**On device:** ESP-IDF for S31, display/face, audio wake pipeline (to implement), local policy, vision and gaze coordination (integration pending), device discovery/link.

**On Mac/Linux:** Rust/Tauri bot-server is the selected server, currently only a LAN discovery/WebSocket session console skeleton. The older Python server and firmware HTTP polling client exist as isolated prototype functionality. Neither is equivalent to a completed LLM/audio service on Rust/Tauri.

**LAN:** mDNS discovery is not authenticated pairing. Existing WS hello/welcome is advisory, not cryptographically authenticated; no privileged remote control or private media should be sent until connection authentication and encryption are in place. See [lan-discovery.md](lan-discovery.md) and [shared/lan-discovery-v1.md](../shared/lan-discovery-v1.md). [shared/protocol.md](../shared/protocol.md) describes legacy Python HTTP polling, NOT the selected final wire protocol.

## 8. Code and integration status

Already present in repo (not necessarily exercised on physical S31):
- Reusable Face rendering, appearance/theme selection, preview.
- Standalone C attention/head-gaze *planner*.
- Standalone C vision_attention multi-person temporal tracker and dark-eye *candidate* locator.
- Standalone firmware lan_link, Rust/Tauri discovery console skeleton.
- Older Python Brain and HTTP polling prototype.

**Not yet wired/implemented on the board**:
- Real esp_video camera and ESP-DL visual inference adapter; validated iris/eye-contact estimator.
- Local continuous wake engine + **小呆小呆** model, audio capture/DOA and audio session routing.
- bot_runtime, bot_bus, attention_manager orchestration and platform-managed night camera/LCD sleep.
- Mechanical pan/tilt hardware, head_motion driver, IMU.
- End-to-end Rust/Tauri ASR/LLM/TTS/tool execution and authenticated media link.

Use [implementation-roadmap.md](implementation-roadmap.md) to sequence work. **Code-first, no CI.** Do not confuse a design specification with flashed or hardware-validated behavior.

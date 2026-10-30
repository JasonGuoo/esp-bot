# Two-microphone direction of arrival (DOA) — optional acoustic cue

**Target board:** ESP32-S31-Korvo-1 V1.1, on-board 2-mic array and codec. **Status:** architecture/design only; no DOA/audio driver currently integrated in esp-bot main. **Hard requirement:** always-on local wake for full phrase 小呆小呆, specified in [wake-word.md](wake-word.md). DOA is optional and NEVER substitutes for wake recognition.

## Confirmed reference path

Espressif provides a live ESP32-S31-Korvo-1 example under [esp-apa DOA live_example](https://github.com/espressif/esp-apa/tree/main/components/esp_apa_doa/examples/live_example). Its source configures:
- Two synchronized microphone channels, **16 kHz** sample rate and **16-bit** PCM.
- Physical on-board mic spacing macro **0.043 m (43 mm)**.
- Board codec capture via esp_codec_dev_read and spatial processing via esp_apa_doa_process.
- A **0..180 degree** two-mic result convention in that example: **0 left / 90 front / 180 right**, with valid flag and quality.
- Device front-facing signed yaw (positive robot-right) can be initialized by **yaw_deg = doa_azimuth_deg - 90**, but MUST additionally account for actual mic-channel order, package mounting, calibration offset and head rotation before use.

The example documents an ADC-only processing path; do not assume built-in VAD, wake detection, speaker separation or complete audio front end. On-board mic geometry is documented for that hardware example; do not copy 43 mm to a later custom PCB.

## Limitations of a linear dual-mic array

- Two microphones cannot uniquely resolve the **front/back** ambiguity in free space, nor estimate reliable elevation. A valid 0..180° output is not full 360° localization.
- Multiple people talking simultaneously, music, Bot TTS/echo, room reflections, low SNR and vibration can produce unstable angles. A quality score does not identify a person.
- Audio direction is a *candidate sector*. Use it to prioritize a search when a person calls. Match with visual persons only when audio-visual timing/spatial evidence supports it, otherwise mark speaker association uncertain.
- **Current fixed base has no motor:** a call from outside the camera FOV cannot make the camera rotate. Eye rendering can acknowledge a direction within display comfort, but cannot truthfully claim face/eye contact outside FOV.
- During a future moving-head build, both mics and camera may rotate with the head. Transform DOA at **capture time** using the measured current head pose before target tracking; never compare a sound angle in a moving local frame with a previous global angle unchanged.

## One audio owner, shared stream

~~~text
board audio codec (one BSP device owner)
             |
       audio_capture
             |
        audio_frontend
          /   |   \
 local wake   |  DOA (when requested)
 model        |
           VAD / post-wake speech
             |
     brain_link media (only after accepted wake)
~~~

- Audio capture owns the microphone/codec sample cadence, channel layout and bounded pre-wake ring buffer.
- Always-on **keyword recognizer** sees full-phrase wake audio. It must not upload audio before recognition, and cannot be replaced by DOA or VAD.
- DOA may operate continuously if measured affordable, or compute over a recent multi-channel pre-wake/wake-time window when requested. Keep sufficient two-channel synchronization; converting early to mono loses localization information.
- During playback, own-speaker loopback can contaminate detection; incorporate AEC/echo gating or disable conflicting DOA measurements with explicit degraded status.
- Do not initialize two independent I2S/codec consumers inside wake and DOA modules.

## Events and proposed fields

**WAKE_DETECTED** (source: keyword engine):
- event_id, capture_monotonic_ms, complete_keyword_model_id, confidence, recognized_phrase=小呆小呆.
- Independent of visual status; if keyword model is not provisioned, **no valid detection**.

**AUDIO_DIRECTION** (source: DOA processor):
- observed_monotonic_ms, yaw_relative_to_microphone_deg, confidence/quality, valid, mic_array_calibration_id, ambiguous_front_back=true for linear two-mic geometry.
- Audio direction is not a target ID, 3D position or identity.

**AUDIO_SESSION** (source: post-wake audio):
- session_id, utterance_id, VAD state, optional speaker association, record/playback health.

bot_runtime accepts WAKE_DETECTED for wake. attention_manager accepts AUDIO_DIRECTION as candidate target cue under runtime focus policy; vision_attention continues observing all visible people. Neither directly drives Face or head GPIO.

## Future decision logic

1. Keyword phrase recognized locally -> runtime WAKING regardless of image quality, link or DOA availability.
2. If DOA valid, store short-lived candidate direction with source timestamp. Do **not** promote to confirmed person.
3. If camera sees 1+ faces, compare their bearings to the candidate and current conversation target; prefer temporal continuity. If unresolved, retain uncertain association.
4. After a speaker is selected, attention uses **human eye midpoint from camera** for fine spatial targeting, **not iris gaze angle and not raw DOA**.
5. If no face due to darkness/occlusion, continue audio-only conversation. Avoid looking at empty coordinates and declaring successful eye contact.

## Suggested manual hardware acceptance later

- Fixed board: call from left/front/right and ensure orientation and channel order are not reversed.
- Move/rotate the board while playing a fixed sound; distinguish local angle changes from changes in source direction (future actuator/pose integration).
- Check human voices, speech echo, multiple simultaneous sources and room reverberation.
- Confirm full single-word 小呆 never wakes and the full four-syllable model is required.
- Profile mic processing and wake performance while LCD and vision are active.

**No CI or background testing is part of this project.** The document is an integration contract and does not imply DOA, wake or multi-speaker localization already works on real hardware.

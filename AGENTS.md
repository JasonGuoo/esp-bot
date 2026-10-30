# esp-bot development instructions

## Delivery and testing policy

- **Do not create or configure CI pipelines** for this project. Do not add GitHub Actions verification jobs or treat CI as a merge prerequisite.
- Prioritize delivering modular, reviewable source code and clear integration contracts. If a real device, toolchain or practical test environment is unavailable, **write the code without attempting or requiring tests**.
- State the verification status precisely in the handoff: distinguish written code from builds, device runs, and camera/motor calibration actually performed. Never call an untested implementation "verified".
- Do not add testing infrastructure or workflow changes solely for reassurance. Existing local/manual test examples may remain for optional future use; do not execute or enforce them without a concrete reason or explicit request.
- Preserve separation between `vision_attention` (all face tracks and eye estimates), `attention` (selected target + head/eye motion planning), `face` (LVGL expression rendering), and server modules.
- Hardware target: **ESP32-S31-Korvo-1 V1.1 / ESP32-S31**, not ESP32-S3.

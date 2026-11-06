# bot_platform

Official ESP32-S31-Korvo-1 BSP adapter for shared I2C, display/touch, NVS and monotonic time. Audio owns its codec; vision owns the V4L2 stream. This component has no behavior policy.

- `create` preserves NVS data on initialization failure and reports peripheral degradation in status.
- `set_display_awake(false)` applies a black LVGL overlay with touch still active. BSP 1.1.0 backlight APIs return NOT_SUPPORTED on this board, so this is **logical blackout**, not a verified dark backlight.
- `prepare_camera` calls the official camera bootstrap at most once per boot, caching failures too. Upstream does not expose its allocated XCLK handle for full cleanup after initialization failure. The vision worker may retry stream open/start but bootstrap failure requires restart.
- LVGL operations use the BSP lock; dark-screen touch emits intent to the application.
- No SDK pin map is duplicated. No build or device execution performed.

See [complete integration/provenance](../../../docs/local-runtime.md).

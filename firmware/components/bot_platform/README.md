# bot_platform

Official ESP32-S31-Korvo-1 BSP adapter for shared I2C, display/touch, NVS and monotonic time. Audio owns its codec; vision owns the V4L2 stream. This component has no behavior policy.

- `create` preserves NVS data on initialization failure and reports peripheral degradation in status.
- `set_display_awake(false)` applies a black LVGL overlay with touch still active. BSP 1.1.0 backlight APIs return NOT_SUPPORTED on this board, so this is **logical blackout**, not a verified dark backlight.
- `prepare_camera` calls the official camera bootstrap at most once per boot, caching failures too. Upstream does not expose its allocated XCLK handle for full cleanup after initialization failure. The vision worker may retry stream open/start but bootstrap failure requires restart.
- LVGL operations use the BSP lock; dark-screen touch emits intent to the application.
- No SDK pin map is duplicated. No build or device execution performed.

See [complete integration/provenance](../../../docs/local-runtime.md).

## Sound preferences

`bot_platform_load_voice_settings()` and `bot_platform_save_voice_settings()`
store the driver-free `bot_voice_settings_t` in `espbot_ui/voice_settings` as one
versioned NVS `uint32_t`. The layout is explicit rather than the C struct ABI:
bits 31–24 version 1, bit 23 reserved zero, bits 22–16 night gain,
bit 15 spoken answers, bits 14–8 wake threshold, bit 7 mute and bits 6–0 volume.
Load validates version/reserved bits and all shared input ranges; any missing,
invalid or unavailable value leaves the caller's initialized defaults unchanged.
Save rejects invalid input before opening storage, reads the current value and
skips both `set` and `commit` when unchanged. A changed value requires one
`nvs_set_u32` and one `nvs_commit`; read/write/commit errors are returned to the
application. No automatic retries, namespace/partition erases or pairing changes
are performed. Restoring defaults writes only this sound-preferences key.

This follows the official
[NVS integer example](https://github.com/espressif/esp-idf/blob/master/examples/storage/nvs/nvs_rw_value/main/nvs_value_example_main.c)
and [S31 NVS API](https://docs.espressif.com/projects/esp-idf/en/latest/esp32s31/api-reference/storage/nvs_flash.html).
The example's erase-on-init-failure recovery is deliberately not used: existing
network credentials and pairing data are preserved. The new persistence adapter
has been reviewed in source; no ESP-IDF build, NVS emulator or physical-device
write was run for it in this environment.

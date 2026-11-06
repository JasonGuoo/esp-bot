/** Board resources shared by local adapters; no behavior decisions here. */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"
#include "lvgl.h"
#ifdef __cplusplus
extern "C" {
#endif

typedef struct bot_platform bot_platform_t;
typedef struct {
    bool display_available;
    bool display_awake;
    bool backlight_control_supported;
    bool full_blackout_verified;
    bool nvs_available;
    bool shared_i2c_available;
    esp_err_t display_error;
} bot_platform_status_t;
typedef void (*bot_platform_touch_cb_t)(void *context);

/** Uses the official S31 BSP. NVS errors do not erase existing credentials.
 * Peripheral failures are exposed in status, allowing audio/runtime to start
 * even if the display cannot. Allocation/argument failures return an error. */
esp_err_t bot_platform_create(bot_platform_t **out);
uint32_t bot_platform_now_ms(void);
bool bot_platform_display_lock(uint32_t timeout_ms);
void bot_platform_display_unlock(void);
/** Caller holds display lock. */
lv_obj_t *bot_platform_screen(bot_platform_t *platform);
/** Logical blackout + best-effort BSP backlight control. Touch remains active.
 * Never uses bsp_display_enter_sleep(), which also disables the touch device. */
esp_err_t bot_platform_set_display_awake(bot_platform_t *platform, bool awake);
/** Register before starting the runtime; callback only enqueues a local event. */
void bot_platform_on_dark_touch(bot_platform_t *platform,
                                bot_platform_touch_cb_t callback, void *context);
/** Owner task only (initialization and display reconciliation share that owner). */
void bot_platform_get_status(const bot_platform_t *platform, bot_platform_status_t *out);
/** Single camera worker only. BSP camera bootstrap is attempted once per boot;
 * a failed attempt is latched because upstream does not expose XCLK cleanup. */
esp_err_t bot_platform_prepare_camera(bot_platform_t *platform);
esp_err_t bot_platform_load_theme(bot_platform_t *platform, char *out, size_t size);
esp_err_t bot_platform_save_theme(bot_platform_t *platform, const char *theme_id);
#ifdef __cplusplus
}
#endif

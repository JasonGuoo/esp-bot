#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "bot_platform.h"
#include "vision_attention.h"
#ifdef __cplusplus
extern "C" {
#endif
typedef enum { BOT_DEBUG_RGB565_LE, BOT_DEBUG_RGB565_BE, BOT_DEBUG_RGB888,
               BOT_DEBUG_UYVY, BOT_DEBUG_YUYV } bot_debug_pixels_t;
/* Explicit physical USB development session only. Disabled at boot, expires
 * without a keepalive, never starts a camera or grants runtime permissions. */
esp_err_t bot_debug_start(bot_platform_t *platform);
void bot_debug_set_preview(bool enabled);
void bot_debug_keepalive(void);
bool bot_debug_preview_enabled(void);
void bot_debug_set_screen_snapshot(bool snapshot);
/* Borrowed pixels are consumed/copied before returning. Never queues pointers
 * owned by camera/LVGL. Busy preview slots are dropped rather than blocking. */
void bot_debug_camera(const void *pixels, uint16_t width, uint16_t height,
                      bot_debug_pixels_t format, uint32_t captured_ms,
                      const vatt_observation_t *observations, size_t count);
void bot_debug_event(unsigned kind, unsigned source, uint32_t observed_ms);
#ifdef __cplusplus
}
#endif

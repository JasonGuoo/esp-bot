/* Adapter over Espressif esp-bsp S31 display example. No copied pin maps.
 * Source/provenance: docs/local-runtime.md. */
#include "bot_platform.h"
#include "rgb565_frame.h"
#include <stdlib.h>
#include <string.h>
#include "sdkconfig.h"
#include "bsp/esp32_s31_korvo_1.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "nvs.h"
#include "nvs_flash.h"

static const char *TAG = "bot_platform";
struct bot_platform {
    lv_display_t *display;
    lv_obj_t *blackout;
    bot_platform_status_t status;
    bot_platform_touch_cb_t on_touch;
    void *touch_context;
    bool camera_attempted;
    esp_err_t camera_prepare_error;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    /* LVGL owns these full-size buffers. Read only under the display lock. */
    const lv_draw_buf_t *last_flushed_frame;
    lv_draw_buf_t *snapshot_frame;
    int64_t refresh_started_us;
    bot_platform_display_metrics_t metrics;
#endif
};

uint32_t bot_platform_now_ms(void) { return (uint32_t)(esp_timer_get_time() / 1000); }
bool bot_platform_display_lock(uint32_t timeout_ms) { return bsp_display_lock(timeout_ms); }
void bot_platform_display_unlock(void) { bsp_display_unlock(); }

static void dark_touch(lv_event_t *event) {
    bot_platform_t *p = lv_event_get_user_data(event);
    if (p && p->on_touch) p->on_touch(p->touch_context);
}

static void trace_touch(lv_event_t *event) {
    (void)event;
    lv_point_t point;
    lv_indev_get_point(bsp_display_get_input_dev(), &point);
    ESP_LOGI(TAG, "LVGL touch pressed at %ld,%ld", (long)point.x, (long)point.y);
}

#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
/* LVGL sends this after completing rendering, before it swaps the active
 * double buffer. Record the rendered buffer, never render the tree again. */
static void display_trace(lv_event_t *event) {
    bot_platform_t *p = lv_event_get_user_data(event);
    if (!p || !p->display) return;
    const lv_event_code_t code = lv_event_get_code(event);
    if (code == LV_EVENT_FLUSH_START && lv_display_flush_is_last(p->display))
        p->last_flushed_frame = lv_display_get_buf_active(p->display);
    else if (code == LV_EVENT_REFR_START)
        p->refresh_started_us = esp_timer_get_time();
    else if (code == LV_EVENT_REFR_READY && p->refresh_started_us) {
        const int64_t elapsed = esp_timer_get_time() - p->refresh_started_us;
        if (elapsed >= 0 && elapsed <= UINT32_MAX) {
            p->metrics.refreshes++;
            p->metrics.refresh_total_us += (uint32_t)elapsed;
            if ((uint32_t)elapsed > p->metrics.refresh_max_us)
                p->metrics.refresh_max_us = (uint32_t)elapsed;
        }
        p->refresh_started_us = 0;
    }
}
#endif

esp_err_t bot_platform_create(bot_platform_t **out) {
    if (!out) return ESP_ERR_INVALID_ARG;
    *out = NULL;
    bot_platform_t *p = calloc(1, sizeof(*p));
    if (!p) return ESP_ERR_NO_MEM;
    esp_err_t err = nvs_flash_init();
    p->status.nvs_available = err == ESP_OK;
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "NVS unavailable (%s); preferences disabled; no automatic erase",
                 esp_err_to_name(err));
    }
    err = bsp_i2c_init();
    p->status.shared_i2c_available = err == ESP_OK;
    if (err != ESP_OK) {
        p->status.display_error = err;
        *out = p;
        return ESP_OK;
    }
    p->display = bsp_display_start();
    if (!p->display) {
        p->status.display_error = ESP_FAIL;
        *out = p;
        return ESP_OK;
    }
    p->status.display_available = true;
    p->status.display_awake = true;
    if (!bsp_display_lock(0)) {
        p->status.display_error = ESP_ERR_TIMEOUT;
        p->status.display_available = false;
        *out = p;
        return ESP_OK;
    }
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    if (!lv_display_add_event_cb(p->display, display_trace, LV_EVENT_ALL, p))
        ESP_LOGW(TAG, "USB screen preview unavailable: display callback allocation failed");
#endif
    /* Top layer stays above face/settings. A child click never bubbles into face. */
    p->blackout = lv_obj_create(lv_display_get_layer_top(p->display));
    lv_obj_set_size(p->blackout, lv_pct(100), lv_pct(100));
    lv_obj_set_style_bg_color(p->blackout, lv_color_black(), 0);
    lv_obj_set_style_bg_opa(p->blackout, LV_OPA_COVER, 0);
    lv_obj_set_style_border_width(p->blackout, 0, 0);
    lv_obj_set_style_radius(p->blackout, 0, 0);
    lv_obj_set_style_pad_all(p->blackout, 0, 0);
    lv_obj_remove_flag(p->blackout, LV_OBJ_FLAG_SCROLLABLE);
    lv_obj_add_flag(p->blackout, LV_OBJ_FLAG_CLICKABLE | LV_OBJ_FLAG_HIDDEN);
    lv_obj_add_event_cb(p->blackout, dark_touch, LV_EVENT_CLICKED, p);
    lv_indev_t *touch = bsp_display_get_input_dev();
    if (touch) lv_indev_add_event_cb(touch, trace_touch, LV_EVENT_PRESSED, p);
    bsp_display_unlock();
    err = bsp_display_backlight_on();
    p->status.backlight_control_supported = err == ESP_OK;
    p->status.display_error = err;
    /* A successful driver call does not establish physical optical blackout. */
    p->status.full_blackout_verified = false;
    *out = p;
    return ESP_OK;
}

lv_obj_t *bot_platform_screen(bot_platform_t *p) {
    return p && p->status.display_available ? lv_display_get_screen_active(p->display) : NULL;
}

esp_err_t bot_platform_set_display_awake(bot_platform_t *p, bool awake) {
    if (!p) return ESP_ERR_INVALID_ARG;
    if (!p->status.display_available) return ESP_ERR_INVALID_STATE;
    if (p->status.display_awake == awake) return ESP_OK;
    if (!bsp_display_lock(20)) return ESP_ERR_TIMEOUT;
    if (awake) lv_obj_add_flag(p->blackout, LV_OBJ_FLAG_HIDDEN);
    else lv_obj_remove_flag(p->blackout, LV_OBJ_FLAG_HIDDEN);
    bsp_display_unlock();
    esp_err_t err = awake ? bsp_display_backlight_on() : bsp_display_backlight_off();
    p->status.backlight_control_supported = err == ESP_OK;
    p->status.display_error = err;
    p->status.display_awake = awake; /* requested logical image state is applied */
    if (err != ESP_OK && err != ESP_ERR_NOT_SUPPORTED)
        ESP_LOGW(TAG, "Backlight request failed: %s (logical overlay applied)", esp_err_to_name(err));
    return ESP_OK;
}

void bot_platform_on_dark_touch(bot_platform_t *p, bot_platform_touch_cb_t cb, void *context) {
    if (!p) return;
    p->on_touch = cb;
    p->touch_context = context;
}
void bot_platform_get_status(const bot_platform_t *p, bot_platform_status_t *out) {
    if (p && out) *out = p->status;
}

esp_err_t bot_platform_copy_screen(bot_platform_t *p, uint8_t *pixels,
                                   size_t capacity, uint16_t width, uint16_t height,
                                   uint32_t *captured_ms, bool snapshot) {
    if (!p || !pixels || !captured_ms || !width || !height ||
        capacity < (size_t)width * height * 2) return ESP_ERR_INVALID_ARG;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    if (!p->status.display_available) return ESP_ERR_INVALID_STATE;
    if (!bsp_display_lock(5)) return ESP_ERR_TIMEOUT;
    const lv_draw_buf_t *frame = p->last_flushed_frame;
    const lv_display_render_mode_t mode = lv_display_get_render_mode(p->display);
    esp_err_t result = ESP_ERR_INVALID_STATE;
    if (snapshot) {
        lv_obj_t *screen = lv_display_get_screen_active(p->display);
        if (!p->snapshot_frame && screen)
            p->snapshot_frame = lv_snapshot_create_draw_buf(screen, LV_COLOR_FORMAT_RGB565);
        frame = p->snapshot_frame;
        if (!frame || !screen ||
            lv_snapshot_take_to_draw_buf(screen, LV_COLOR_FORMAT_RGB565, p->snapshot_frame) != LV_RESULT_OK)
            frame = NULL;
    }
    if (frame && frame->data &&
        (snapshot || mode == LV_DISPLAY_RENDER_MODE_DIRECT || mode == LV_DISPLAY_RENDER_MODE_FULL) &&
        frame->header.cf == LV_COLOR_FORMAT_RGB565 &&
        frame->header.w == lv_display_get_horizontal_resolution(p->display) &&
        frame->header.h == lv_display_get_vertical_resolution(p->display) &&
        frame->header.stride >= (uint32_t)frame->header.w * 2 &&
        frame->data_size >= (uint32_t)frame->header.stride * frame->header.h) {
        if (bot_platform_scale_rgb565(frame->data, frame->data_size,
                                      frame->header.w, frame->header.h, frame->header.stride,
                                      pixels, capacity, width, height)) {
            *captured_ms = bot_platform_now_ms();
            result = ESP_OK;
        }
    }
    bsp_display_unlock();
    return result;
#else
    (void)snapshot;
    return ESP_ERR_NOT_SUPPORTED;
#endif
}

esp_err_t bot_platform_read_display_metrics(bot_platform_t *p, bot_platform_display_metrics_t *out) {
    if (!p || !out) return ESP_ERR_INVALID_ARG;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    if (!p->status.display_available) return ESP_ERR_INVALID_STATE;
    if (!bsp_display_lock(5)) return ESP_ERR_TIMEOUT;
    *out = p->metrics;
    p->metrics.refreshes = 0;
    p->metrics.refresh_total_us = 0;
    p->metrics.refresh_max_us = 0;
    out->display_width = lv_display_get_horizontal_resolution(p->display);
    out->display_height = lv_display_get_vertical_resolution(p->display);
    out->render_mode = lv_display_get_render_mode(p->display);
    lv_obj_t *screen = lv_display_get_screen_active(p->display);
    if (screen) {
        lv_area_t area;
        lv_obj_get_coords(screen, &area);
        out->x1 = area.x1; out->y1 = area.y1; out->x2 = area.x2; out->y2 = area.y2;
        out->scroll_y = lv_obj_get_scroll_y(screen);
    }
    bsp_display_unlock();
    return ESP_OK;
#else
    return ESP_ERR_NOT_SUPPORTED;
#endif
}
esp_err_t bot_platform_prepare_camera(bot_platform_t *p) {
    if (!p) return ESP_ERR_INVALID_ARG;
    if (!p->camera_attempted) {
        p->camera_attempted = true;
        p->camera_prepare_error = bsp_camera_start(NULL);
        if (p->camera_prepare_error != ESP_OK)
            ESP_LOGW(TAG, "BSP camera bootstrap failed: %s; retry requires restart",
                     esp_err_to_name(p->camera_prepare_error));
    }
    return p->camera_prepare_error;
}
esp_err_t bot_platform_load_theme(bot_platform_t *p, char *out, size_t size) {
    if (!p || !out || !size) return ESP_ERR_INVALID_ARG;
    if (!p->status.nvs_available) return ESP_ERR_INVALID_STATE;
    nvs_handle_t handle;
    esp_err_t err = nvs_open("espbot_ui", NVS_READONLY, &handle);
    if (err != ESP_OK) return err;
    err = nvs_get_str(handle, "appearance", out, &size);
    nvs_close(handle);
    return err;
}
esp_err_t bot_platform_save_theme(bot_platform_t *p, const char *id) {
    if (!p || !id || !*id || strlen(id) > 31) return ESP_ERR_INVALID_ARG;
    if (!p->status.nvs_available) return ESP_ERR_INVALID_STATE;
    nvs_handle_t handle;
    esp_err_t err = nvs_open("espbot_ui", NVS_READWRITE, &handle);
    if (err != ESP_OK) return err;
    char current[32];
    size_t size = sizeof(current);
    if (nvs_get_str(handle, "appearance", current, &size) == ESP_OK && strcmp(current, id) == 0) {
        nvs_close(handle);
        return ESP_OK;
    }
    err = nvs_set_str(handle, "appearance", id);
    if (err == ESP_OK) err = nvs_commit(handle);
    nvs_close(handle);
    return err;
}

esp_err_t bot_platform_load_camera_orientation(bot_platform_t *p, uint8_t *orientation) {
    if (!p || !orientation) return ESP_ERR_INVALID_ARG;
    if (!p->status.nvs_available) return ESP_ERR_INVALID_STATE;
    nvs_handle_t handle;
    esp_err_t err = nvs_open("espbot_ui", NVS_READONLY, &handle);
    if (err != ESP_OK) return err;
    uint8_t value = 0;
    err = nvs_get_u8(handle, "camera_orient", &value);
    nvs_close(handle);
    if (err == ESP_OK && value > 3) return ESP_ERR_INVALID_RESPONSE;
    if (err == ESP_OK) *orientation = value;
    return err;
}

esp_err_t bot_platform_save_camera_orientation(bot_platform_t *p, uint8_t orientation) {
    if (!p || orientation > 3) return ESP_ERR_INVALID_ARG;
    if (!p->status.nvs_available) return ESP_ERR_INVALID_STATE;
    nvs_handle_t handle;
    esp_err_t err = nvs_open("espbot_ui", NVS_READWRITE, &handle);
    if (err != ESP_OK) return err;
    uint8_t current = 0;
    if (nvs_get_u8(handle, "camera_orient", &current) == ESP_OK && current == orientation) {
        nvs_close(handle);
        return ESP_OK;
    }
    err = nvs_set_u8(handle, "camera_orient", orientation);
    if (err == ESP_OK) err = nvs_commit(handle);
    nvs_close(handle);
    return err;
}

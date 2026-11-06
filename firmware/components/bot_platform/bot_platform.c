/* Adapter over Espressif esp-bsp S31 display example. No copied pin maps.
 * Source/provenance: docs/local-runtime.md. */
#include "bot_platform.h"
#include <stdlib.h>
#include <string.h>
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
};

uint32_t bot_platform_now_ms(void) { return (uint32_t)(esp_timer_get_time() / 1000); }
bool bot_platform_display_lock(uint32_t timeout_ms) { return bsp_display_lock(timeout_ms); }
void bot_platform_display_unlock(void) { bsp_display_unlock(); }

static void dark_touch(lv_event_t *event) {
    bot_platform_t *p = lv_event_get_user_data(event);
    if (p && p->on_touch) p->on_touch(p->touch_context);
}

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
    err = nvs_set_str(handle, "appearance", id);
    if (err == ESP_OK) err = nvs_commit(handle);
    nvs_close(handle);
    return err;
}

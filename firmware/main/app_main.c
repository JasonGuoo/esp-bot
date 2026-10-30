#include "esp_log.h"
#include "nvs_flash.h"
#include "nvs.h"
#include "bsp/esp32_s31_korvo_1.h"
#include "face.h"
#include "brain_client.h"

static const char *TAG = "esp_bot";

/* Device persistence is injected via callback: Face itself stays NVS-free. */
static void persist_appearance(const char *theme_id, void *ctx) {
    (void)ctx;
    nvs_handle_t handle;
    esp_err_t err = nvs_open("espbot_ui", NVS_READWRITE, &handle);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "Cannot open UI preferences: %s", esp_err_to_name(err));
        return;
    }
    err = nvs_set_str(handle, "appearance", theme_id);
    if (err == ESP_OK) err = nvs_commit(handle);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "Failed to save appearance: %s", esp_err_to_name(err));
    }
    nvs_close(handle);
}

static void load_appearance(face_view_t *face) {
    nvs_handle_t handle;
    if (nvs_open("espbot_ui", NVS_READONLY, &handle) != ESP_OK) return;
    char theme_id[32];
    size_t n = sizeof(theme_id);
    if (nvs_get_str(handle, "appearance", theme_id, &n) == ESP_OK &&
        !face_set_theme(face, theme_id)) {
        ESP_LOGW(TAG, "Unknown saved appearance: %s", theme_id);
    }
    nvs_close(handle);
}


void app_main(void) {
    /* The official ESP32-S31-Korvo-1 BSP owns LCD RGB pins and GT1151 touch.
       Do NOT use pin maps from ESP32-S3-Korvo-1. */
    esp_err_t ret = nvs_flash_init();
    if (ret == ESP_ERR_NVS_NO_FREE_PAGES ||
        ret == ESP_ERR_NVS_NEW_VERSION_FOUND) {
        ESP_ERROR_CHECK(nvs_flash_erase());
        ret = nvs_flash_init();
    }
    ESP_ERROR_CHECK(ret);

    lv_display_t *display = bsp_display_start();
    if (!display) {
        ESP_LOGE(TAG, "BSP display / touch initialization failed");
        return;
    }

    if (!bsp_display_lock(0)) {
        ESP_LOGE(TAG, "Unable to acquire LVGL lock");
        return;
    }
    face_view_t *face = face_create(lv_screen_active());
    if (face) {
        face_set_expression(face, "idle");
        load_appearance(face);
        face_on_theme_applied(face, persist_appearance, NULL);
    }
    bsp_display_unlock();
    if (!face) {
        ESP_LOGE(TAG, "Cannot allocate face");
        return;
    }
    ESP_LOGI(TAG, "Face ready: tap -> gear -> Settings -> Appearance -> Apply.");
    /* Wi-Fi is optional. The face can always run as a standalone demo. */
    brain_client_start(face);
}

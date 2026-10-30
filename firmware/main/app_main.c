#include "esp_log.h"
#include "nvs_flash.h"
#include "bsp/esp32_s31_korvo_1.h"
#include "face.h"
#include "brain_client.h"

static const char *TAG = "esp_bot";

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
        face_set_theme(face, "mint");
    }
    bsp_display_unlock();
    if (!face) {
        ESP_LOGE(TAG, "Cannot allocate face");
        return;
    }
    ESP_LOGI(TAG, "Face started. Tap screen to reveal theme settings.");
    /* Wi-Fi is optional. The face can always run as a standalone demo. */
    brain_client_start(face);
}

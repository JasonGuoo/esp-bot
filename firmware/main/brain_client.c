#include "brain_client.h"
#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include "sdkconfig.h"
#include "esp_log.h"
#include "esp_check.h"
#include "esp_wifi.h"
#include "esp_event.h"
#include "esp_netif.h"
#include "esp_http_client.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "cJSON.h"
#include "bsp/esp32_s31_korvo_1.h"

static const char *TAG = "brain_client";

typedef struct {
    char data[2048];
    size_t used;
    bool overflow;
} http_buffer_t;

static esp_err_t on_http(esp_http_client_event_t *evt) {
    if (evt->event_id == HTTP_EVENT_ON_DATA && evt->user_data) {
        http_buffer_t *buf = (http_buffer_t *)evt->user_data;
        if (evt->data_len <= 0) return ESP_OK;
        if (buf->used + (size_t)evt->data_len >= sizeof(buf->data)) {
            buf->overflow = true;
            return ESP_OK;
        }
        memcpy(buf->data + buf->used, evt->data, evt->data_len);
        buf->used += (size_t)evt->data_len;
        buf->data[buf->used] = '\0';
    }
    return ESP_OK;
}

static void on_wifi(void *arg, esp_event_base_t base, int32_t id, void *data) {
    (void)arg; (void)base; (void)data;
    if (id == WIFI_EVENT_STA_DISCONNECTED) {
        ESP_LOGW(TAG, "Wi-Fi disconnected; reconnecting");
        esp_wifi_connect();
    }
}

static esp_err_t connect_wifi(void) {
    esp_err_t err = esp_netif_init();
    if (err != ESP_OK) return err;
    err = esp_event_loop_create_default();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;
    if (!esp_netif_create_default_wifi_sta()) return ESP_FAIL;
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_RETURN_ON_ERROR(esp_wifi_init(&init), TAG, "Wi-Fi init");
    ESP_RETURN_ON_ERROR(
        esp_event_handler_register(WIFI_EVENT, WIFI_EVENT_STA_DISCONNECTED,
                                   on_wifi, NULL),
        TAG, "Wi-Fi handler");
    wifi_config_t cfg = {0};
    snprintf((char *)cfg.sta.ssid, sizeof(cfg.sta.ssid), "%s", CONFIG_ESPBOT_WIFI_SSID);
    snprintf((char *)cfg.sta.password, sizeof(cfg.sta.password),
             "%s", CONFIG_ESPBOT_WIFI_PASSWORD);
    ESP_RETURN_ON_ERROR(esp_wifi_set_mode(WIFI_MODE_STA), TAG, "STA mode");
    ESP_RETURN_ON_ERROR(esp_wifi_set_config(WIFI_IF_STA, &cfg), TAG, "STA config");
    ESP_RETURN_ON_ERROR(esp_wifi_start(), TAG, "Wi-Fi start");
    return esp_wifi_connect();
}

static void apply_face_command(face_view_t *face, const cJSON *payload) {
    if (!cJSON_IsObject(payload)) return;
    if (!bsp_display_lock(1000)) {
        ESP_LOGW(TAG, "LVGL lock timed out; command skipped");
        return;
    }
    const cJSON *theme = cJSON_GetObjectItemCaseSensitive(payload, "theme");
    const cJSON *expression = cJSON_GetObjectItemCaseSensitive(payload, "expression");
    const cJSON *subtitle = cJSON_GetObjectItemCaseSensitive(payload, "subtitle");
    const cJSON *gx = cJSON_GetObjectItemCaseSensitive(payload, "gaze_x");
    const cJSON *gy = cJSON_GetObjectItemCaseSensitive(payload, "gaze_y");
    if (cJSON_IsString(theme)) face_set_theme(face, theme->valuestring);
    if (cJSON_IsString(expression)) face_set_expression(face, expression->valuestring);
    if (cJSON_IsString(subtitle)) face_set_subtitle(face, subtitle->valuestring);
    if (cJSON_IsNumber(gx) || cJSON_IsNumber(gy)) {
        /* A partial gaze command resets the other axis to center. */
        int x = cJSON_IsNumber(gx) ? gx->valueint : 0;
        int y = cJSON_IsNumber(gy) ? gy->valueint : 0;
        face_set_gaze(face, x, y);
    }
    bsp_display_unlock();
}

static void poll_one(face_view_t *face, const char *url) {
    http_buffer_t buf = {0};
    esp_http_client_config_t cfg = {
        .url = url, .timeout_ms = 3500, .event_handler = on_http,
        .user_data = &buf,
    };
    esp_http_client_handle_t client = esp_http_client_init(&cfg);
    if (!client) return;
    if (CONFIG_ESPBOT_AUTH_TOKEN[0]) {
        char bearer[256];
        snprintf(bearer, sizeof(bearer), "Bearer %s", CONFIG_ESPBOT_AUTH_TOKEN);
        esp_http_client_set_header(client, "Authorization", bearer);
    }
    esp_err_t err = esp_http_client_perform(client);
    int status = esp_http_client_get_status_code(client);
    esp_http_client_cleanup(client);
    if (err != ESP_OK || status != 200 || buf.overflow || !buf.used) return;
    cJSON *root = cJSON_Parse(buf.data);
    if (!root) return;
    const cJSON *cmd = cJSON_GetObjectItemCaseSensitive(root, "command");
    const cJSON *type = cJSON_GetObjectItemCaseSensitive(cmd, "type");
    if (cJSON_IsString(type) && strcmp(type->valuestring, "face.set") == 0) {
        apply_face_command(face, cJSON_GetObjectItemCaseSensitive(cmd, "payload"));
    }
    cJSON_Delete(root);
}

static void poll_task(void *argument) {
    face_view_t *face = (face_view_t *)argument;
    if (connect_wifi() != ESP_OK) {
        ESP_LOGE(TAG, "Wi-Fi init failed; face still available offline");
        vTaskDelete(NULL);
        return;
    }
    char url[420];
    snprintf(url, sizeof(url), "%s/v1/commands/next?device_id=%s",
             CONFIG_ESPBOT_BRAIN_URL, CONFIG_ESPBOT_DEVICE_ID);
    ESP_LOGI(TAG, "Connecting to configured Brain URL");
    while (true) {
        poll_one(face, url);
        vTaskDelay(pdMS_TO_TICKS(900));
    }
}

void brain_client_start(face_view_t *face) {
    if (!face || !CONFIG_ESPBOT_WIFI_SSID[0] || !CONFIG_ESPBOT_BRAIN_URL[0]) {
        ESP_LOGI(TAG, "No Wi-Fi/Brain URL configured; offline face mode");
        return;
    }
    xTaskCreate(poll_task, "brain_poll", 6144, face, 5, NULL);
}

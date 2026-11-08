/* Wi-Fi setup follows ESP-IDF examples/wifi/getting_started/station.
 * LAN is an optional diagnostic connection, not an authority bypass. */
#include "brain_link.h"
#include <stdio.h>
#include <string.h>
#include "sdkconfig.h"
#include "bot_lan.h"
#include "cJSON.h"
#include "esp_event.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_netif_sntp.h"
#include "esp_timer.h"
#include "esp_wifi.h"
#include "freertos/event_groups.h"
#include "freertos/task.h"

static const char *TAG = "brain_link";
#define WIFI_RECONNECT BIT0
static bot_bus_t *s_bus;
static EventGroupHandle_t s_events;
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
static bool s_time_sync_started;
static void clock_synchronized(struct timeval *tv) {
    (void)tv;
    ESP_LOGI(TAG, "Network clock synchronized for the night schedule");
}

/* Only sets wall time for the local sleep schedule; runtime leases and
 * security decisions continue to use their existing monotonic clock. */
static void start_night_clock(void) {
    if (!CONFIG_ESPBOT_NTP_SERVER[0]) return;
    esp_err_t err;
    if (!s_time_sync_started) {
        esp_sntp_config_t cfg = ESP_NETIF_SNTP_DEFAULT_CONFIG(CONFIG_ESPBOT_NTP_SERVER);
        cfg.wait_for_sync = false;
        cfg.sync_cb = clock_synchronized;
        err = esp_netif_sntp_init(&cfg);
        if (err == ESP_OK) s_time_sync_started = true;
    } else {
        err = esp_netif_sntp_start();
    }
    if (err != ESP_OK) ESP_LOGW(TAG, "Night clock synchronization unavailable: %s", esp_err_to_name(err));
}
#endif

static void link_state(bool connected) {
    const bot_event_t event = {
        .kind = BOT_EVENT_LINK_STATE, .source = BOT_SOURCE_LINK,
        .observed_ms = (uint32_t)(esp_timer_get_time() / 1000),
        .data.link = { .connected = connected, .authenticated = false, .encrypted = false },
    };
    esp_err_t err = bot_bus_post(s_bus, &event, 0);
    if (err != ESP_OK) ESP_LOGW(TAG, "Link state queue: %s", esp_err_to_name(err));
}

static void reject_untrusted(const char *json) {
    cJSON *root = cJSON_ParseWithLengthOpts(json, strlen(json) + 1, NULL, true);
    const cJSON *type = cJSON_GetObjectItemCaseSensitive(root, "type");
    const cJSON *id = cJSON_GetObjectItemCaseSensitive(root, "id");
    if (!cJSON_IsString(type) || strcmp(type->valuestring, "request") != 0 ||
        !cJSON_IsString(id) || !id->valuestring[0] || strlen(id->valuestring) > 96) {
        cJSON_Delete(root);
        return;
    }
    cJSON *reply = cJSON_CreateObject();
    if (reply) {
        cJSON_AddStringToObject(reply, "type", "response");
        cJSON_AddStringToObject(reply, "id", id->valuestring);
        cJSON_AddBoolToObject(reply, "ok", false);
        cJSON_AddStringToObject(reply, "error", "AUTH_REQUIRED");
        cJSON_AddStringToObject(reply, "detail", "LAN hello is not authenticated; local runtime retains control");
        char *encoded = cJSON_PrintUnformatted(reply);
        if (encoded) {
            esp_err_t err = bot_lan_send_response(encoded);
            if (err != ESP_OK) ESP_LOGW(TAG, "Reject receipt queue: %s", esp_err_to_name(err));
            cJSON_free(encoded);
        }
        cJSON_Delete(reply);
    }
    cJSON_Delete(root);
}

static void lan_event(const bot_lan_event_t *event, void *context) {
    (void)context;
    if (event->type == BOT_LAN_EVENT_SCAN_COMPLETE) {
        ESP_LOGI(TAG, "Diagnostic discovery: %u compatible server(s); retries remain active",
                 (unsigned)event->discovered_count);
        return;
    }
    if (event->type == BOT_LAN_EVENT_JSON) reject_untrusted(event->json);
    else if (event->type == BOT_LAN_EVENT_CONNECTED) {
        ESP_LOGI(TAG, "Diagnostic bot-server connected; private media remains disabled");
        link_state(true);
    } else if (event->type == BOT_LAN_EVENT_DISCONNECTED || event->type == BOT_LAN_EVENT_ERROR) {
        ESP_LOGW(TAG, "Diagnostic link unavailable (%s); automatic discovery continues",
                 esp_err_to_name(event->error));
        link_state(false);
    }
}

static void wifi_event(void *arg, esp_event_base_t base, int32_t id, void *data) {
    (void)arg;
    if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        ESP_LOGI(TAG, "Wi-Fi address acquired; discovering bot-server");
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
        start_night_clock();
#endif
        return;
    }
    if (base == WIFI_EVENT && (id == WIFI_EVENT_STA_START || id == WIFI_EVENT_STA_DISCONNECTED)) {
        if (id == WIFI_EVENT_STA_DISCONNECTED) {
            const wifi_event_sta_disconnected_t *event = data;
            ESP_LOGW(TAG, "Wi-Fi disconnected (reason=%u); retrying in background",
                     event ? event->reason : 0);
            link_state(false);
        }
        xEventGroupSetBits(s_events, WIFI_RECONNECT);
    }
}

static void network_task(void *argument) {
    (void)argument;
    bot_lan_config_t cfg = { .auto_connect = true, .scan_interval_ms = 5000 };
    snprintf(cfg.device_id, sizeof(cfg.device_id), "%s", CONFIG_ESPBOT_DEVICE_ID);
    snprintf(cfg.device_name, sizeof(cfg.device_name), "%s", "小呆");
    snprintf(cfg.preferred_server_id, sizeof(cfg.preferred_server_id), "%s", CONFIG_ESPBOT_SERVER_ID);
    esp_err_t err = bot_lan_start(&cfg, lan_event, NULL);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "LAN unavailable: %s; local runtime remains active", esp_err_to_name(err));
        link_state(false);
    }
    for (;;) {
        xEventGroupWaitBits(s_events, WIFI_RECONNECT, pdTRUE, pdFALSE, portMAX_DELAY);
        err = esp_wifi_connect();
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "Wi-Fi connect: %s", esp_err_to_name(err));
            xEventGroupSetBits(s_events, WIFI_RECONNECT);
        }
        /* Bound reconnect attempts even when authentication fails immediately. */
        vTaskDelay(pdMS_TO_TICKS(3000));
    }
}

esp_err_t brain_link_start(bot_bus_t *bus) {
    if (!bus || s_bus) return ESP_ERR_INVALID_ARG;
    s_bus = bus;
    if (strlen(CONFIG_ESPBOT_WIFI_SSID) > 32 || strlen(CONFIG_ESPBOT_WIFI_PASSWORD) > 64)
        return ESP_ERR_INVALID_SIZE;
    esp_err_t err = esp_netif_init();
    if (err != ESP_OK) return err;
    err = esp_event_loop_create_default();
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE) return err;
    esp_netif_t *netif = esp_netif_create_default_wifi_sta();
    if (!netif) return ESP_ERR_NO_MEM;
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    err = esp_wifi_init(&init);
    if (err != ESP_OK) { esp_netif_destroy_default_wifi(netif); return err; }
    /* Driver init loads any previously provisioned STA config from NVS.
     * Normal boots/reconnects use RAM and never rewrite Wi-Fi credentials. */
    err = esp_wifi_set_storage(WIFI_STORAGE_RAM);
    if (err == ESP_OK) err = esp_wifi_set_mode(WIFI_MODE_STA);
    wifi_config_t cfg = {0};
    if (err == ESP_OK && CONFIG_ESPBOT_WIFI_SSID[0]) {
        memcpy(cfg.sta.ssid, CONFIG_ESPBOT_WIFI_SSID, strlen(CONFIG_ESPBOT_WIFI_SSID));
        memcpy(cfg.sta.password, CONFIG_ESPBOT_WIFI_PASSWORD, strlen(CONFIG_ESPBOT_WIFI_PASSWORD));
    } else if (err == ESP_OK) {
        err = esp_wifi_get_config(WIFI_IF_STA, &cfg);
    }
    if (err != ESP_OK || !cfg.sta.ssid[0]) {
        esp_wifi_deinit();
        esp_netif_destroy_default_wifi(netif);
        if (err != ESP_OK) return err;
        ESP_LOGI(TAG, "No saved or build-time Wi-Fi credentials; offline local runtime");
        link_state(false);
        return ESP_OK;
    }
    s_events = xEventGroupCreate();
    if (!s_events) {
        esp_wifi_deinit(); esp_netif_destroy_default_wifi(netif);
        return ESP_ERR_NO_MEM;
    }
    err = esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event, NULL);
    if (err == ESP_OK)
        err = esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event, NULL);
    if (err == ESP_OK) err = esp_wifi_set_config(WIFI_IF_STA, &cfg);
    if (err == ESP_OK) err = esp_wifi_start();
    memset(&cfg, 0, sizeof(cfg));
    if (err == ESP_OK && xTaskCreate(network_task, "brain_link", 6144, NULL, 4, NULL) != pdPASS)
        err = ESP_ERR_NO_MEM;
    if (err != ESP_OK) {
        esp_wifi_stop();
        esp_event_handler_unregister(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event);
        esp_event_handler_unregister(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event);
        esp_wifi_deinit();
        esp_netif_destroy_default_wifi(netif);
        vEventGroupDelete(s_events);
        s_events = NULL;
        return err;
    }
    return ESP_OK;
}

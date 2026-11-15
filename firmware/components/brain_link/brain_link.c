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
#include "mbedtls/base64.h"
#include "mdns.h"
#include "nvs.h"

static const char *TAG = "brain_link";
static bot_bus_t *s_bus;
ESP_EVENT_DEFINE_BASE(BOT_WIFI_RETRY);
static esp_timer_handle_t s_retry_timer;
static bool s_needs_connection; /* accessed only on the default event loop */
static bool s_mdns_ready;

/* Store the USB-provisioned override separately from the Wi-Fi driver's old
 * settings. This preserves the previous settings as a fallback and avoids
 * rewriting flash during ordinary reconnects. Flash encryption is not enabled
 * on the current board, so physical access can reveal these credentials. */
typedef struct { uint32_t version; char ssid[33]; char password[65]; } saved_wifi_t;

static bool valid_wifi(const saved_wifi_t *wifi) {
    size_t ssid = strnlen(wifi->ssid, sizeof(wifi->ssid));
    size_t pass = strnlen(wifi->password, sizeof(wifi->password));
    if (wifi->version != 1 || ssid == 0 || ssid > 32 || pass > 64 ||
        (pass > 0 && pass < 8)) return false;
    for (size_t i = 0; i < pass; i++) {
        unsigned char c = (unsigned char)wifi->password[i];
        if (c < 32 || c > 126) return false;
        if (pass == 64 && !((c >= '0' && c <= '9') || (c >= 'a' && c <= 'f') ||
                            (c >= 'A' && c <= 'F'))) return false;
    }
    return true;
}

esp_err_t brain_link_wifi_provision(const char *input) {
    if (!input || strlen(input) > 512) return ESP_ERR_INVALID_ARG;
    unsigned char decoded[256] = {0};
    size_t size = 0;
    saved_wifi_t wifi = { .version = 1 };
    esp_err_t result = ESP_ERR_INVALID_ARG;
    cJSON *json = NULL;
    if (mbedtls_base64_decode(decoded, sizeof(decoded) - 1, &size,
                              (const unsigned char *)input, strlen(input)) != 0 || size == 0) goto done;
    json = cJSON_ParseWithLengthOpts((const char *)decoded, size + 1, NULL, true);
    const cJSON *ssid = cJSON_GetObjectItemCaseSensitive(json, "ssid");
    const cJSON *password = cJSON_GetObjectItemCaseSensitive(json, "password");
    if (!cJSON_IsString(ssid) || !cJSON_IsString(password) ||
        strlen(ssid->valuestring) >= sizeof(wifi.ssid) ||
        strlen(password->valuestring) >= sizeof(wifi.password)) goto done;
    memcpy(wifi.ssid, ssid->valuestring, strlen(ssid->valuestring));
    memcpy(wifi.password, password->valuestring, strlen(password->valuestring));
    if (!valid_wifi(&wifi)) goto done;
    nvs_handle_t nvs;
    result = nvs_open("espbot_wifi", NVS_READWRITE, &nvs);
    if (result == ESP_OK) {
        result = nvs_set_blob(nvs, "station", &wifi, sizeof(wifi));
        if (result == ESP_OK) result = nvs_commit(nvs);
        nvs_close(nvs);
    }
done:
    if (json) {
        cJSON *secret = cJSON_GetObjectItemCaseSensitive(json, "password");
        if (cJSON_IsString(secret) && secret->valuestring)
            memset(secret->valuestring, 0, strlen(secret->valuestring));
        cJSON_Delete(json);
    }
    memset(decoded, 0, sizeof(decoded));
    memset(&wifi, 0, sizeof(wifi));
    return result;
}

static bool load_saved_wifi(saved_wifi_t *wifi) {
    nvs_handle_t nvs;
    if (nvs_open("espbot_wifi", NVS_READONLY, &nvs) != ESP_OK) return false;
    size_t size = sizeof(*wifi);
    esp_err_t err = nvs_get_blob(nvs, "station", wifi, &size);
    nvs_close(nvs);
    return err == ESP_OK && size == sizeof(*wifi) && valid_wifi(wifi);
}
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

#if CONFIG_ESPBOT_LAN_DIAGNOSTICS
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
    } else if (event->type == BOT_LAN_EVENT_DISCONNECTED) {
        ESP_LOGI(TAG, "Diagnostic connection closed; automatic discovery continues");
    } else if (event->type == BOT_LAN_EVENT_ERROR) {
        ESP_LOGW(TAG, "Diagnostic link unavailable (%s); automatic discovery continues",
                 esp_err_to_name(event->error));
    }
}
#endif

static void schedule_retry(void) {
    esp_err_t err = esp_timer_start_once(s_retry_timer, 3000000);
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE)
        ESP_LOGW(TAG, "Wi-Fi retry timer: %s", esp_err_to_name(err));
}

static void retry_timer_fired(void *arg) {
    (void)arg;
    if (esp_event_post(BOT_WIFI_RETRY, 0, NULL, 0, 0) != ESP_OK) schedule_retry();
}

static void wifi_event(void *arg, esp_event_base_t base, int32_t id, void *data) {
    (void)arg;
    if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP) {
        const ip_event_got_ip_t *event = data;
        ESP_LOGI(TAG, "Wi-Fi STA " IPSTR " mask " IPSTR " gateway " IPSTR,
                 IP2STR(&event->ip_info.ip), IP2STR(&event->ip_info.netmask), IP2STR(&event->ip_info.gw));
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
        start_night_clock();
#endif
        return;
    }
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_CONNECTED) {
        s_needs_connection = false;
        (void)esp_timer_stop(s_retry_timer);
    } else if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED) {
        const wifi_event_sta_disconnected_t *event = data;
        ESP_LOGW(TAG, "Wi-Fi disconnected (reason=%u); retrying in background", event ? event->reason : 0);
        brain_secure_network_down();
        s_needs_connection = true;
        schedule_retry();
    } else if ((base == WIFI_EVENT && id == WIFI_EVENT_STA_START) ||
               (base == BOT_WIFI_RETRY && s_needs_connection)) {
        /* The IDF station example calls connect on the default event loop.
         * Only the retry delay needs a timer, not a permanent task/6 KiB stack. */
        s_needs_connection = true;
        esp_err_t err = esp_wifi_connect();
        if (err != ESP_OK) {
            ESP_LOGW(TAG, "Wi-Fi connect: %s", esp_err_to_name(err));
            schedule_retry();
        }
    }
}

void brain_link_start_diagnostics(void) {
#if CONFIG_ESPBOT_LAN_DIAGNOSTICS
    if (!s_retry_timer) return; /* offline/unconfigured */
    bot_lan_config_t cfg = { .auto_connect = true, .scan_interval_ms = 5000, .mdns_preinitialized = s_mdns_ready };
    snprintf(cfg.device_id, sizeof(cfg.device_id), "%s", CONFIG_ESPBOT_DEVICE_ID);
    snprintf(cfg.device_name, sizeof(cfg.device_name), "%s", "小呆");
    snprintf(cfg.preferred_server_id, sizeof(cfg.preferred_server_id), "%s", CONFIG_ESPBOT_SERVER_ID);
    esp_err_t err = bot_lan_start(&cfg, lan_event, NULL);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "LAN unavailable: %s; local runtime remains active", esp_err_to_name(err));
    }
#endif
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
    saved_wifi_t saved = {0};
    if (err == ESP_OK && load_saved_wifi(&saved)) {
        memcpy(cfg.sta.ssid, saved.ssid, strlen(saved.ssid));
        memcpy(cfg.sta.password, saved.password, strlen(saved.password));
    } else if (err == ESP_OK && CONFIG_ESPBOT_WIFI_SSID[0]) {
        memcpy(cfg.sta.ssid, CONFIG_ESPBOT_WIFI_SSID, strlen(CONFIG_ESPBOT_WIFI_SSID));
        memcpy(cfg.sta.password, CONFIG_ESPBOT_WIFI_PASSWORD, strlen(CONFIG_ESPBOT_WIFI_PASSWORD));
    } else if (err == ESP_OK) {
        err = esp_wifi_get_config(WIFI_IF_STA, &cfg);
    }
    memset(&saved, 0, sizeof(saved));
    if (err != ESP_OK || !cfg.sta.ssid[0]) {
        esp_wifi_deinit();
        esp_netif_destroy_default_wifi(netif);
        if (err != ESP_OK) return err;
        ESP_LOGI(TAG, "No saved or build-time Wi-Fi credentials; offline local runtime");
        return ESP_OK;
    }
    const esp_timer_create_args_t timer = { .callback = retry_timer_fired, .name = "wifi_retry" };
    err = esp_timer_create(&timer, &s_retry_timer);
    if (err == ESP_OK) err = esp_event_handler_register(BOT_WIFI_RETRY, 0, wifi_event, NULL);
    if (err == ESP_OK) err = esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event, NULL);
    if (err == ESP_OK)
        err = esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event, NULL);
    if (err == ESP_OK) err = esp_wifi_set_config(WIFI_IF_STA, &cfg);
    if (err == ESP_OK) err = esp_wifi_start();
    if (err == ESP_OK) {
        /* Real-time voice needs a continuously responsive station. Espressif
         * recommends disabling modem sleep when diagnosing unstable links. */
        esp_err_t ps = esp_wifi_set_ps(WIFI_PS_NONE);
        if (ps != ESP_OK) ESP_LOGW(TAG, "Could not disable Wi-Fi power save: %s", esp_err_to_name(ps));
    }
    memset(&cfg, 0, sizeof(cfg));
    if (err != ESP_OK) {
        esp_wifi_stop();
        esp_event_handler_unregister(BOT_WIFI_RETRY, 0, wifi_event);
        esp_event_handler_unregister(WIFI_EVENT, ESP_EVENT_ANY_ID, wifi_event);
        esp_event_handler_unregister(IP_EVENT, IP_EVENT_STA_GOT_IP, wifi_event);
        esp_wifi_deinit();
        esp_netif_destroy_default_wifi(netif);
        if (s_retry_timer) { esp_timer_delete(s_retry_timer); s_retry_timer = NULL; }
        return err;
    }
    /* Both link versions share Espressif's one mDNS responder. The official
     * query_advertise example also initializes it before acquiring an IP. */
    s_mdns_ready = mdns_init() == ESP_OK;
    return ESP_OK;
}

#include "bot_lan.h"
#include "discovery.h"

#include <stdio.h>
#include <string.h>
#include <stdint.h>
#include <ctype.h>
#include <arpa/inet.h>
#include "cJSON.h"
#include "esp_log.h"
#include "esp_netif.h"
#include "esp_websocket_client.h"
#include "mdns.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

static const char *TAG = "bot_lan";
#define QUEUE_LENGTH 8
#define DEFAULT_RESCAN_MS 5000
#define WS_HANDSHAKE_TIMEOUT_MS 10000

typedef enum {
    CMD_SCAN,
    CMD_CONNECT,
    CMD_DISCONNECT,
    CMD_RESUME_AUTO,
    CMD_SEND_EVENT,
} cmd_kind_t;

typedef struct {
    cmd_kind_t kind;
    bot_lan_server_t server;
    char json[BOT_LAN_MAX_EVENT_JSON + 1];
} command_t;

static struct {
    SemaphoreHandle_t mutex;
    QueueHandle_t queue;
    bot_lan_config_t cfg;
    bot_lan_callback_t callback;
    void *callback_ctx;

    bot_lan_server_t servers[BOT_LAN_MAX_SERVERS];
    size_t count;
    bot_lan_server_t selected;
    bot_lan_state_t state;
    bool auto_paused;
    bool mdns_ready;

    esp_websocket_client_handle_t ws; /* owned solely by worker task */
    bool ws_connected;
    bool hello_sent;
    bool welcomed;
    bool ws_failed;
    TickType_t connection_started;
    TickType_t last_scan;
} s;

static void notify(bot_lan_event_type_t type, esp_err_t error, const char *json) {
    if (!s.callback) return;
    bot_lan_event_t ev = { .type = type, .error = error };
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    ev.state = s.state;
    ev.discovered_count = s.count;
    ev.server = s.selected;
    xSemaphoreGive(s.mutex);
    if (json) snprintf(ev.json, sizeof(ev.json), "%s", json);
    s.callback(&ev, s.callback_ctx);
}

static void change_state(bot_lan_state_t state) {
    bool changed;
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    changed = s.state != state;
    s.state = state;
    xSemaphoreGive(s.mutex);
    if (changed) notify(BOT_LAN_EVENT_STATE, ESP_OK, NULL);
}

static bool wifi_has_ip(void) {
    esp_netif_t *wifi = esp_netif_get_handle_from_ifkey("WIFI_STA_DEF");
    if (!wifi) return false;
    esp_netif_ip_info_t info = {0};
    return esp_netif_get_ip_info(wifi, &info) == ESP_OK && info.ip.addr != 0;
}

static bool is_local_ipv4(const char *ip) {
    struct in_addr address;
    if (!ip || inet_pton(AF_INET, ip, &address) != 1) return false;
    uint32_t n = ntohl(address.s_addr);
    return (n >> 24) == 10 ||
           (n >> 20) == 0xAC1 ||          /* 172.16.0.0/12 */
           (n >> 16) == 0xC0A8 ||         /* 192.168.0.0/16 */
           (n >> 16) == 0xA9FE ||         /* 169.254.0.0/16 */
           (n >> 24) == 127;
}

static bool valid_device_id(const char *id, size_t capacity) {
    size_t len = strnlen(id, capacity);
    if (!len || len > 64 || len >= capacity) return false;
    for (size_t i = 0; i < len; ++i) {
        unsigned char c = (unsigned char)id[i];
        if ((!isalnum(c) || c > 127) && c != '_' && c != '-') return false;
    }
    return true;
}

static void websocket_event(void *handler_args, esp_event_base_t base,
                            int32_t event_id, void *event_data) {
    (void)handler_args;
    (void)base;
    if (event_id == WEBSOCKET_EVENT_CONNECTED) {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        s.ws_connected = true;
        s.ws_failed = false;
        s.hello_sent = false;
        s.welcomed = false;
        xSemaphoreGive(s.mutex);
        return;
    }
    if (event_id == WEBSOCKET_EVENT_DISCONNECTED ||
        event_id == WEBSOCKET_EVENT_CLOSED || event_id == WEBSOCKET_EVENT_ERROR) {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        s.ws_failed = true;
        s.ws_connected = false;
        xSemaphoreGive(s.mutex);
        return;
    }
    if (event_id != WEBSOCKET_EVENT_DATA || !event_data) return;
    const esp_websocket_event_data_t *data = (const esp_websocket_event_data_t *)event_data;
    if (data->op_code != 0x1 || data->payload_offset != 0 ||
        data->data_len != data->payload_len ||
        data->data_len <= 0 || data->data_len > BOT_LAN_MAX_EVENT_JSON) {
        return; /* No fragmented/oversized JSON is forwarded into application code. */
    }

    char input[BOT_LAN_MAX_EVENT_JSON + 1];
    memcpy(input, data->data_ptr, (size_t)data->data_len);
    input[data->data_len] = '\0';
    cJSON *root = cJSON_Parse(input);
    if (!root || !cJSON_IsObject(root)) {
        cJSON_Delete(root);
        return;
    }
    const cJSON *type = cJSON_GetObjectItemCaseSensitive(root, "type");
    if (cJSON_IsString(type) && strcmp(type->valuestring, "welcome") == 0) {
        const cJSON *version = cJSON_GetObjectItemCaseSensitive(root, "protocol");
        const cJSON *server_id = cJSON_GetObjectItemCaseSensitive(root, "server_id");
        bool valid = cJSON_IsNumber(version) && version->valueint == BOT_LAN_PROTOCOL_VERSION &&
                     cJSON_IsString(server_id);
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        valid = valid && strcmp(s.selected.server_id, server_id->valuestring) == 0;
        if (valid) {
            s.welcomed = true;
            s.state = BOT_LAN_CONNECTED;
        } else {
            s.ws_failed = true;
        }
        xSemaphoreGive(s.mutex);
        if (valid) {
            notify(BOT_LAN_EVENT_CONNECTED, ESP_OK, NULL);
        } else {
            notify(BOT_LAN_EVENT_ERROR, ESP_ERR_INVALID_RESPONSE, NULL);
        }
    } else {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        bool ready = s.welcomed;
        xSemaphoreGive(s.mutex);
        if (ready) notify(BOT_LAN_EVENT_JSON, ESP_OK, input);
    }
    cJSON_Delete(root);
}

static void destroy_socket(void) {
    if (!s.ws) return;
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    bool previously_connected = s.welcomed;
    s.ws_connected = false;
    s.welcomed = false;
    s.hello_sent = false;
    xSemaphoreGive(s.mutex);
    /* Never call these inside websocket_event: the worker owns lifecycle. */
    esp_websocket_client_stop(s.ws);
    esp_websocket_client_destroy(s.ws);
    s.ws = NULL;
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    s.ws_failed = false;
    xSemaphoreGive(s.mutex);
    if (previously_connected) notify(BOT_LAN_EVENT_DISCONNECTED, ESP_OK, NULL);
}

static esp_err_t start_connection(const bot_lan_server_t *server) {
    if (!server || !is_local_ipv4(server->ip) || server->port == 0 ||
        !server->server_id[0]) return ESP_ERR_INVALID_ARG;
    destroy_socket();
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    s.selected = *server;
    s.ws_failed = false;
    s.ws_connected = false;
    s.hello_sent = false;
    s.welcomed = false;
    xSemaphoreGive(s.mutex);

    char uri[128];
    snprintf(uri, sizeof(uri), "ws://%s:%u%s", server->ip, server->port, BOT_LAN_WS_PATH);
    const esp_websocket_client_config_t config = {
        .uri = uri,
        .disable_auto_reconnect = true,
        .network_timeout_ms = 3000,
        .ping_interval_sec = 20,
        .pingpong_timeout_sec = 10,
        .task_stack = 6144,
        .buffer_size = 1024,
    };
    s.ws = esp_websocket_client_init(&config);
    if (!s.ws) return ESP_ERR_NO_MEM;
    esp_err_t err = esp_websocket_register_events(s.ws, WEBSOCKET_EVENT_ANY, websocket_event, NULL);
    if (err == ESP_OK) err = esp_websocket_client_start(s.ws);
    if (err != ESP_OK) {
        destroy_socket();
        return err;
    }
    s.connection_started = xTaskGetTickCount();
    change_state(BOT_LAN_CONNECTING);
    return ESP_OK;
}

static void send_hello(void) {
    cJSON *root = cJSON_CreateObject();
    if (!root) return;
    cJSON_AddStringToObject(root, "type", "hello");
    cJSON_AddNumberToObject(root, "protocol", BOT_LAN_PROTOCOL_VERSION);
    cJSON_AddStringToObject(root, "device_id", s.cfg.device_id);
    cJSON_AddStringToObject(root, "name", s.cfg.device_name);
    char *json = cJSON_PrintUnformatted(root);
    cJSON_Delete(root);
    if (!json) return;
    int written = esp_websocket_client_send_text(s.ws, json, (int)strlen(json),
                                                  pdMS_TO_TICKS(1500));
    if (written <= 0) {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        s.ws_failed = true;
        xSemaphoreGive(s.mutex);
    }
    cJSON_free(json);
}

static esp_err_t perform_scan(bool auto_select) {
    change_state(BOT_LAN_SCANNING);
    bot_lan_server_t found[BOT_LAN_MAX_SERVERS] = {0};
    size_t count = 0;
    esp_err_t err = bot_lan_discovery_scan(found, BOT_LAN_MAX_SERVERS, &count);
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    if (err == ESP_OK) {
        memcpy(s.servers, found, sizeof(found));
        s.count = count;
    } else {
        s.count = 0;
    }
    xSemaphoreGive(s.mutex);
    s.last_scan = xTaskGetTickCount();

    if (err != ESP_OK) {
        change_state(BOT_LAN_ERROR);
        notify(BOT_LAN_EVENT_ERROR, err, NULL);
        return err;
    }
    notify(BOT_LAN_EVENT_SCAN_COMPLETE, ESP_OK, NULL);
    if (auto_select && count == 1) {
        err = start_connection(&found[0]);
        if (err != ESP_OK) {
            change_state(BOT_LAN_ERROR);
            notify(BOT_LAN_EVENT_ERROR, err, NULL);
        }
    } else if (count > 1 && auto_select) {
        change_state(BOT_LAN_SELECTION_REQUIRED);
    } else if (!s.ws) {
        change_state(BOT_LAN_DISCONNECTED);
    } else {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        bool ready = s.welcomed;
        xSemaphoreGive(s.mutex);
        change_state(ready ? BOT_LAN_CONNECTED : BOT_LAN_CONNECTING);
    }
    return err;
}

static void worker(void *arg) {
    (void)arg;
    if (s.cfg.mdns_preinitialized) s.mdns_ready = true;

    for (;;) {
        command_t cmd;
        if (xQueueReceive(s.queue, &cmd, pdMS_TO_TICKS(200)) == pdTRUE) {
            if (cmd.kind == CMD_DISCONNECT) {
                s.auto_paused = true;
                destroy_socket();
                change_state(BOT_LAN_DISCONNECTED);
            } else if (cmd.kind == CMD_RESUME_AUTO) {
                s.auto_paused = false;
                s.last_scan = 0;
            } else if (cmd.kind == CMD_CONNECT) {
                s.auto_paused = false;
                esp_err_t err = wifi_has_ip()
                    ? start_connection(&cmd.server) : ESP_ERR_INVALID_STATE;
                if (err != ESP_OK) {
                    change_state(BOT_LAN_ERROR);
                    notify(BOT_LAN_EVENT_ERROR, err, NULL);
                }
            } else if (cmd.kind == CMD_SCAN) {
                if (wifi_has_ip()) {
                    esp_err_t err = ESP_OK;
                    if (!s.mdns_ready) {
                        err = mdns_init();
                        s.mdns_ready = err == ESP_OK;
                    }
                    if (err == ESP_OK) (void)perform_scan(false);
                    else notify(BOT_LAN_EVENT_ERROR, err, NULL);
                } else {
                    notify(BOT_LAN_EVENT_ERROR, ESP_ERR_INVALID_STATE, NULL);
                }
            } else if (cmd.kind == CMD_SEND_EVENT && s.ws) {
                xSemaphoreTake(s.mutex, portMAX_DELAY);
                bool ready = s.welcomed;
                xSemaphoreGive(s.mutex);
                if (ready) (void)esp_websocket_client_send_text(
                    s.ws, cmd.json, (int)strlen(cmd.json), pdMS_TO_TICKS(1500));
            }
        }

        if (!wifi_has_ip()) {
            if (s.ws) destroy_socket();
            change_state(BOT_LAN_WAITING_FOR_WIFI);
            continue;
        }
        if (!s.mdns_ready) {
            esp_err_t err = mdns_init();
            s.mdns_ready = err == ESP_OK;
            if (err != ESP_OK) {
                change_state(BOT_LAN_ERROR);
                notify(BOT_LAN_EVENT_ERROR, err, NULL);
                vTaskDelay(pdMS_TO_TICKS(1500));
                continue;
            }
        }

        if (s.ws) {
            xSemaphoreTake(s.mutex, portMAX_DELAY);
            bool connected = s.ws_connected;
            bool sent = s.hello_sent;
            bool welcomed = s.welcomed;
            bool failed = s.ws_failed;
            if (connected && !sent) s.hello_sent = true;
            xSemaphoreGive(s.mutex);

            if (connected && !sent) send_hello();
            if (failed || (!welcomed &&
                (xTaskGetTickCount() - s.connection_started) > pdMS_TO_TICKS(WS_HANDSHAKE_TIMEOUT_MS))) {
                destroy_socket();
                change_state(BOT_LAN_DISCONNECTED);
            }
        }

        if (!s.ws && !s.auto_paused && s.cfg.auto_connect &&
            (s.last_scan == 0 ||
             (xTaskGetTickCount() - s.last_scan) >=
             pdMS_TO_TICKS(s.cfg.scan_interval_ms))) {
            (void)perform_scan(true);
        }
    }
}

esp_err_t bot_lan_start(const bot_lan_config_t *config,
                        bot_lan_callback_t callback, void *ctx) {
    if (!config || !valid_device_id(config->device_id, sizeof(config->device_id)) ||
        !config->device_name[0] ||
        strnlen(config->device_name, sizeof(config->device_name)) >= sizeof(config->device_name) ||
        s.queue) return ESP_ERR_INVALID_ARG;
    s.mutex = xSemaphoreCreateMutex();
    s.queue = xQueueCreate(QUEUE_LENGTH, sizeof(command_t));
    if (!s.mutex || !s.queue) {
        if (s.queue) vQueueDelete(s.queue);
        if (s.mutex) vSemaphoreDelete(s.mutex);
        s.queue = NULL;
        s.mutex = NULL;
        return ESP_ERR_NO_MEM;
    }
    s.cfg = *config;
    if (s.cfg.scan_interval_ms < 2000) s.cfg.scan_interval_ms = DEFAULT_RESCAN_MS;
    s.callback = callback;
    s.callback_ctx = ctx;
    s.state = BOT_LAN_WAITING_FOR_WIFI;
    if (xTaskCreate(worker, "bot_lan", 8192, NULL, 5, NULL) != pdPASS) {
        vQueueDelete(s.queue);
        vSemaphoreDelete(s.mutex);
        s.queue = NULL;
        s.mutex = NULL;
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

static esp_err_t submit(const command_t *cmd) {
    if (!s.queue || !cmd) return ESP_ERR_INVALID_STATE;
    return xQueueSend(s.queue, cmd, 0) == pdTRUE ? ESP_OK : ESP_ERR_TIMEOUT;
}

esp_err_t bot_lan_scan(void) {
    const command_t cmd = { .kind = CMD_SCAN };
    return submit(&cmd);
}

esp_err_t bot_lan_connect(const bot_lan_server_t *server) {
    if (!server || !is_local_ipv4(server->ip) || server->port == 0 ||
        !server->server_id[0]) return ESP_ERR_INVALID_ARG;
    /* Manual selections must originate from our most recent discovery list. */
    bool known = false;
    if (s.mutex) {
        xSemaphoreTake(s.mutex, portMAX_DELAY);
        for (size_t i = 0; i < s.count; ++i) {
            if (strcmp(server->server_id, s.servers[i].server_id) == 0 &&
                strcmp(server->ip, s.servers[i].ip) == 0 &&
                server->port == s.servers[i].port) known = true;
        }
        xSemaphoreGive(s.mutex);
    }
    if (!known) return ESP_ERR_NOT_FOUND;
    command_t cmd = { .kind = CMD_CONNECT, .server = *server };
    return submit(&cmd);
}

esp_err_t bot_lan_disconnect(void) {
    const command_t cmd = { .kind = CMD_DISCONNECT };
    return submit(&cmd);
}

esp_err_t bot_lan_resume_auto(void) {
    const command_t cmd = { .kind = CMD_RESUME_AUTO };
    return submit(&cmd);
}

size_t bot_lan_copy_servers(bot_lan_server_t *out, size_t capacity) {
    if (!s.mutex || !out || capacity == 0) return 0;
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    size_t n = s.count < capacity ? s.count : capacity;
    memcpy(out, s.servers, n * sizeof(*out));
    xSemaphoreGive(s.mutex);
    return n;
}

bot_lan_state_t bot_lan_state(void) {
    if (!s.mutex) return BOT_LAN_STOPPED;
    xSemaphoreTake(s.mutex, portMAX_DELAY);
    bot_lan_state_t state = s.state;
    xSemaphoreGive(s.mutex);
    return state;
}

esp_err_t bot_lan_send_event(const char *json) {
    if (!json) return ESP_ERR_INVALID_ARG;
    size_t len = strnlen(json, BOT_LAN_MAX_EVENT_JSON + 1);
    if (!len || len > BOT_LAN_MAX_EVENT_JSON) return ESP_ERR_INVALID_SIZE;
    cJSON *root = cJSON_Parse(json);
    if (!cJSON_IsObject(root)) {
        cJSON_Delete(root);
        return ESP_ERR_INVALID_ARG;
    }
    const cJSON *type = cJSON_GetObjectItemCaseSensitive(root, "type");
    bool valid = cJSON_IsString(type) && strcmp(type->valuestring, "event") == 0;
    cJSON_Delete(root);
    if (!valid || bot_lan_state() != BOT_LAN_CONNECTED) return ESP_ERR_INVALID_STATE;
    command_t cmd = { .kind = CMD_SEND_EVENT };
    memcpy(cmd.json, json, len + 1);
    return submit(&cmd);
}

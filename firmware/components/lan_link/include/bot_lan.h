#pragma once

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define BOT_LAN_SERVICE_TYPE "_espbot-server._tcp.local."
#define BOT_LAN_WS_PATH "/v1/lan/ws"
#define BOT_LAN_PROTOCOL_VERSION 1
#define BOT_LAN_MAX_SERVERS 8
#define BOT_LAN_MAX_EVENT_JSON 512

/** IPv4-only discovery records. These are copied values, safe for UI use. */
typedef struct {
    char instance[64];
    char server_id[48];
    char ip[16];
    uint16_t port;
} bot_lan_server_t;

typedef enum {
    BOT_LAN_STOPPED = 0,
    BOT_LAN_WAITING_FOR_WIFI,
    BOT_LAN_SCANNING,
    BOT_LAN_DISCONNECTED,
    BOT_LAN_SELECTION_REQUIRED,
    BOT_LAN_CONNECTING,
    BOT_LAN_CONNECTED,
    BOT_LAN_ERROR,
} bot_lan_state_t;

typedef enum {
    BOT_LAN_EVENT_STATE,
    BOT_LAN_EVENT_SCAN_COMPLETE,
    BOT_LAN_EVENT_CONNECTED,
    BOT_LAN_EVENT_DISCONNECTED,
    BOT_LAN_EVENT_JSON,
    BOT_LAN_EVENT_ERROR,
} bot_lan_event_type_t;

typedef struct {
    bot_lan_event_type_t type;
    bot_lan_state_t state;
    esp_err_t error;
    size_t discovered_count;
    bot_lan_server_t server;
    char json[BOT_LAN_MAX_EVENT_JSON + 1];
} bot_lan_event_t;

/**
 * Called from the lan_link worker or WebSocket event task, NOT the LVGL task.
 * Copy data and dispatch it to your UI thread before updating display objects.
 * Never call blocking network operations inside this callback.
 */
typedef void (*bot_lan_callback_t)(const bot_lan_event_t *event, void *ctx);

typedef struct {
    char device_id[65];       /* ASCII alnum, underscore or dash; max 64 */
    char device_name[65];     /* UTF-8, max 64 bytes */
    bool auto_connect;       /* auto-connect only when exactly ONE server is found */
    bool mdns_preinitialized; /* true if another component already called mdns_init */
    uint32_t scan_interval_ms; /* 0 selects 5000 ms */
} bot_lan_config_t;

/**
 * Start once after the application has created Wi-Fi STA/netif.
 * Does not initialize Wi-Fi, start the face, or handle credentials.
 * The worker waits for a nonzero STA IPv4 address before discovering.
 */
esp_err_t bot_lan_start(const bot_lan_config_t *config,
                        bot_lan_callback_t callback, void *ctx);

/** Nonblocking; completion is BOT_LAN_EVENT_SCAN_COMPLETE. */
esp_err_t bot_lan_scan(void);

/** Connect to an endpoint returned by bot_lan_copy_servers; nonblocking. */
esp_err_t bot_lan_connect(const bot_lan_server_t *server);

/** Disconnect and suspend auto-connect until bot_lan_resume_auto(). */
esp_err_t bot_lan_disconnect(void);

/** Resume automatic searches and connection when enabled in config. */
esp_err_t bot_lan_resume_auto(void);

/** Copy up to capacity discovered servers. Returns number copied. */
size_t bot_lan_copy_servers(bot_lan_server_t *out, size_t capacity);

/** Snapshot access; safe outside callbacks. */
bot_lan_state_t bot_lan_state(void);

/**
 * Queue one JSON object with type=="event" after welcome.
 * Future subsystems may use this; it does not send audio or model commands.
 * Payload must be <= BOT_LAN_MAX_EVENT_JSON bytes.
 */
esp_err_t bot_lan_send_event(const char *json);

#ifdef __cplusplus
}
#endif

/** Bounded task-to-task messages. No media buffers and no behavior decisions. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"
#include "freertos/FreeRTOS.h"
#include "vision_attention.h"
#ifdef __cplusplus
extern "C" {
#endif

#define BOT_BUS_SCHEMA_VERSION 1
#define BOT_CAPTION_MAX_BYTES 383
#define BOT_EXPRESSION_MAX_BYTES 15

/** All timestamps are low 32 bits of local esp_timer monotonic milliseconds.
 * Compare wrap-aware; all configured intervals must be shorter than 2^31 ms. */
typedef struct {
    uint32_t epoch;              /* changes on cancellation, interruption or new session */
    uint32_t session_id;         /* ephemeral within this boot, zero = no session */
    uint32_t turn_id;            /* ephemeral within session, zero = no turn */
    uint32_t focus_generation;   /* zero means command is not bound to a visual focus */
} bot_session_token_t;

typedef enum {
    BOT_SOURCE_PLATFORM = 0, BOT_SOURCE_AUDIO, BOT_SOURCE_VISION,
    BOT_SOURCE_LINK, BOT_SOURCE_LOCAL_UI, BOT_SOURCE_SYSTEM
} bot_event_source_t;

typedef enum {
    BOT_EVENT_WAKE_DETECTED = 0,
    BOT_EVENT_TOUCH_WAKE,
    BOT_EVENT_LOCAL_STOP,
    BOT_EVENT_SETTINGS_OPEN,
    BOT_EVENT_SETTINGS_CLOSE,
    BOT_EVENT_UTTERANCE_STARTED,
    BOT_EVENT_UTTERANCE_ENDED,
    BOT_EVENT_PLAYBACK_STARTED,
    BOT_EVENT_PLAYBACK_DONE,
    BOT_EVENT_PLAYBACK_FAILED,
    BOT_EVENT_LINK_STATE,
    BOT_EVENT_AUDIO_HEALTH,
    BOT_EVENT_CAMERA_HEALTH,
    BOT_EVENT_DISPLAY_HEALTH,
    BOT_EVENT_WAKE_STATUS,
    BOT_EVENT_NIGHT_POLICY,
    BOT_EVENT_SESSION_END,
    BOT_EVENT_FAULT,
    BOT_EVENT_COUNT
} bot_event_kind_t;

typedef enum {
    BOT_WAKE_MODEL_MISSING = 0, BOT_WAKE_READY, BOT_WAKE_UNAVAILABLE
} bot_wake_status_t;

typedef enum {
    BOT_WAKE_PHRASE_NONE = 0,
    BOT_WAKE_PHRASE_XIAODAI_XIAODAI /* one whole acoustic model keyword; never two name hits */
} bot_wake_phrase_t;

typedef struct {
    bot_event_kind_t kind;
    bot_event_source_t source;
    uint32_t observed_ms;
    bot_session_token_t token; /* required for utterance/playback/session completion */
    union {
        struct { bot_wake_phrase_t phrase; float confidence; bool echo_safe; bool confidence_available; } wake;
        struct { bool intentional; } touch;
        struct { bool echo_safe; } utterance;
        struct { uint32_t id; } playback; /* nonzero bot_audio playback_id, never synthesized */
        struct { bool connected, authenticated, encrypted; } link;
        struct { bool available; esp_err_t error; } health;
        struct { bool awake; bool sleep_supported; bool full_blackout_verified; esp_err_t error; } display;
        struct { bot_wake_status_t status; } wake_status;
        struct { bool clock_valid; bool night; } night;
        struct { uint32_t code; } fault;
    } data;
} bot_event_t;

typedef enum {
    BOT_COMMAND_SET_EXPRESSION = 0,
    BOT_COMMAND_SET_CAPTION,
    BOT_COMMAND_THINKING,
    BOT_COMMAND_REPLY_DONE,
    BOT_COMMAND_END_SESSION
} bot_command_kind_t;

typedef struct {
    uint32_t request_id;
    bot_command_kind_t kind;
    bot_session_token_t token;
    uint32_t received_ms;        /* stamped by local ingress, never remote uptime */
    uint32_t ttl_ms;             /* bounded by runtime; zero uses runtime default */
    union {
        char expression[BOT_EXPRESSION_MAX_BYTES + 1];
        char caption[BOT_CAPTION_MAX_BYTES + 1];
    } data;
} bot_command_t;

typedef struct {
    uint16_t critical_event_capacity;
    uint16_t event_capacity;
    uint16_t command_capacity;
} bot_bus_config_t;

typedef struct {
    uint32_t events_posted, events_received, normal_events_dropped;
    uint32_t critical_events_rejected, commands_posted, commands_rejected;
    uint32_t vision_published, vision_stale_rejected;
} bot_bus_stats_t;

typedef struct {
    bool local_stop;             /* latched independently of queue capacity */
    bool critical_overflow;      /* runtime must fail closed and invalidate session */
    uint32_t observed_ms;
} bot_bus_signals_t;

typedef struct bot_bus bot_bus_t;
bot_bus_config_t bot_bus_default_config(void);
esp_err_t bot_bus_create(const bot_bus_config_t *config, bot_bus_t **out);
/** Quiesce all producers/consumers first. */
void bot_bus_destroy(bot_bus_t *bus);
/** Task context only. Returns explicit ESP_ERR_TIMEOUT on a full queue.
 * Critical loss latches critical_overflow even when caller ignores the return. */
esp_err_t bot_bus_post(bot_bus_t *bus, const bot_event_t *event, TickType_t wait_ticks);
bool bot_bus_receive(bot_bus_t *bus, bot_event_t *out);
/** Stop uses a separate sticky latch and cannot wait behind queued work. */
void bot_bus_request_stop(bot_bus_t *bus, uint32_t observed_ms);
bot_bus_signals_t bot_bus_take_signals(bot_bus_t *bus);
esp_err_t bot_bus_submit_command(bot_bus_t *bus, const bot_command_t *command, TickType_t wait_ticks);
bool bot_bus_receive_command(bot_bus_t *bus, bot_command_t *out);
/** Copies a complete tracker snapshot, not camera bytes. Equal/older capture
 * timestamps are rejected. Do not change capture time to publication time. */
esp_err_t bot_bus_publish_vision(bot_bus_t *bus, const vatt_snapshot_t *snapshot);
bool bot_bus_read_vision(bot_bus_t *bus, vatt_snapshot_t *out);
/** Retire latest snapshot and its ordering timestamp while camera is stopped.
 * Nonblocking; false means contention, so the owner may retry on its next step. */
bool bot_bus_clear_vision(bot_bus_t *bus);
void bot_bus_get_stats(bot_bus_t *bus, bot_bus_stats_t *out);
#ifdef __cplusplus
}
#endif

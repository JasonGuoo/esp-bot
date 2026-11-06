/** 小呆 local behavior authority. Single owner calls step at ~20 ms.
 * Producers only post bus events/commands. No inference, rendering or drivers here. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"
#include "bot_bus.h"
#include "attention_manager.h"
#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    BOT_BEHAVIOR_IDLE = 0, BOT_BEHAVIOR_BORED, BOT_BEHAVIOR_DROWSY,
    BOT_BEHAVIOR_SLEEPING, BOT_BEHAVIOR_NIGHT_DARK, BOT_BEHAVIOR_WAKING,
    BOT_BEHAVIOR_ENGAGED
} bot_behavior_t;
typedef enum {
    BOT_CONVERSATION_NONE = 0, BOT_CONVERSATION_LISTENING,
    BOT_CONVERSATION_THINKING, BOT_CONVERSATION_SPEAKING,
    BOT_CONVERSATION_WAITING, BOT_CONVERSATION_ENDING
} bot_conversation_t;
typedef enum {
    BOT_COMMAND_ACCEPTED = 0, BOT_COMMAND_REJECTED_STALE,
    BOT_COMMAND_REJECTED_EXPIRED, BOT_COMMAND_REJECTED_UNTRUSTED,
    BOT_COMMAND_REJECTED_PASSIVE, BOT_COMMAND_REJECTED_SETTINGS,
    BOT_COMMAND_REJECTED_INVALID, BOT_COMMAND_REJECTED_FAULT
} bot_command_result_t;

typedef struct {
    bot_behavior_t behavior;
    bot_conversation_t conversation;
    bot_session_token_t token;
    bot_focus_snapshot_t focus;
    bot_wake_status_t wake_status;
    bool settings_open;
    bool display_awake_requested, camera_requested;
    bool audio_session_requested; /* local post-wake permission, never opens a second mic */
    bool audio_uplink_permitted;  /* session + current secure link + capture availability */
    bool visual_tracking_permitted;
    bool link_connected, link_authenticated, link_encrypted;
    bool audio_available, camera_available;
    bool display_awake, display_sleep_supported, display_full_blackout_verified;
    bool clock_valid, night_policy;
    bool critical_fault;
    bool utterance_active;
    uint32_t playback_id; /* current local playback operation; zero means none */
    uint32_t last_interaction_ms;
    uint32_t rejected_commands, rejected_events;
    char expression[BOT_EXPRESSION_MAX_BYTES + 1];
    char caption[BOT_CAPTION_MAX_BYTES + 1];
} bot_runtime_snapshot_t;

typedef struct {
    uint32_t idle_to_bored_ms, idle_to_drowsy_ms, idle_to_sleep_ms;
    uint32_t waking_ms;
    /* Maximum time between meaningful local conversation transitions, including
     * an ongoing utterance/playback. This is a hard progress bound, not silence
     * detection; longer narration needs an explicit larger bound. */
    uint32_t session_timeout_ms, wake_debounce_ms;
    uint32_t event_max_age_ms, command_max_age_ms;
    uint16_t max_events_per_step, max_commands_per_step;
    bool sleep_display;
    bool allow_audio_uplink;
    bool allow_barge_in;
    attention_manager_config_t focus;
} bot_runtime_config_t;

typedef struct {
    /** Called on owner task, after state changes; MUST be short/nonblocking.
     * Reconcile requested vs actual resource state in platform/audio adapters. */
    void (*apply_state)(void *context, const bot_runtime_snapshot_t *snapshot);
    /** Local output cancellation is called before remote cancellation. */
    void (*stop_output)(void *context);
    void (*cancel_remote)(void *context, const bot_session_token_t *invalidated_token);
    /** Transport should report this receipt for every dequeued remote command. */
    void (*command_result)(void *context, uint32_t request_id, bot_command_result_t result);
    /** Optional local monotonic clock, low 32 bits of milliseconds. Called on
     * the owner task after dequeuing input and before time-dependent decisions;
     * MUST be short/nonblocking and use the same clock as producer timestamps.
     * Wire this for asynchronous producers: step's entry-time snapshot can be
     * older than an event which arrives while that same step is draining queues.
     * NULL uses the supplied now_ms unchanged and is suitable only when inputs
     * are synchronously bounded by that timestamp. Never use wall/server time. */
    uint32_t (*now_ms)(void *context);
} bot_runtime_ops_t;

typedef struct bot_runtime bot_runtime_t;
bot_runtime_config_t bot_runtime_default_config(void);
esp_err_t bot_runtime_create(bot_bus_t *bus, const bot_runtime_config_t *config,
                             const bot_runtime_ops_t *ops, void *context,
                             uint32_t now_ms, bot_runtime_t **out);
void bot_runtime_destroy(bot_runtime_t *runtime);
/** now_ms is the fallback clock value when ops.now_ms is NULL. */
void bot_runtime_step(bot_runtime_t *runtime, uint32_t now_ms);
/** Owner task only. Other tasks should copy from apply_state callback. */
void bot_runtime_get_snapshot(const bot_runtime_t *runtime, bot_runtime_snapshot_t *out);
/** Owner-only explicit recovery after application has dealt with the overflow/fault.
 * Does not restore old session/command authority or automatically wake the bot. */
void bot_runtime_clear_fault(bot_runtime_t *runtime, uint32_t now_ms);
const char *bot_behavior_name(bot_behavior_t behavior);
const char *bot_conversation_name(bot_conversation_t conversation);
const char *bot_command_result_name(bot_command_result_t result);
#ifdef __cplusplus
}
#endif

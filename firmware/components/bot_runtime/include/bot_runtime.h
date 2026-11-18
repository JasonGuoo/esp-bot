/** 小呆 local behavior authority. Single owner calls step at ~20 ms.
 * Producers only post bus events/commands. No inference, rendering or drivers here. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"
#include "bot_bus.h"
#include "attention_manager.h"
#include "bot_responses.h"
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
    BOT_CONVERSATION_WAITING, BOT_CONVERSATION_ENDING,
    BOT_CONVERSATION_SUBMITTED /* capture sealed; awaiting authenticated processing */
} bot_conversation_t;
typedef enum {
    BOT_ACTIVATION_NONE = 0, BOT_ACTIVATION_TOUCH, BOT_ACTIVATION_WAKE_WORD,
    BOT_ACTIVATION_PROACTIVE /* runtime-chosen moment for a Mac offer; speaker only */
} bot_activation_t;
typedef enum {
    BOT_COMMAND_ACCEPTED = 0, BOT_COMMAND_REJECTED_STALE,
    BOT_COMMAND_REJECTED_EXPIRED, BOT_COMMAND_REJECTED_UNTRUSTED,
    BOT_COMMAND_REJECTED_PASSIVE, BOT_COMMAND_REJECTED_SETTINGS,
    BOT_COMMAND_REJECTED_INVALID, BOT_COMMAND_REJECTED_FAULT
} bot_command_result_t;

typedef struct {
    bot_behavior_t behavior;
    bot_conversation_t conversation;
    bot_activation_t activation; /* origin accepted by the runtime, not a VAD inference */
    bot_session_token_t token;
    bot_focus_snapshot_t focus;
    bot_wake_status_t wake_status;
    bool settings_open;
    uint32_t settings_lease, settings_revision;
    bot_voice_settings_t voice_settings;
    uint32_t remote_volume_request_id;
    uint8_t remote_volume_result; /* 0 none, 1 accepted, 2 rejected */
    bool sound_preview_active; /* explicit settings audition; no microphone/session lease */
    bool capture_ready;        /* AUDIO confirms current token's DMA fence is clear */
    bool transport_ready;      /* LINK confirms current token's authenticated media path */
    bool supplement_turn;     /* explicitly adds input to the previous foreground turn */
    bool followup_armed;       /* next token allocated before follow-up speech starts */
    bool display_awake_requested, camera_requested;
    bool audio_session_requested; /* local post-wake permission, never opens a second mic */
    /* Unprompted line (UX review §12.2): a speaker-only output lease. Never
     * implies audio_session_requested, capture, VAD or uplink. */
    bool proactive_active;
    uint32_t proactive_offer_id;     /* offer spoken by this session, zero otherwise */
    uint32_t proactive_report_id;    /* last offer that ended without being spoken */
    uint8_t proactive_report_status; /* 0 none, 1 expired unseen, 2 shown on screen */
    uint8_t proactive_report_seq;    /* changes with every report */
    bool audio_uplink_permitted;  /* session + current secure link + capture availability */
    bool visual_tracking_permitted;
    bool person_visible;         /* fresh valid face, not identity or conversation intent */
    bool near_face;              /* large nearby face, not a gaze/consent claim */
    uint16_t near_face_area_permille; /* largest fresh face box / full image */
    bool sound_reaction, sound_direction_valid;
    bool touch_gaze_valid;       /* idle finger drag; the eyes follow, no session */
    int8_t touch_gaze_x, touch_gaze_y;
    uint8_t blink_request;       /* changes when the face should double blink */
    float sound_azimuth_deg;     /* two-microphone direction; no elevation or identity */
    bool link_connected, link_authenticated, link_encrypted;
    bool audio_available, camera_available;
    bool display_awake, display_sleep_supported, display_full_blackout_verified;
    bool clock_valid, night_policy;
    uint8_t local_hour;          /* valid only with clock_valid; local configured timezone */
    bool ambient_dark, ambient_sample_valid, ambient_probe;
    uint8_t ambient_luma;        /* camera image mean, not calibrated lux */
    bool critical_fault;
    bool fault_recoverable;     /* queue overflow only; Stop/touch recovers to passive after drain */
    bool utterance_active;
    bool utterance_seen;        /* sticky for this token after an accepted VAD START */
    bool local_response_active; /* queued/playing wake acknowledgement; microphone gate closed */
    uint32_t playback_id; /* current local playback operation; zero means none */
    bot_wait_stage_t wait_stage; /* never an output lease or a playback receipt */
    uint32_t wait_started_ms, last_interaction_ms;
    uint32_t rejected_commands, rejected_events;
    char expression[BOT_EXPRESSION_MAX_BYTES + 1];
    char caption[BOT_CAPTION_MAX_BYTES + 1];
    /* Last completed visual reply. Its lifetime is independent of microphone permission. */
    bot_session_token_t reading_token;
    uint32_t reading_sequence; /* presentation segment generation, zero for legacy */
    char reading_text[BOT_CAPTION_MAX_BYTES + 1];
    /* Terminal notices survive automatic session expiry with the retained reply.
     * A new user task or explicit dismissal clears them. Neither grants audio permission. */
    char reading_notice[BOT_CAPTION_MAX_BYTES + 1];
    char image_notice[BOT_CAPTION_MAX_BYTES + 1];
} bot_runtime_snapshot_t;

typedef struct {
    uint32_t daytime_awake_ms, daytime_drowsy_ms;
    uint32_t waking_ms;
    /* Maximum gap between meaningful conversation transitions. A bounded,
     * matching AUDIO hardware-write progress event also refreshes playback.
     * Capture/network PCM and cosmetic updates never refresh this deadline. */
    uint32_t session_timeout_ms, wake_debounce_ms;
    uint32_t event_max_age_ms, command_max_age_ms;
    uint16_t max_events_per_step, max_commands_per_step;
    /* Proactive moment: face seen this long, no nearby speech for this long,
     * no interaction for this long, and this gap between unprompted lines. */
    uint32_t proactive_presence_ms, proactive_speech_quiet_ms;
    uint32_t proactive_idle_ms, proactive_min_gap_ms;
    bool allow_audio_uplink;
    bool allow_barge_in;
    bot_voice_settings_t voice_settings;
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
    /** Queue one embedded acknowledgement after an accepted acoustic wake.
     * MUST be nonblocking. The runtime has already closed the capture gate and
     * applied the selected expression/caption. false means nothing was queued.
     * Playback events keep the supplied token and local_response=true; a queue
     * or decoder failure posts BOT_EVENT_LOCAL_RESPONSE_FAILED from SYSTEM.
     * stop_output MUST invalidate any queued job as well as hardware playback. */
    bool (*play_wake_response)(void *context, const bot_session_token_t *token,
                               const bot_response_entry_t *entry, uint8_t gain_percent);
    /** Explicit settings-only audition, token has an epoch and no session/turn.
     * Uses the same cancellable output owner without authorizing microphone input. */
    bool (*play_sound_preview)(void *context, const bot_session_token_t *token,
                              const bot_response_entry_t *entry, uint8_t gain_percent);
    bool (*set_theme)(void *context, const char *theme_id);
    /** Short/nonblocking hardware-tail check AFTER the real local DONE event.
     * Keeps the listening UI/session closed until self-echo cleanup finishes.
     * NULL means that the adapter's DONE already includes the tail boundary. */
    bool (*wake_response_tail_clear)(void *context);
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
/** Owner task only: a fresh, successfully captured camera brightness sample.
 * Timestamp is local monotonic capture time, never the time of an old frame. */
void bot_runtime_observe_light(bot_runtime_t *runtime, uint8_t mean_luma,
                               uint32_t captured_ms, uint32_t now_ms);
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

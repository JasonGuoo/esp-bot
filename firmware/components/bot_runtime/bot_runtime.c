#include "bot_runtime.h"
#include "ambient_light.h"
#include "idle_expression.h"
#include "near_face.h"
#include "passive_cycle.h"
#include "sdkconfig.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>

struct bot_runtime {
    bot_bus_t *bus;
    bot_runtime_config_t config;
    bot_runtime_ops_t ops;
    void *context;
    bot_runtime_snapshot_t state, published;
    attention_manager_t attention;
    vatt_snapshot_t vision;
    bool has_vision, has_published, has_wake, wake_barrier_valid;
    bool has_acoustic_wake, has_link_state;
    bool revoking, identity_exhausted, waiting_for_approval, manual_sleep;
    bool reply_presented, remote_audio_done, presentation_done_pending;
    bool overflow_fault, recovery_requested;
    bot_passive_cycle_t passive_cycle;
    bot_idle_expression_t idle_expression;
    bot_near_face_t near_face;
    bot_face_presence_t face_presence;
    bool face_reaction_active;
    uint32_t face_reaction_started_ms;
    bot_ambient_light_t ambient;
    bot_responses_state_t responses;
    const bot_response_entry_t *response, *last_response;
    /* The Mac's validated per-segment emotion hint; preferred only while SPEAKING. */
    char segment_expression[BOT_EXPRESSION_MAX_BYTES + 1];
    bool response_playback_done;
    uint32_t response_started_ms, response_timeout_ms;
    uint32_t waking_since_ms, last_wake_ms, wake_barrier_ms, next_session_id;
    uint32_t last_acoustic_wake_ms;
    uint32_t last_sound_ms;
    uint32_t event_times[BOT_EVENT_COUNT];
    bool event_seen[BOT_EVENT_COUNT];
    uint32_t capture_event_times[2];
    bool capture_event_seen[2];
};

static bool within_age(uint32_t now_ms, uint32_t observed_ms, uint32_t limit_ms)
{
    int32_t age = (int32_t)(now_ms - observed_ms);
    return age >= 0 && (uint32_t)age <= limit_ms;
}

static uint32_t read_now_ms(const bot_runtime_t *runtime, uint32_t fallback_ms)
{
    return runtime->ops.now_ms ? runtime->ops.now_ms(runtime->context) : fallback_ms;
}

static bool is_active(const bot_runtime_t *runtime)
{
    return runtime->state.behavior == BOT_BEHAVIOR_WAKING ||
           runtime->state.behavior == BOT_BEHAVIOR_ENGAGED;
}

static bool secure_link(const bot_runtime_t *runtime)
{
    return runtime->state.link_connected && runtime->state.link_authenticated &&
           runtime->state.link_encrypted;
}

static void reset_expression(bot_runtime_t *runtime)
{
    if (runtime->state.local_response_active && runtime->response) {
        const char *expression = runtime->response->expression;
        memcpy(runtime->state.expression, expression, strlen(expression) + 1);
        return;
    }
    const char *expression = "idle";
    const bot_conversation_t conversation = runtime->state.conversation;
    if (runtime->state.behavior != BOT_BEHAVIOR_ENGAGED ||
        (conversation != BOT_CONVERSATION_THINKING && conversation != BOT_CONVERSATION_SPEAKING))
        runtime->segment_expression[0] = '\0'; /* a hint never outlives its answer */
    switch (runtime->state.behavior) {
    case BOT_BEHAVIOR_BORED: expression = "bored"; break;
    case BOT_BEHAVIOR_DROWSY: expression = "drowsy"; break;
    case BOT_BEHAVIOR_SLEEPING:
    case BOT_BEHAVIOR_NIGHT_DARK: expression = "sleeping"; break;
    case BOT_BEHAVIOR_WAKING: expression = "surprised"; break;
    case BOT_BEHAVIOR_ENGAGED:
        switch (runtime->state.conversation) {
        case BOT_CONVERSATION_LISTENING: expression = "listening"; break;
        case BOT_CONVERSATION_SUBMITTED: expression = "listening"; break;
        case BOT_CONVERSATION_THINKING: expression = "thinking"; break;
        case BOT_CONVERSATION_SPEAKING:
            expression = runtime->segment_expression[0] ? runtime->segment_expression : "speaking";
            break;
        case BOT_CONVERSATION_WAITING: expression = "curious"; break;
        default: break;
        }
        break;
    default: break;
    }
    memcpy(runtime->state.expression, expression, strlen(expression) + 1);
}

static void derive_permissions(bot_runtime_t *runtime)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    state->fault_recoverable = state->critical_fault && runtime->overflow_fault && !runtime->identity_exhausted;
    bool engaged = is_active(runtime) && !state->settings_open && !state->critical_fault &&
                   state->token.session_id != 0;
    state->display_awake_requested = state->settings_open ||
        (state->reading_token.session_id && !state->night_policy && !state->ambient_dark) ||
        (state->behavior != BOT_BEHAVIOR_NIGHT_DARK && state->behavior != BOT_BEHAVIOR_SLEEPING);
    /* Local daytime perception supports the requested close-face expression.
     * Face presence never creates a session or enables audio/network uplink. */
    const bool passive_camera = !is_active(runtime) && !state->settings_open &&
        !state->critical_fault && (state->behavior == BOT_BEHAVIOR_IDLE ||
        state->behavior == BOT_BEHAVIOR_BORED || state->behavior == BOT_BEHAVIOR_DROWSY);
    state->camera_requested = engaged || passive_camera ||
        (state->behavior == BOT_BEHAVIOR_SLEEPING && state->ambient_probe &&
         !state->settings_open && !state->critical_fault);
    state->audio_session_requested = engaged && !runtime->revoking && !state->local_response_active;
    const bool listening = state->conversation == BOT_CONVERSATION_LISTENING ||
                           state->conversation == BOT_CONVERSATION_WAITING;
    state->audio_uplink_permitted = listening && state->audio_session_requested && runtime->config.allow_audio_uplink &&
                                   state->audio_available && secure_link(runtime) &&
                                   state->utterance_seen &&
                                   (!state->followup_armed || state->utterance_active);
    if (!state->audio_session_requested) {
        state->capture_ready = false;
        state->transport_ready = false;
    }
    state->visual_tracking_permitted = engaged && state->camera_available;
    attention_manager_set_policy(&runtime->attention, state->visual_tracking_permitted, engaged);
    if (!state->visual_tracking_permitted) state->focus = runtime->attention.focus;
}

static void publish_state(bot_runtime_t *runtime)
{
    if (!runtime->has_published || memcmp(&runtime->published, &runtime->state,
                                         sizeof(runtime->state)) != 0) {
        runtime->published = runtime->state;
        runtime->has_published = true;
        if (runtime->ops.apply_state) runtime->ops.apply_state(runtime->context, &runtime->state);
    }
}

static bool token_matches(const bot_runtime_t *runtime, const bot_session_token_t *token,
                           bool bind_focus)
{
    return token->session_id != 0 && token->turn_id != 0 &&
        token->epoch == runtime->state.token.epoch &&
        token->session_id == runtime->state.token.session_id &&
        token->turn_id == runtime->state.token.turn_id &&
        (!bind_focus || token->focus_generation == 0 ||
         (runtime->state.focus.localization_valid &&
          token->focus_generation == runtime->state.focus.generation));
}

static bool advance_epoch(bot_runtime_t *runtime)
{
    if (runtime->state.token.epoch == UINT32_MAX) {
        /* A reboot may establish a new transport handshake; silently wrapping
         * would make an ancient token reusable within this process. */
        runtime->identity_exhausted = true;
        runtime->state.critical_fault = true;
        return false;
    }
    runtime->state.token.epoch++;
    return true;
}

static void set_passive(bot_runtime_t *runtime, uint32_t now_ms)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    const bot_passive_phase_t phase = bot_passive_cycle_step(&runtime->passive_cycle, now_ms,
        state->clock_valid && state->night_policy,
        runtime->config.daytime_awake_ms, runtime->config.daytime_drowsy_ms);
    bot_behavior_t next = runtime->manual_sleep ? BOT_BEHAVIOR_SLEEPING :
                          phase == BOT_PASSIVE_NIGHT ? BOT_BEHAVIOR_NIGHT_DARK :
                          runtime->ambient.dark ? BOT_BEHAVIOR_SLEEPING :
                          phase == BOT_PASSIVE_DROWSY ? BOT_BEHAVIOR_DROWSY : BOT_BEHAVIOR_IDLE;
    if (state->behavior != next || state->conversation != BOT_CONVERSATION_NONE) {
        state->behavior = next;
        state->conversation = BOT_CONVERSATION_NONE;
        reset_expression(runtime);
    }
}

static void invalidate_output(bot_runtime_t *runtime, const bot_session_token_t *old_token)
{
    /* This callback publishes the closed gate before touching a playback queue.
     * A transport/audio adapter must atomically replace its permission/token. */
    runtime->revoking = true;
    derive_permissions(runtime);
    publish_state(runtime);
    if (runtime->ops.stop_output) runtime->ops.stop_output(runtime->context);
    if (old_token->session_id && runtime->ops.cancel_remote)
        runtime->ops.cancel_remote(runtime->context, old_token);
    runtime->revoking = false;
}

typedef enum { SESSION_DISMISSED, SESSION_INACTIVE, SESSION_FAULT } session_end_reason_t;

static void dismiss_result(bot_runtime_snapshot_t *state)
{
    state->reading_token = (bot_session_token_t){0};
    state->reading_text[0] = 0;
    state->reading_notice[0] = 0;
    state->image_notice[0] = 0;
}

static void end_session(bot_runtime_t *runtime, uint32_t now_ms, session_end_reason_t reason)
{
    bot_session_token_t old = runtime->state.token;
    runtime->state.critical_fault |= reason == SESSION_FAULT;
    if (reason != SESSION_INACTIVE) dismiss_result(&runtime->state);
    else if (runtime->state.reading_token.session_id &&
             !token_matches(runtime, &runtime->state.reading_token, false) &&
             !runtime->state.reading_notice[0]) {
        const char *notice = runtime->state.conversation == BOT_CONVERSATION_THINKING ?
            "处理已超时，请再试一次" : "本次收音已结束，点一下可再问";
        memcpy(runtime->state.reading_notice, notice, strlen(notice) + 1);
    }
    runtime->state.wait_stage = BOT_WAIT_NONE;
    runtime->waiting_for_approval = false;
    runtime->reply_presented = false;
    runtime->remote_audio_done = false;
    runtime->presentation_done_pending = false;
    advance_epoch(runtime);
    runtime->state.token.session_id = 0;
    runtime->state.token.turn_id = 0;
    runtime->state.token.focus_generation = 0;
    runtime->state.activation = BOT_ACTIVATION_NONE;
    runtime->state.utterance_active = false;
    runtime->state.utterance_seen = false;
    runtime->state.playback_id = 0;
    runtime->state.local_response_active = false;
    runtime->state.sound_preview_active = false;
    runtime->state.followup_armed = false;
    runtime->state.capture_ready = runtime->state.transport_ready = false;
    runtime->response = NULL;
    runtime->response_playback_done = false;
    runtime->state.last_interaction_ms = now_ms;
    bot_passive_cycle_reset(&runtime->passive_cycle, now_ms);
    runtime->state.caption[0] = '\0';
    runtime->wake_barrier_valid = true;
    runtime->wake_barrier_ms = now_ms;
    runtime->state.conversation = BOT_CONVERSATION_ENDING;
    invalidate_output(runtime, &old);
    set_passive(runtime, now_ms);
    derive_permissions(runtime);
}

static bool begin_turn_with_intent(bot_runtime_t *runtime, uint32_t now_ms, bool supplement)
{
    bot_session_token_t old = runtime->state.token;
    runtime->state.wait_stage = BOT_WAIT_NONE;
    runtime->waiting_for_approval = false;
    runtime->reply_presented = false;
    runtime->remote_audio_done = false;
    runtime->presentation_done_pending = false;
    if (runtime->state.token.turn_id == UINT32_MAX || !advance_epoch(runtime)) {
        runtime->identity_exhausted = true;
        end_session(runtime, now_ms, SESSION_FAULT);
        return false;
    }
    runtime->state.token.turn_id++;
    runtime->state.token.focus_generation = 0;
    runtime->state.utterance_active = false;
    runtime->state.utterance_seen = false;
    runtime->state.playback_id = 0;
    runtime->state.local_response_active = false;
    runtime->state.followup_armed = false;
    runtime->state.supplement_turn = supplement;
    runtime->state.capture_ready = runtime->state.transport_ready = false;
    runtime->response = NULL;
    runtime->response_playback_done = false;
    runtime->state.caption[0] = '\0';
    runtime->state.reading_notice[0] = '\0';
    runtime->state.image_notice[0] = '\0';
    invalidate_output(runtime, &old);
    runtime->state.conversation = BOT_CONVERSATION_LISTENING;
    runtime->state.last_interaction_ms = now_ms;
    reset_expression(runtime);
    derive_permissions(runtime);
    return true;
}

static bool begin_turn(bot_runtime_t *runtime, uint32_t now_ms)
{
    return begin_turn_with_intent(runtime, now_ms, false);
}

static bool arm_followup(bot_runtime_t *runtime, uint32_t now_ms)
{
    /* Allocate the new authorization BEFORE any next-turn audio is retained.
     * The old visual result has its own reading_token and remains on screen.
     * No intermediate inactive snapshot: the completed result's image transfer
     * and hardware receipt must survive this preparation of the next turn. */
    if (runtime->state.token.turn_id == UINT32_MAX || !advance_epoch(runtime)) {
        runtime->identity_exhausted = true;
        end_session(runtime, now_ms, SESSION_FAULT);
        return false;
    }
    runtime->state.token.turn_id++;
    runtime->state.token.focus_generation = 0;
    runtime->state.utterance_active = false;
    runtime->state.utterance_seen = false;
    runtime->state.playback_id = 0;
    runtime->state.local_response_active = false;
    runtime->state.capture_ready = runtime->state.transport_ready = false;
    runtime->response = NULL;
    runtime->response_playback_done = false;
    runtime->reply_presented = runtime->remote_audio_done = false;
    runtime->presentation_done_pending = false;
    runtime->state.wait_stage = BOT_WAIT_NONE;
    runtime->waiting_for_approval = false;
    runtime->state.caption[0] = '\0';
    runtime->state.last_interaction_ms = now_ms;
    runtime->state.supplement_turn = false;
    runtime->state.followup_armed = true;
    runtime->state.conversation = BOT_CONVERSATION_WAITING;
    reset_expression(runtime);
    derive_permissions(runtime);
    return true;
}

static bool begin_session(bot_runtime_t *runtime, uint32_t now_ms)
{
    runtime->manual_sleep = false;
    runtime->state.wait_stage = BOT_WAIT_NONE;
    runtime->waiting_for_approval = false;
    runtime->reply_presented = false;
    runtime->remote_audio_done = false;
    runtime->presentation_done_pending = false;
    if (runtime->next_session_id == UINT32_MAX || !advance_epoch(runtime)) {
        runtime->identity_exhausted = true;
        end_session(runtime, now_ms, SESSION_FAULT);
        return false;
    }
    runtime->next_session_id++;
    runtime->state.token.session_id = runtime->next_session_id;
    runtime->state.token.turn_id = 1;
    runtime->state.token.focus_generation = 0;
    runtime->state.behavior = BOT_BEHAVIOR_WAKING;
    runtime->state.conversation = BOT_CONVERSATION_LISTENING;
    runtime->state.utterance_active = false;
    runtime->state.utterance_seen = false;
    runtime->state.playback_id = 0;
    runtime->state.local_response_active = false;
    runtime->state.sound_preview_active = false;
    runtime->state.supplement_turn = false;
    runtime->state.followup_armed = false;
    runtime->state.capture_ready = runtime->state.transport_ready = false;
    runtime->response = NULL;
    runtime->response_playback_done = false;
    runtime->state.last_interaction_ms = now_ms;
    bot_passive_cycle_reset(&runtime->passive_cycle, now_ms);
    runtime->state.caption[0] = '\0';
    runtime->state.reading_notice[0] = '\0';
    runtime->state.image_notice[0] = '\0';
    runtime->waking_since_ms = now_ms;
    /* A new authorization starts with a new visual focus as well. */
    attention_manager_set_policy(&runtime->attention, false, false);
    runtime->has_vision = false;
    reset_expression(runtime);
    derive_permissions(runtime);
    return true;
}

static bool consume_signals(bot_runtime_t *runtime, uint32_t *now_ms)
{
    bot_bus_signals_t signals = bot_bus_take_signals(runtime->bus);
    /* Read AFTER taking queued input/signals. A producer may have posted after
     * step() began; comparing that event with the entry clock would reject a
     * legitimate one-shot health transition as a future timestamp. Do not infer
     * time from event data: within_age still rejects truly future observations. */
    *now_ms = read_now_ms(runtime, *now_ms);
    if (!signals.local_stop && !signals.critical_overflow) return false;
    if (signals.critical_overflow) {
        /* An explicit hardware/system fault must never become recoverable just
         * because another producer also overflows while it is latched. */
        if (!runtime->state.critical_fault) runtime->overflow_fault = true;
        runtime->recovery_requested = false;
    } else if (signals.local_stop && runtime->overflow_fault) {
        runtime->recovery_requested = true;
    }
    end_session(runtime, *now_ms, signals.critical_overflow ? SESSION_FAULT : SESSION_DISMISSED);
    publish_state(runtime);
    return true;
}

static bool local_intent(bot_event_source_t source)
{
    return source == BOT_SOURCE_LOCAL_UI || source == BOT_SOURCE_PLATFORM;
}

static bool source_allowed(const bot_event_t *event)
{
    switch (event->kind) {
    case BOT_EVENT_WAKE_DETECTED:
    case BOT_EVENT_BARGE_IN:
    case BOT_EVENT_UTTERANCE_STARTED:
    case BOT_EVENT_UTTERANCE_ENDED:
    case BOT_EVENT_PLAYBACK_STARTED:
    case BOT_EVENT_PLAYBACK_DONE:
    case BOT_EVENT_PLAYBACK_FAILED:
    case BOT_EVENT_PLAYBACK_PROGRESS:
    case BOT_EVENT_WAKE_STATUS:
    case BOT_EVENT_SOUND_ACTIVITY:
    case BOT_EVENT_LOCAL_COMMAND:
    case BOT_EVENT_CAPTURE_OVERFLOW:
        return event->source == BOT_SOURCE_AUDIO;
    case BOT_EVENT_CAPTURE_READY:
        return event->source == BOT_SOURCE_AUDIO || event->source == BOT_SOURCE_LINK;
    case BOT_EVENT_TURN_SUBMITTED:
    case BOT_EVENT_REMOTE_VOLUME:
        return event->source == BOT_SOURCE_LINK;
    case BOT_EVENT_AUDIO_HEALTH:
        return event->source == BOT_SOURCE_AUDIO || event->source == BOT_SOURCE_PLATFORM;
    case BOT_EVENT_TOUCH_WAKE:
    case BOT_EVENT_LOCAL_STOP:
    case BOT_EVENT_RESULT_DISMISSED:
    case BOT_EVENT_SETTINGS_OPEN:
    case BOT_EVENT_SETTINGS_CLOSE:
    case BOT_EVENT_VOICE_SETTINGS:
    case BOT_EVENT_SOUND_PREVIEW:
    case BOT_EVENT_NIGHT_POLICY:
        return local_intent(event->source);
    case BOT_EVENT_CAMERA_HEALTH:
        return event->source == BOT_SOURCE_VISION || event->source == BOT_SOURCE_PLATFORM;
    case BOT_EVENT_DISPLAY_HEALTH:
        return event->source == BOT_SOURCE_PLATFORM;
    case BOT_EVENT_LINK_STATE:
        return event->source == BOT_SOURCE_LINK || event->source == BOT_SOURCE_PLATFORM;
    case BOT_EVENT_SESSION_END:
        return event->source == BOT_SOURCE_AUDIO || event->source == BOT_SOURCE_SYSTEM;
    case BOT_EVENT_LOCAL_RESPONSE_FAILED:
        return event->source == BOT_SOURCE_SYSTEM;
    case BOT_EVENT_FAULT:
        return event->source == BOT_SOURCE_SYSTEM || event->source == BOT_SOURCE_PLATFORM ||
               event->source == BOT_SOURCE_AUDIO || event->source == BOT_SOURCE_VISION;
    default: return false;
    }
}

static bool may_wake(const bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms)
{
    return !runtime->state.settings_open && !runtime->state.critical_fault &&
        within_age(now_ms, event->observed_ms, runtime->config.event_max_age_ms) &&
        (!runtime->wake_barrier_valid || (int32_t)(event->observed_ms - runtime->wake_barrier_ms) > 0) &&
        (!runtime->has_wake || ((int32_t)(event->observed_ms - runtime->last_wake_ms) >= 0 &&
         (uint32_t)(event->observed_ms - runtime->last_wake_ms) >= runtime->config.wake_debounce_ms));
}

static bot_response_context_t response_context(const bot_runtime_t *runtime, uint32_t now_ms)
{
    const bot_runtime_snapshot_t *state = &runtime->state;
    bot_response_context_t context = {
        .now_ms = now_ms,
        .time_valid = state->clock_valid && state->local_hour < 24,
        .local_hour = state->local_hour,
        .night_dark = state->clock_valid && state->night_policy,
        .was_sleeping = state->behavior == BOT_BEHAVIOR_SLEEPING ||
                        state->behavior == BOT_BEHAVIOR_NIGHT_DARK ||
                        state->behavior == BOT_BEHAVIOR_DROWSY,
        .ambient_sample_valid = state->ambient_sample_valid,
        .ambient_dark = state->ambient_dark,
        .face_observation_valid = runtime->has_vision && state->camera_available &&
            runtime->vision.state != VATT_UNAVAILABLE &&
            within_age(now_ms, runtime->vision.timestamp_ms,
                       runtime->config.focus.observation_max_age_ms),
        .brain_status_valid = runtime->has_link_state,
        .brain_online = secure_link(runtime),
        .repeated_wake = runtime->has_acoustic_wake &&
                         within_age(now_ms, runtime->last_acoustic_wake_ms, 60000),
    };
    if (context.face_observation_valid) {
        for (size_t i = 0; i < runtime->vision.face_count && i < VATT_MAX_FACES; i++) {
            const vatt_face_t *face = &runtime->vision.faces[i];
            if (face->visible && isfinite(face->observation.face_confidence) &&
                face->observation.face_confidence >= runtime->config.focus.min_face_confidence &&
                within_age(now_ms, face->last_seen_ms, runtime->config.focus.observation_max_age_ms)) {
                context.face_visible = true;
                break;
            }
        }
    }
    return context;
}

static void finish_local_response(bot_runtime_t *runtime, uint32_t now_ms)
{
    runtime->state.local_response_active = false;
    runtime->response = NULL;
    runtime->response_playback_done = false;
    runtime->state.playback_id = 0;
    runtime->state.conversation = BOT_CONVERSATION_LISTENING;
    runtime->state.caption[0] = '\0';
    runtime->state.last_interaction_ms = now_ms;
    reset_expression(runtime);
    derive_permissions(runtime);
}

static uint8_t response_gain(const bot_runtime_t *runtime, const bot_response_entry_t *entry)
{
    return runtime->state.clock_valid && runtime->state.night_policy ?
        runtime->state.voice_settings.night_gain_percent : entry->gain_percent;
}

static void start_local_response(bot_runtime_t *runtime, const bot_response_entry_t *entry,
                                 uint32_t now_ms)
{
    if (!entry || !runtime->ops.play_wake_response || !runtime->state.audio_available ||
        runtime->state.voice_settings.muted || !runtime->state.voice_settings.volume_percent) return;
    runtime->response = entry;
    runtime->state.local_response_active = true;
    runtime->response_playback_done = false;
    runtime->response_started_ms = now_ms;
    runtime->response_timeout_ms = entry->pcm_samples / 16U + 5000U;
    if (runtime->response_timeout_ms > 15000U) runtime->response_timeout_ms = 15000U;
    memcpy(runtime->state.caption, entry->text, strlen(entry->text) + 1);
    reset_expression(runtime);
    derive_permissions(runtime);
    publish_state(runtime);
    if (!runtime->ops.play_wake_response(runtime->context, &runtime->state.token, entry,
                                        response_gain(runtime, entry)))
        finish_local_response(runtime, now_ms);
}

static void stop_preview(bot_runtime_t *runtime)
{
    runtime->state.sound_preview_active = false;
    runtime->state.playback_id = 0;
    runtime->response = NULL;
    if (runtime->ops.stop_output) runtime->ops.stop_output(runtime->context);
}

static bool preview_token_matches(const bot_runtime_t *runtime, const bot_session_token_t *token)
{
    return runtime->state.sound_preview_active && runtime->state.settings_open &&
        !token->session_id && !token->turn_id && token->epoch == runtime->state.token.epoch;
}

static void apply_local_command(bot_runtime_t *runtime, bot_voice_command_t command, uint32_t now_ms)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    if (command == BOT_VOICE_COMMAND_STOP || command == BOT_VOICE_COMMAND_SLEEP) {
        runtime->manual_sleep = command == BOT_VOICE_COMMAND_SLEEP;
        end_session(runtime, now_ms, SESSION_DISMISSED);
        return;
    }
    if (command == BOT_VOICE_COMMAND_SUPPLEMENT) {
        dismiss_result(state);
        begin_turn_with_intent(runtime,now_ms,true);
        return;
    }
    /* Recognition consumed this utterance locally. Revoke the old remote turn
     * before applying an action, even when the Mac is unavailable. */
    const bot_session_token_t old = state->token;
    invalidate_output(runtime, &old);
    if (!arm_followup(runtime, now_ms)) return;
    switch (command) {
    case BOT_VOICE_COMMAND_VOLUME_UP:
        state->voice_settings.volume_percent = state->voice_settings.volume_percent > BOT_VOICE_VOLUME_MAX - 5 ?
            BOT_VOICE_VOLUME_MAX : state->voice_settings.volume_percent + 5;
        state->voice_settings.muted = false;
        strcpy(state->caption, "声音调大了");
        break;
    case BOT_VOICE_COMMAND_VOLUME_DOWN:
        state->voice_settings.volume_percent = state->voice_settings.volume_percent < 5 ?
            0 : state->voice_settings.volume_percent - 5;
        strcpy(state->caption, "声音调小了");
        break;
    case BOT_VOICE_COMMAND_QUIET:
        state->voice_settings.muted = true;
        strcpy(state->caption, "已经静音");
        break;
    case BOT_VOICE_COMMAND_UNMUTE:
        state->voice_settings.muted = false;
        if (!state->voice_settings.volume_percent) state->voice_settings.volume_percent = 30;
        strcpy(state->caption, "声音已打开");
        break;
    case BOT_VOICE_COMMAND_THEME_BLUE:
        strcpy(state->caption, runtime->ops.set_theme && runtime->ops.set_theme(runtime->context, "sky") ?
            "换成蓝色了" : "外观切换失败，请在设置中重试");
        break;
    case BOT_VOICE_COMMAND_HAPPY:
        strcpy(state->caption, "给你一个笑脸");
        strcpy(state->expression, "happy");
        break;
    case BOT_VOICE_COMMAND_REPEAT:
        if (runtime->last_response && !state->voice_settings.muted && state->voice_settings.volume_percent) {
            state->followup_armed = false;
            start_local_response(runtime, runtime->last_response, now_ms);
        } else strcpy(state->caption, "暂时没有可以重播的本地应答");
        break;
    default: break;
    }
}

static void mark_wake(bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms)
{
    /* Capture context before begin_session retires old vision and sleep state. */
    const bot_response_context_t context = response_context(runtime, now_ms);
    runtime->manual_sleep = false;
    bool accepted = is_active(runtime) ? begin_turn(runtime, now_ms) : begin_session(runtime, now_ms);
    if (!accepted) return;
    runtime->last_wake_ms = event->observed_ms;
    runtime->has_wake = true;
    runtime->state.activation = event->kind == BOT_EVENT_TOUCH_WAKE ?
        BOT_ACTIVATION_TOUCH : BOT_ACTIVATION_WAKE_WORD;
    if (event->kind == BOT_EVENT_WAKE_DETECTED) {
        runtime->has_acoustic_wake = true;
        runtime->last_acoustic_wake_ms = event->observed_ms;
    }
    if (event->kind != BOT_EVENT_WAKE_DETECTED || !runtime->ops.play_wake_response ||
        !runtime->state.audio_available) return;
    const bot_response_entry_t *entry = bot_responses_select(&runtime->responses, &context);
    if (!entry) return;
    start_local_response(runtime, entry, now_ms);
}

static bool state_event(bot_event_kind_t kind)
{
    switch (kind) {
    case BOT_EVENT_LINK_STATE:
    case BOT_EVENT_AUDIO_HEALTH:
    case BOT_EVENT_CAMERA_HEALTH:
    case BOT_EVENT_DISPLAY_HEALTH:
    case BOT_EVENT_WAKE_STATUS:
    case BOT_EVENT_NIGHT_POLICY:
    case BOT_EVENT_TURN_SUBMITTED: /* terminal capture state; full token checked by handler */
        return true;
    default: return false;
    }
}

static bool event_order_valid(bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms)
{
    /* Change-only health/state facts survive startup and owner stalls. Keep
     * their ordering too, so a delayed older fact cannot undo a newer one.
     * Edges and token-bound readiness retain the short freshness lease. */
    const uint32_t max_age = state_event(event->kind) ? INT32_MAX : runtime->config.event_max_age_ms;
    if (!within_age(now_ms, event->observed_ms, max_age)) return false;
    if (event->kind == BOT_EVENT_CAPTURE_READY) {
        /* Audio and network are independent producers of separate readiness
         * facts. One producer's timestamp must not suppress the other one. */
        const unsigned producer = event->source == BOT_SOURCE_AUDIO ? 0 : 1;
        if (runtime->capture_event_seen[producer] &&
            within_age(now_ms, runtime->capture_event_times[producer], runtime->config.event_max_age_ms) &&
            (int32_t)(event->observed_ms - runtime->capture_event_times[producer]) < 0) return false;
        runtime->capture_event_seen[producer] = true;
        runtime->capture_event_times[producer] = event->observed_ms;
        return true;
    }
    if (runtime->event_seen[event->kind] &&
        !within_age(now_ms, runtime->event_times[event->kind], max_age))
        runtime->event_seen[event->kind] = false;
    if (runtime->event_seen[event->kind] &&
        (int32_t)(event->observed_ms - runtime->event_times[event->kind]) < 0) return false;
    runtime->event_seen[event->kind] = true;
    runtime->event_times[event->kind] = event->observed_ms;
    return true;
}

static void handle_event(bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms,
                          bool stopped_this_step)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    if (event->kind == BOT_EVENT_FAULT && source_allowed(event)) {
        /* A locally reported fatal condition does not become harmless merely
         * because its producer was delayed. Explicit local recovery is required. */
        runtime->overflow_fault = runtime->recovery_requested = false;
        end_session(runtime, now_ms, SESSION_FAULT);
        return;
    }
    if (event->kind < 0 || event->kind >= BOT_EVENT_COUNT || !source_allowed(event) ||
        !event_order_valid(runtime, event, now_ms)) {
        state->rejected_events++;
        return;
    }
    switch (event->kind) {
    case BOT_EVENT_SOUND_ACTIVITY:
        if (event->data.sound.direction_valid &&
            (!isfinite(event->data.sound.azimuth_deg) || event->data.sound.azimuth_deg < 0 ||
             event->data.sound.azimuth_deg > 180 || !isfinite(event->data.sound.quality) ||
             event->data.sound.quality < 3 || event->data.sound.quality > 10)) {
            state->rejected_events++;
            break;
        }
        if (!stopped_this_step && !is_active(runtime) && !state->settings_open &&
            !state->critical_fault && !(state->clock_valid && state->night_policy) &&
            state->behavior != BOT_BEHAVIOR_NIGHT_DARK && state->behavior != BOT_BEHAVIOR_SLEEPING) {
            state->sound_reaction = true;
            state->sound_direction_valid = event->data.sound.direction_valid;
            state->sound_azimuth_deg = event->data.sound.azimuth_deg;
            runtime->last_sound_ms = event->observed_ms;
        }
        break;
    case BOT_EVENT_WAKE_DETECTED:
        if (stopped_this_step || !may_wake(runtime, event, now_ms) ||
            state->wake_status != BOT_WAKE_READY ||
            event->data.wake.phrase != BOT_WAKE_PHRASE_XIAODAI_XIAODAI ||
            (event->data.wake.confidence_available &&
             (!isfinite(event->data.wake.confidence) || event->data.wake.confidence < 0 ||
              event->data.wake.confidence > 1)) ||
            (state->conversation == BOT_CONVERSATION_SPEAKING && !event->data.wake.echo_safe)) {
            state->rejected_events++;
            break;
        }
        mark_wake(runtime, event, now_ms);
        break;
    case BOT_EVENT_TOUCH_WAKE:
        /* The explicit replace button belongs only to the turn it was drawn for.
         * Do not let a delayed click start a new session or interrupt another turn. */
        if (event->token.session_id) {
            if (stopped_this_step || !event->data.touch.intentional || !is_active(runtime) ||
                state->settings_open || state->critical_fault || !token_matches(runtime,&event->token,false) ||
                (state->conversation!=BOT_CONVERSATION_SUBMITTED && state->conversation!=BOT_CONVERSATION_THINKING &&
                 state->conversation!=BOT_CONVERSATION_SPEAKING)) { state->rejected_events++; break; }
            dismiss_result(state);
            if (begin_turn_with_intent(runtime,now_ms,event->data.touch.supplement)) state->activation=BOT_ACTIVATION_TOUCH;
            break;
        }
        if (state->critical_fault && runtime->overflow_fault && event->data.touch.intentional) {
            runtime->recovery_requested = true;
            break; /* Recovery consumes the touch; a second action may wake. */
        }
        if (stopped_this_step || !event->data.touch.intentional || !may_wake(runtime, event, now_ms)) {
            state->rejected_events++;
            break;
        }
        mark_wake(runtime, event, now_ms);
        break;
    case BOT_EVENT_LOCAL_STOP:
        end_session(runtime, now_ms, SESSION_DISMISSED);
        break;
    case BOT_EVENT_RESULT_DISMISSED:
        dismiss_result(state);
        break;
    case BOT_EVENT_SETTINGS_OPEN:
        if (!event->settings_lease ||
            (state->settings_lease && (int32_t)(event->settings_lease - state->settings_lease) <= 0)) {
            state->rejected_events++;
            break;
        }
        state->settings_lease = event->settings_lease;
        state->settings_revision = 0;
        state->settings_open = true;
        end_session(runtime, now_ms, SESSION_DISMISSED);
        break;
    case BOT_EVENT_SETTINGS_CLOSE:
        if (!state->settings_open || event->settings_lease != state->settings_lease) {
            state->rejected_events++;
            break;
        }
        /* CLOSE is safety-critical and can overtake the normal edit queue.
         * Commit its causally captured final snapshot before retiring the page.
         * A delayed event from this or an older page can never reopen it. */
        if (event->settings_has_value && !state->critical_fault &&
            event->settings_revision >= state->settings_revision &&
            bot_voice_settings_valid(&event->data.voice_settings)) {
            state->voice_settings = event->data.voice_settings;
            state->settings_revision = event->settings_revision;
        }
        if (state->sound_preview_active) stop_preview(runtime);
        state->settings_open = false;
        state->last_interaction_ms = now_ms;
        bot_passive_cycle_reset(&runtime->passive_cycle, now_ms);
        runtime->wake_barrier_valid = true;
        runtime->wake_barrier_ms = now_ms;
        set_passive(runtime, now_ms);
        break;
    case BOT_EVENT_REMOTE_VOLUME: {
        const uint32_t request = event->data.remote_volume.request_id;
        if (!request) { state->rejected_events++; break; }
        state->remote_volume_request_id = request;
        if (!secure_link(runtime) || state->settings_open || state->critical_fault ||
            (!event->data.remote_volume.set_volume && !event->data.remote_volume.set_muted) ||
            (event->data.remote_volume.set_volume && event->data.remote_volume.volume > BOT_VOICE_VOLUME_MAX)) {
            state->remote_volume_result = 2;
            state->rejected_events++;
            break;
        }
        if (event->data.remote_volume.set_volume)
            state->voice_settings.volume_percent = event->data.remote_volume.volume;
        if (event->data.remote_volume.set_muted)
            state->voice_settings.muted = event->data.remote_volume.muted;
        state->remote_volume_result = 1;
        break;
    }
    case BOT_EVENT_VOICE_SETTINGS:
        if (!state->settings_open || state->critical_fault ||
            event->settings_lease != state->settings_lease ||
            event->settings_revision <= state->settings_revision ||
            !bot_voice_settings_valid(&event->data.voice_settings)) {
            state->rejected_events++;
            break;
        }
        if (state->sound_preview_active) stop_preview(runtime);
        state->voice_settings = event->data.voice_settings;
        state->settings_revision = event->settings_revision;
        break;
    case BOT_EVENT_SOUND_PREVIEW: {
        if (stopped_this_step || !state->settings_open || state->critical_fault ||
            event->settings_lease != state->settings_lease ||
            !state->audio_available || state->voice_settings.muted ||
            !state->voice_settings.volume_percent || !runtime->ops.play_sound_preview) {
            state->rejected_events++;
            break;
        }
        if (state->sound_preview_active) stop_preview(runtime);
        if (!advance_epoch(runtime)) break;
        const bot_response_entry_t *entry = bot_responses_get(0);
        if (!entry) break;
        state->sound_preview_active = true;
        state->playback_id = 0;
        runtime->response_started_ms = now_ms;
        runtime->response_timeout_ms = entry->pcm_samples / 16U + 5000U;
        derive_permissions(runtime);
        publish_state(runtime);
        if (!runtime->ops.play_sound_preview(runtime->context, &state->token, entry,
                                             response_gain(runtime, entry))) stop_preview(runtime);
        break;
    }
    case BOT_EVENT_CAPTURE_READY:
        if (!state->audio_session_requested || !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        if (event->source == BOT_SOURCE_AUDIO) state->capture_ready = event->data.capture.ready;
        else state->transport_ready = event->data.capture.ready && secure_link(runtime);
        break;
    case BOT_EVENT_CAPTURE_OVERFLOW:
        if (!state->audio_session_requested || !token_matches(runtime, &event->token, false) ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_WAITING)) {
            state->rejected_events++;
            break;
        }
        if (arm_followup(runtime, now_ms))
            strcpy(state->caption, "连接还没准备好，请等就绪后再说一次");
        break;
    case BOT_EVENT_LOCAL_COMMAND:
        if (stopped_this_step || !is_active(runtime) || state->settings_open || state->critical_fault ||
            state->local_response_active || !state->audio_available ||
            !token_matches(runtime, &event->token, false) ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_WAITING &&
             !((event->data.local_command.command == BOT_VOICE_COMMAND_STOP || event->data.local_command.command == BOT_VOICE_COMMAND_SUPPLEMENT) && !state->playback_id &&
               (state->conversation == BOT_CONVERSATION_THINKING || state->conversation == BOT_CONVERSATION_SUBMITTED))) ||
            (event->data.local_command.command == BOT_VOICE_COMMAND_SUPPLEMENT &&
             state->conversation != BOT_CONVERSATION_THINKING && state->conversation != BOT_CONVERSATION_SUBMITTED) ||
            !bot_voice_command_valid(event->data.local_command.command) ||
            !isfinite(event->data.local_command.score) || event->data.local_command.score < 0 ||
            event->data.local_command.score > 1) {
            state->rejected_events++;
            break;
        }
        apply_local_command(runtime, event->data.local_command.command, now_ms);
        break;
    case BOT_EVENT_BARGE_IN:
        if (!runtime->config.allow_barge_in || !is_active(runtime) || state->settings_open ||
            state->critical_fault || state->local_response_active ||
            state->conversation != BOT_CONVERSATION_SPEAKING || !state->playback_id ||
            state->playback_id != event->data.playback.id || !token_matches(runtime,&event->token,false)) {
            state->rejected_events++;
            break;
        }
        /* Retire output/capture leases before returning to listening. The old
         * AFE onset is never relabelled; only subsequent fence-cleared audio
         * belongs to the new turn. Server task ownership stays server-side. */
        begin_turn_with_intent(runtime,now_ms,true);
        break;
    case BOT_EVENT_UTTERANCE_STARTED:
        if (!is_active(runtime) || state->critical_fault || state->settings_open ||
            state->local_response_active ||
            !state->audio_available || !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        if (state->utterance_active) break;
        if (state->conversation == BOT_CONVERSATION_THINKING ||
            state->conversation == BOT_CONVERSATION_SPEAKING) {
            if (!runtime->config.allow_barge_in || !event->data.utterance.echo_safe) {
                state->rejected_events++;
                break;
            }
            begin_turn(runtime, now_ms);
            /* This START belongs to the old capture lease. The audio adapter
             * clears its AFE/VAD history on the new token and emits a new START
             * if speech continues; never re-label an old operation as new. */
            break;
        } else if (state->conversation == BOT_CONVERSATION_WAITING) {
            if (!state->followup_armed) {
                state->rejected_events++;
                break;
            }
            /* This START already belongs to the next token; retain its onset. */
            state->followup_armed = false;
            state->caption[0] = '\0';
        } else if (state->conversation != BOT_CONVERSATION_LISTENING) {
            state->rejected_events++;
            break;
        }
        state->conversation = BOT_CONVERSATION_LISTENING;
        state->utterance_active = true;
        state->utterance_seen = true;
        /* Ambient sound without a usable media link is not conversation
         * progress and must not hold an offline session open indefinitely. */
        if (runtime->config.allow_audio_uplink && secure_link(runtime))
            state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_UTTERANCE_ENDED:
        if (!is_active(runtime) || !state->utterance_active ||
            !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        state->utterance_active = false;
        /* Local VAD only indicates sound ended. The authenticated server must
         * explicitly acknowledge processing before showing "thinking". */
        state->conversation = BOT_CONVERSATION_LISTENING;
        if (runtime->config.allow_audio_uplink && secure_link(runtime))
            state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_TURN_SUBMITTED:
        if (!is_active(runtime) || state->settings_open || state->critical_fault ||
            state->local_response_active || !secure_link(runtime) || !state->utterance_seen ||
            !token_matches(runtime, &event->token, false) ||
            state->conversation != BOT_CONVERSATION_LISTENING) {
            state->rejected_events++;
            break;
        }
        /* A START already queued during the final transport drain cannot
         * reopen this token. Only a newly authorized turn may capture again. */
        state->utterance_active = false;
        state->capture_ready = state->transport_ready = false;
        state->conversation = BOT_CONVERSATION_SUBMITTED;
        state->wait_started_ms = now_ms;
        state->wait_stage = BOT_WAIT_SENDING;
        state->caption[0] = 0;
        state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_PLAYBACK_PROGRESS:
        if (event->data.playback.preview && preview_token_matches(runtime, &event->token)) break;
        if (!is_active(runtime) || !token_matches(runtime, &event->token, false) ||
            state->conversation != BOT_CONVERSATION_SPEAKING || !event->data.playback.id ||
            event->data.playback.id != state->playback_id ||
            event->data.playback.local_response != state->local_response_active) {
            state->rejected_events++;
            break;
        }
        state->last_interaction_ms = now_ms;
        break;
    case BOT_EVENT_PLAYBACK_STARTED:
        if (event->data.playback.preview) {
            if (!preview_token_matches(runtime, &event->token) || !event->data.playback.id ||
                state->playback_id) state->rejected_events++;
            else state->playback_id = event->data.playback.id;
            break;
        }
        if (!is_active(runtime) || state->settings_open || state->critical_fault ||
            state->utterance_active || event->data.playback.id == 0 ||
            event->data.playback.local_response != state->local_response_active ||
            state->playback_id != 0 || !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        state->playback_id = event->data.playback.id;
        state->conversation = BOT_CONVERSATION_SPEAKING;
        state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_PLAYBACK_DONE:
    case BOT_EVENT_PLAYBACK_FAILED:
        if (event->data.playback.preview) {
            if (!preview_token_matches(runtime, &event->token) || !event->data.playback.id ||
                (state->playback_id && state->playback_id != event->data.playback.id))
                state->rejected_events++;
            else {
                state->sound_preview_active = false;
                state->playback_id = 0;
            }
            break;
        }
        if (event->data.playback.local_response) {
            if (!is_active(runtime) || !state->local_response_active ||
                !token_matches(runtime, &event->token, false) || !event->data.playback.id ||
                (state->playback_id && state->playback_id != event->data.playback.id)) {
                state->rejected_events++;
                break;
            }
            if (event->kind == BOT_EVENT_PLAYBACK_DONE && runtime->ops.wake_response_tail_clear) {
                runtime->last_response = runtime->response;
                runtime->response_playback_done = true;
                state->playback_id = 0;
            } else {
                if (event->kind == BOT_EVENT_PLAYBACK_DONE) runtime->last_response = runtime->response;
                finish_local_response(runtime, now_ms);
            }
            break;
        }
        if (!is_active(runtime) || !token_matches(runtime, &event->token, false) ||
            state->local_response_active ||
            state->conversation != BOT_CONVERSATION_SPEAKING ||
            event->data.playback.id == 0 || event->data.playback.id != state->playback_id) {
            state->rejected_events++;
            break;
        }
        state->playback_id = 0;
        state->last_interaction_ms = now_ms;
        if (event->data.playback.presentation_managed || event->data.playback.purpose != BOT_PLAYBACK_FINAL) {
            /* ACK/progress/content is not a terminal answer. The single-token
             * presentation owner waits for actual audio AND display receipts. */
            state->conversation = BOT_CONVERSATION_THINKING;
            reset_expression(runtime);
            if (runtime->presentation_done_pending) {
                runtime->presentation_done_pending = false;
                if (event->kind == BOT_EVENT_PLAYBACK_DONE)
                    arm_followup(runtime, now_ms);
            }
            break;
        }
        runtime->remote_audio_done = true;
        /* A very short clip may finish before the queued text receipt is
         * drained. Do not retire that token until its PRESENT has been applied. */
        if (!state->voice_settings.spoken_answers || runtime->reply_presented)
            arm_followup(runtime, now_ms);
        break;
    case BOT_EVENT_LOCAL_RESPONSE_FAILED:
        if (preview_token_matches(runtime, &event->token)) {
            stop_preview(runtime);
            break;
        }
        if (!is_active(runtime) || !state->local_response_active ||
            !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        if (runtime->ops.stop_output) runtime->ops.stop_output(runtime->context);
        finish_local_response(runtime, now_ms);
        break;
    case BOT_EVENT_LINK_STATE: {
        bool was_secure = secure_link(runtime);
        runtime->has_link_state = true; /* connection state outlives event-order bookkeeping */
        state->link_connected = event->data.link.connected;
        state->link_authenticated = state->link_connected && event->data.link.authenticated;
        state->link_encrypted = state->link_connected && event->data.link.encrypted;
        if (!secure_link(runtime)) state->transport_ready = false;
        if (was_secure && !secure_link(runtime) && is_active(runtime) && !state->local_response_active)
            begin_turn(runtime, now_ms);
        break;
    }
    case BOT_EVENT_AUDIO_HEALTH: {
        bool was_available = state->audio_available;
        state->audio_available = event->data.health.available;
        if (!state->audio_available) state->capture_ready = false;
        if (!state->audio_available) state->wake_status = BOT_WAKE_UNAVAILABLE;
        if (was_available && !state->audio_available && is_active(runtime)) begin_turn(runtime, now_ms);
        break;
    }
    case BOT_EVENT_CAMERA_HEALTH:
        state->camera_available = event->data.health.available;
        if (!state->camera_available) runtime->has_vision = false;
        break;
    case BOT_EVENT_DISPLAY_HEALTH:
        state->display_awake = event->data.display.awake;
        state->display_sleep_supported = event->data.display.sleep_supported;
        state->display_full_blackout_verified = event->data.display.full_blackout_verified;
        break;
    case BOT_EVENT_WAKE_STATUS:
        if (event->data.wake_status.status < BOT_WAKE_MODEL_MISSING ||
            event->data.wake_status.status > BOT_WAKE_UNAVAILABLE) {
            state->rejected_events++;
            break;
        }
        if (event->data.wake_status.status == BOT_WAKE_READY && !state->audio_available) {
            state->rejected_events++;
            break;
        }
        state->wake_status = event->data.wake_status.status;
        break;
    case BOT_EVENT_NIGHT_POLICY:
        state->clock_valid = event->data.night.clock_valid;
        state->night_policy = state->clock_valid && event->data.night.night;
        state->local_hour = event->data.night.local_hour;
        if (!is_active(runtime)) set_passive(runtime, now_ms);
        break;
    case BOT_EVENT_SESSION_END:
        if (!is_active(runtime) || !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        end_session(runtime, now_ms, SESSION_DISMISSED);
        switch (event->data.session_end.code) {
        case BOT_SESSION_END_PARENT_STOP: strcpy(state->caption, "Mac 已停止本次互动"); break;
        case BOT_SESSION_END_MEMORY_CHANGED: strcpy(state->caption, "记忆已更新，请再问一次"); break;
        case BOT_SESSION_END_SERVICE_STOPPED: strcpy(state->caption, "Mac 服务已停止"); break;
        case BOT_SESSION_END_EXPIRED: strcpy(state->caption, "本次互动已结束"); break;
        default: break;
        }
        break;
    case BOT_EVENT_FAULT:
        end_session(runtime, now_ms, SESSION_FAULT);
        break;
    default:
        state->rejected_events++;
        break;
    }
    derive_permissions(runtime);
}

static bool valid_text(const char *text, size_t capacity)
{
    const unsigned char *bytes = (const unsigned char *)text;
    const char *terminator = memchr(text, '\0', capacity);
    if (!terminator) return false;
    size_t length = (size_t)(terminator - text);
    for (size_t i = 0; i < length;) {
        unsigned char lead = bytes[i++];
        if (lead < 0x80) {
            if (lead < 0x20 && lead != '\n' && lead != '\t') return false;
            continue;
        }
        unsigned count;
        uint32_t scalar;
        if (lead >= 0xc2 && lead <= 0xdf) { count = 1; scalar = lead & 0x1f; }
        else if (lead >= 0xe0 && lead <= 0xef) { count = 2; scalar = lead & 0x0f; }
        else if (lead >= 0xf0 && lead <= 0xf4) { count = 3; scalar = lead & 0x07; }
        else return false;
        if (i + count > length) return false;
        for (unsigned n = 0; n < count; n++) {
            if ((bytes[i] & 0xc0) != 0x80) return false;
            scalar = (scalar << 6) | (bytes[i++] & 0x3f);
        }
        if ((count == 1 && scalar < 0x80) || (count == 2 && scalar < 0x800) ||
            (count == 3 && scalar < 0x10000) || scalar > 0x10ffff ||
            (scalar >= 0xd800 && scalar <= 0xdfff)) return false;
    }
    return true;
}

static bool known_expression(const char *name)
{
    static const char *const names[] = { "idle", "listening", "happy", "curious", "thinking",
        "surprised", "sleepy", "shy", "blink", "bored", "drowsy", "sleeping", "speaking",
        "gentle", "confused", "discovery", "laughing", "excited", "wink", "smug", "skeptical",
        "determined", "annoyed", "worried", "sad", "affection", "relieved", "overjoyed" };
    if (!memchr(name, '\0', BOT_EXPRESSION_MAX_BYTES + 1)) return false;
    for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++)
        if (strcmp(names[i], name) == 0) return true;
    return false;
}

static bot_command_result_t handle_command(bot_runtime_t *runtime, const bot_command_t *command,
                                           uint32_t now_ms)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    if (command->kind < BOT_COMMAND_SET_EXPRESSION || command->kind > BOT_COMMAND_PRESENTATION_DONE)
        return BOT_COMMAND_REJECTED_INVALID;
    if (state->critical_fault) return BOT_COMMAND_REJECTED_FAULT;
    if (state->settings_open) return BOT_COMMAND_REJECTED_SETTINGS;
    if (!secure_link(runtime)) return BOT_COMMAND_REJECTED_UNTRUSTED;
    if (!is_active(runtime)) return BOT_COMMAND_REJECTED_PASSIVE;
    const bool reading_image = command->kind == BOT_COMMAND_IMAGE_FAILED &&
        state->reading_token.session_id &&
        command->token.epoch == state->reading_token.epoch &&
        command->token.session_id == state->reading_token.session_id &&
        command->token.turn_id == state->reading_token.turn_id &&
        command->token.focus_generation == state->reading_token.focus_generation &&
        (state->followup_armed || token_matches(runtime, &command->token, true));
    if (!reading_image && !token_matches(runtime, &command->token, true))
        return BOT_COMMAND_REJECTED_STALE;
    /* Local acknowledgements own their expression/caption and playback. The
     * remote session opens only after their hardware completion. */
    if (state->local_response_active) return BOT_COMMAND_REJECTED_INVALID;
    uint32_t ttl = command->ttl_ms ? command->ttl_ms : runtime->config.command_max_age_ms;
    if (ttl > runtime->config.command_max_age_ms) ttl = runtime->config.command_max_age_ms;
    if (!within_age(now_ms, command->received_ms, ttl)) return BOT_COMMAND_REJECTED_EXPIRED;
    switch (command->kind) {
    case BOT_COMMAND_SET_EXPRESSION:
        if (!known_expression(command->data.expression)) return BOT_COMMAND_REJECTED_INVALID;
        memset(state->expression, 0, sizeof(state->expression));
        memcpy(state->expression, command->data.expression, strlen(command->data.expression));
        if (state->conversation == BOT_CONVERSATION_THINKING ||
            state->conversation == BOT_CONVERSATION_SPEAKING ||
            state->conversation == BOT_CONVERSATION_SUBMITTED)
            memcpy(runtime->segment_expression, state->expression, sizeof(runtime->segment_expression));
        break;
    case BOT_COMMAND_SET_CAPTION:
    case BOT_COMMAND_FAILURE_CAPTION:
        if (!valid_text(command->data.caption, sizeof(command->data.caption)))
            return BOT_COMMAND_REJECTED_INVALID;
        memset(state->caption, 0, sizeof(state->caption));
        memcpy(state->caption, command->data.caption, strlen(command->data.caption));
        if (command->kind == BOT_COMMAND_FAILURE_CAPTION && state->reading_token.session_id &&
            !token_matches(runtime, &state->reading_token, false))
            memcpy(state->reading_notice, state->caption, sizeof(state->reading_notice));
        break;
    case BOT_COMMAND_IMAGE_FAILED:
        if (!reading_image) return BOT_COMMAND_REJECTED_STALE;
        if (!valid_text(command->data.caption, sizeof(command->data.caption)))
            return BOT_COMMAND_REJECTED_INVALID;
        memcpy(state->image_notice, command->data.caption, sizeof(state->image_notice));
        break;
    case BOT_COMMAND_THINKING:
        if (state->utterance_active ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_SUBMITTED &&
             state->conversation != BOT_CONVERSATION_THINKING))
            return BOT_COMMAND_REJECTED_INVALID;
        if (state->wait_stage == BOT_WAIT_NONE) state->wait_started_ms = now_ms;
        if (state->wait_stage == BOT_WAIT_NONE || state->wait_stage == BOT_WAIT_SENDING)
            state->wait_stage = BOT_WAIT_WORKING;
        state->conversation = BOT_CONVERSATION_THINKING;
        state->last_interaction_ms = now_ms;
        runtime->waiting_for_approval = false;
        reset_expression(runtime);
        break;
    case BOT_COMMAND_PRESENTATION_SEGMENT:
        if (state->utterance_active || state->followup_armed ||
            (state->conversation != BOT_CONVERSATION_SUBMITTED &&
             state->conversation != BOT_CONVERSATION_THINKING &&
             state->conversation != BOT_CONVERSATION_SPEAKING)) return BOT_COMMAND_REJECTED_INVALID;
        if (!valid_text(command->data.caption, sizeof(command->data.caption)))
            return BOT_COMMAND_REJECTED_INVALID;
        memcpy(state->reading_text, command->data.caption, sizeof(state->reading_text));
        state->reading_token = state->token;
        state->wait_stage = BOT_WAIT_NONE;
        runtime->segment_expression[0] = '\0'; /* this segment's own hint follows it */
        state->reading_sequence = command->presentation_sequence;
        state->reading_notice[0] = state->image_notice[0] = '\0';
        if (state->conversation != BOT_CONVERSATION_SPEAKING)
            state->conversation = BOT_CONVERSATION_THINKING;
        state->last_interaction_ms = now_ms;
        runtime->reply_presented = runtime->remote_audio_done = false;
        runtime->presentation_done_pending = false;
        break;
    case BOT_COMMAND_PRESENTATION_DONE:
        if (state->utterance_active || state->followup_armed)
            return BOT_COMMAND_REJECTED_INVALID;
        /* The transport and runtime consume separate queues. Its genuine
         * final receipt can arrive while our hardware DONE is still queued.
         * Retain that intent instead of losing it or opening capture early. */
        if (state->playback_id) runtime->presentation_done_pending = true;
        else arm_followup(runtime, now_ms);
        break;
    case BOT_COMMAND_REPLY_PRESENT:
    case BOT_COMMAND_REPLY_DONE:
        if (state->utterance_active ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_SUBMITTED &&
             state->conversation != BOT_CONVERSATION_THINKING &&
             !(state->conversation == BOT_CONVERSATION_SPEAKING &&
               (command->kind == BOT_COMMAND_REPLY_PRESENT || runtime->remote_audio_done))))
            return BOT_COMMAND_REJECTED_INVALID;
        if (command->data.caption[0]) {
            if (!valid_text(command->data.caption, sizeof(command->data.caption)))
                return BOT_COMMAND_REJECTED_INVALID;
            memcpy(state->reading_text, command->data.caption, sizeof(state->reading_text));
            state->reading_token = state->token;
            state->reading_sequence = 0;
            state->reading_notice[0] = '\0';
            state->image_notice[0] = '\0';
        } else if (state->reading_token.session_id &&
                   !token_matches(runtime, &state->reading_token, false)) {
            /* A failed follow-up while reading must not rearm the microphone
             * on room noise. Keep its error above the old answer after the
             * microphone session closes; a deliberate touch starts a new task. */
            if (state->caption[0])
                memcpy(state->reading_notice, state->caption, sizeof(state->reading_notice));
            end_session(runtime, now_ms, SESSION_INACTIVE);
            break;
        }
        if (command->kind == BOT_COMMAND_REPLY_PRESENT) {
            runtime->reply_presented = true;
            state->last_interaction_ms = now_ms;
            if (runtime->remote_audio_done) arm_followup(runtime, now_ms);
            break;
        }
        /* Audio completion is a local event. This receipt closes a text-only
         * reply; a remote server cannot declare hardware playback completed. */
        arm_followup(runtime, now_ms);
        break;
    case BOT_COMMAND_PROGRESS:
        if (state->utterance_active || state->conversation != BOT_CONVERSATION_THINKING)
            return BOT_COMMAND_REJECTED_INVALID;
        if (command->wait_stage <= BOT_WAIT_NONE || command->wait_stage > BOT_WAIT_ANSWER ||
            (command->data.caption[0] && !valid_text(command->data.caption, sizeof(command->data.caption)))) return BOT_COMMAND_REJECTED_INVALID;
        /* Progress never advances a presentation or replaces an already displayed answer. */
        if (state->wait_stage != BOT_WAIT_NONE) {
            state->wait_stage = command->wait_stage;
            if (command->data.caption[0]) memcpy(state->caption,command->data.caption,sizeof(state->caption));
        }
        runtime->waiting_for_approval = command->wait_stage == BOT_WAIT_APPROVAL;
        state->last_interaction_ms = now_ms;
        break;
    case BOT_COMMAND_END_SESSION:
        end_session(runtime, now_ms, SESSION_DISMISSED);
        break;
    default: return BOT_COMMAND_REJECTED_INVALID;
    }
    return BOT_COMMAND_ACCEPTED;
}

bot_runtime_config_t bot_runtime_default_config(void)
{
    return (bot_runtime_config_t){
        .daytime_awake_ms = CONFIG_BOT_RUNTIME_DAY_AWAKE_MS,
        .daytime_drowsy_ms = CONFIG_BOT_RUNTIME_DAY_DROWSY_MS,
        .waking_ms = CONFIG_BOT_RUNTIME_WAKING_MS,
        .session_timeout_ms = CONFIG_BOT_RUNTIME_PROGRESS_TIMEOUT_MS,
        .wake_debounce_ms = CONFIG_BOT_RUNTIME_WAKE_DEBOUNCE_MS,
        .event_max_age_ms = 2000, .command_max_age_ms = 5000,
        .max_events_per_step = 32, .max_commands_per_step = 8,
        .allow_audio_uplink = false, .allow_barge_in = false,
        .voice_settings = bot_voice_default_settings(),
        .focus = attention_manager_default_config()
    };
}

static bool valid_config(const bot_runtime_config_t *config)
{
    return config->daytime_awake_ms > 0 && config->daytime_awake_ms < INT32_MAX &&
        config->daytime_drowsy_ms > 0 && config->daytime_drowsy_ms < INT32_MAX &&
        config->waking_ms < INT32_MAX &&
        config->session_timeout_ms > config->waking_ms && config->session_timeout_ms < INT32_MAX &&
        config->wake_debounce_ms < INT32_MAX && config->event_max_age_ms > 0 &&
        config->event_max_age_ms < INT32_MAX && config->command_max_age_ms > 0 &&
        config->command_max_age_ms < INT32_MAX &&
        bot_voice_settings_valid(&config->voice_settings) &&
        config->max_events_per_step > 0 && config->max_events_per_step <= 512 &&
        config->max_commands_per_step > 0 && config->max_commands_per_step <= 64;
}

esp_err_t bot_runtime_create(bot_bus_t *bus, const bot_runtime_config_t *config,
                             const bot_runtime_ops_t *ops, void *context,
                             uint32_t now_ms, bot_runtime_t **out)
{
    if (!bus || !out) return ESP_ERR_INVALID_ARG;
    *out = NULL;
    bot_runtime_config_t cfg = config ? *config : bot_runtime_default_config();
    if (!valid_config(&cfg)) return ESP_ERR_INVALID_ARG;
    bot_runtime_t *runtime = calloc(1, sizeof(*runtime));
    if (!runtime) return ESP_ERR_NO_MEM;
    runtime->bus = bus;
    runtime->config = cfg;
    if (ops) runtime->ops = *ops;
    runtime->context = context;
    if (!attention_manager_init(&runtime->attention, &cfg.focus)) {
        free(runtime);
        return ESP_ERR_INVALID_ARG;
    }
    runtime->state.token.epoch = 1;
    runtime->state.voice_settings = cfg.voice_settings;
    runtime->state.last_interaction_ms = now_ms;
    bot_passive_cycle_reset(&runtime->passive_cycle, now_ms);
    runtime->state.wake_status = BOT_WAKE_MODEL_MISSING;
    bot_idle_expression_init(&runtime->idle_expression, now_ms ^ 0xa3c59ac3U);
    bot_responses_init(&runtime->responses, now_ms ^ 0x71b95d83U);
    runtime->state.behavior = BOT_BEHAVIOR_IDLE;
    reset_expression(runtime);
    derive_permissions(runtime);
    *out = runtime;
    /* First publication occurs on step(), so callers can finish adapter setup. */
    return ESP_OK;
}

void bot_runtime_destroy(bot_runtime_t *runtime)
{
    if (!runtime) return;
    /* Stop producers and the owner loop before destruction. */
    end_session(runtime, runtime->state.last_interaction_ms, SESSION_DISMISSED);
    publish_state(runtime);
    free(runtime);
}

static void update_ambient(bot_runtime_t *runtime, uint32_t now_ms)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    const bool allow_probe = !runtime->manual_sleep && !is_active(runtime) && !state->settings_open && !state->critical_fault &&
        !(state->clock_valid && state->night_policy);
    bot_ambient_step(&runtime->ambient, now_ms, allow_probe);
    state->ambient_dark = runtime->ambient.dark;
    state->ambient_sample_valid = runtime->ambient.valid;
    state->ambient_luma = runtime->ambient.luma;
    state->ambient_probe = runtime->ambient.probing;
}

void bot_runtime_observe_light(bot_runtime_t *runtime, uint8_t mean_luma,
                               uint32_t captured_ms, uint32_t now_ms)
{
    if (!runtime || !within_age(now_ms, captured_ms, 2000)) return;
    bot_ambient_observe(&runtime->ambient, mean_luma, captured_ms);
}

void bot_runtime_step(bot_runtime_t *runtime, uint32_t now_ms)
{
    if (!runtime) return;
    now_ms = read_now_ms(runtime, now_ms);
    update_ambient(runtime, now_ms);
    /* Debounce/barrier clocks are short leases, not a permanent comparison to
     * a boot-old timestamp. Expire them while stepping across timer wraps. */
    if (runtime->has_wake && !within_age(now_ms, runtime->last_wake_ms,
                                         runtime->config.wake_debounce_ms)) runtime->has_wake = false;
    if (runtime->wake_barrier_valid && !within_age(now_ms, runtime->wake_barrier_ms,
                                                  runtime->config.event_max_age_ms))
        runtime->wake_barrier_valid = false;
    for (unsigned kind = 0; kind < BOT_EVENT_COUNT; kind++) {
        const uint32_t max_age = state_event((bot_event_kind_t)kind) ? INT32_MAX : runtime->config.event_max_age_ms;
        if (runtime->event_seen[kind] && !within_age(now_ms, runtime->event_times[kind],
                                                    max_age))
            runtime->event_seen[kind] = false;
    }
    bool stopped_this_step = consume_signals(runtime, &now_ms);
    bot_event_t event;
    for (uint16_t i = 0; i < runtime->config.max_events_per_step &&
                         bot_bus_receive(runtime->bus, &event); i++) {
        stopped_this_step |= consume_signals(runtime, &now_ms);
        handle_event(runtime, &event, now_ms, stopped_this_step);
        /* Apply newly revoked permissions before considering the next message. */
        publish_state(runtime);
    }
    stopped_this_step |= consume_signals(runtime, &now_ms);
    derive_permissions(runtime);
    if (!runtime->state.camera_requested) {
        /* Retire stopped-camera timestamps as well as pixel-free snapshots; an
         * old frame must not block publishing after a long sleep/timer wrap. */
        bot_bus_clear_vision(runtime->bus);
        runtime->has_vision = false;
    } else if (bot_bus_read_vision(runtime->bus, &runtime->vision)) {
        runtime->has_vision = true;
    }
    /* latest vision can also be published during this step. Its capture clock
     * must be judged against a reading taken after the snapshot was copied. */
    now_ms = read_now_ms(runtime, now_ms);
    derive_permissions(runtime);
    /* Frames predating the explicit wake cannot become this session's focus. */
    const vatt_snapshot_t *vision = is_active(runtime) && runtime->has_vision &&
        (int32_t)(runtime->vision.timestamp_ms - runtime->waking_since_ms) >= 0 ?
        &runtime->vision : NULL;
    attention_manager_step(&runtime->attention, vision, now_ms, &runtime->state.focus);

    bot_command_t command;
    for (uint16_t i = 0; i < runtime->config.max_commands_per_step &&
                         bot_bus_receive_command(runtime->bus, &command); i++) {
        stopped_this_step |= consume_signals(runtime, &now_ms);
        bot_command_result_t result = handle_command(runtime, &command, now_ms);
        if (result != BOT_COMMAND_ACCEPTED) runtime->state.rejected_commands++;
        if (runtime->ops.command_result)
            runtime->ops.command_result(runtime->context, command.request_id, result);
    }
    consume_signals(runtime, &now_ms);
    if (runtime->has_acoustic_wake && !within_age(now_ms, runtime->last_acoustic_wake_ms, 60000))
        runtime->has_acoustic_wake = false;
    if (runtime->state.local_response_active && runtime->response_playback_done &&
        runtime->ops.wake_response_tail_clear &&
        runtime->ops.wake_response_tail_clear(runtime->context))
        finish_local_response(runtime, now_ms);
    if (runtime->state.local_response_active &&
        !within_age(now_ms, runtime->response_started_ms, runtime->response_timeout_ms)) {
        if (runtime->ops.stop_output) runtime->ops.stop_output(runtime->context);
        finish_local_response(runtime, now_ms);
    }
    if (runtime->state.sound_preview_active &&
        !within_age(now_ms, runtime->response_started_ms, runtime->response_timeout_ms))
        stop_preview(runtime);
    if (is_active(runtime)) {
        uint32_t timeout_ms = runtime->config.session_timeout_ms;
        if (runtime->waiting_for_approval && timeout_ms < 180000) timeout_ms = 180000;
        if (!within_age(now_ms, runtime->state.last_interaction_ms, timeout_ms)) {
            end_session(runtime, now_ms, SESSION_INACTIVE);
        } else if (runtime->state.behavior == BOT_BEHAVIOR_WAKING &&
                   (int32_t)(now_ms - runtime->waking_since_ms) >= 0 &&
                   (uint32_t)(now_ms - runtime->waking_since_ms) >= runtime->config.waking_ms) {
            runtime->state.behavior = BOT_BEHAVIOR_ENGAGED;
            reset_expression(runtime);
        }
    } else {
        set_passive(runtime, now_ms);
    }
    update_ambient(runtime, now_ms);
    derive_permissions(runtime);
    runtime->state.near_face = bot_near_face_step(&runtime->near_face,
        runtime->has_vision && runtime->state.camera_available ? &runtime->vision : NULL, now_ms,
        runtime->config.focus.observation_max_age_ms,
        runtime->config.focus.min_face_confidence,
        runtime->state.camera_requested && !runtime->state.settings_open && !runtime->state.critical_fault,
        &runtime->state.near_face_area_permille);
    const bool had_person = runtime->state.person_visible;
    runtime->state.person_visible = bot_face_presence_step(&runtime->face_presence,
        runtime->has_vision && runtime->state.camera_available ? &runtime->vision : NULL, now_ms,
        runtime->config.focus.observation_max_age_ms, runtime->config.focus.min_face_confidence,
        runtime->state.camera_requested && runtime->state.camera_available &&
            !runtime->state.settings_open && !runtime->state.critical_fault);
    if (!runtime->state.person_visible || runtime->state.behavior != BOT_BEHAVIOR_IDLE)
        runtime->face_reaction_active = false;
    else if (!had_person) {
        runtime->face_reaction_started_ms = now_ms;
        runtime->face_reaction_active = true;
    } else if (runtime->face_reaction_active &&
               !within_age(now_ms, runtime->face_reaction_started_ms, 1800))
        runtime->face_reaction_active = false;
    if (is_active(runtime) || runtime->state.settings_open || runtime->state.critical_fault ||
        runtime->state.behavior == BOT_BEHAVIOR_NIGHT_DARK ||
        runtime->state.behavior == BOT_BEHAVIOR_SLEEPING ||
        !within_age(now_ms, runtime->last_sound_ms, 1800)) {
        runtime->state.sound_reaction = false;
        runtime->state.sound_direction_valid = false;
    }
    const bool expressive_idle = runtime->state.behavior == BOT_BEHAVIOR_IDLE &&
        !runtime->state.near_face && !runtime->state.settings_open && !runtime->state.critical_fault;
    const char *idle_expression = bot_idle_expression_step(&runtime->idle_expression, now_ms, expressive_idle);
    if (idle_expression) {
        memcpy(runtime->state.expression, idle_expression, strlen(idle_expression) + 1);
    } else if (runtime->state.behavior == BOT_BEHAVIOR_IDLE) {
        /* Leave no stale idle pose behind when a nearby face is present. */
        reset_expression(runtime);
    } else if (!is_active(runtime)) reset_expression(runtime);
    /* A passive nearby person gets a smile. Once explicitly awakened, the
     * actual listening/thinking/speaking state must remain visible. */
    if (runtime->state.near_face && !is_active(runtime) &&
        runtime->state.behavior != BOT_BEHAVIOR_SLEEPING &&
        runtime->state.behavior != BOT_BEHAVIOR_NIGHT_DARK &&
        !runtime->state.settings_open && !runtime->state.critical_fault)
        memcpy(runtime->state.expression, "happy", sizeof("happy"));
    else if (runtime->face_reaction_active && !runtime->state.near_face &&
        within_age(now_ms, runtime->face_reaction_started_ms, 1800))
        memcpy(runtime->state.expression, "happy", sizeof("happy"));
    if (runtime->recovery_requested && runtime->overflow_fault &&
        !bot_bus_events_pending(runtime->bus)) bot_runtime_clear_fault(runtime, now_ms);
    publish_state(runtime);
}

void bot_runtime_get_snapshot(const bot_runtime_t *runtime, bot_runtime_snapshot_t *out)
{
    if (runtime && out) *out = runtime->state;
}

void bot_runtime_clear_fault(bot_runtime_t *runtime, uint32_t now_ms)
{
    if (!runtime || !runtime->state.critical_fault || runtime->identity_exhausted) return;
    /* Caller first resolves the queue/producer fault. Commands remain queued so
     * they can receive an explicit stale/passive receipt; they regain no lease. */
    if (consume_signals(runtime, &now_ms)) return;
    runtime->state.critical_fault = false;
    runtime->overflow_fault = runtime->recovery_requested = false;
    end_session(runtime, now_ms, SESSION_DISMISSED);
    derive_permissions(runtime);
    publish_state(runtime);
}

const char *bot_behavior_name(bot_behavior_t behavior)
{
    switch (behavior) {
    case BOT_BEHAVIOR_IDLE: return "idle";
    case BOT_BEHAVIOR_BORED: return "bored";
    case BOT_BEHAVIOR_DROWSY: return "drowsy";
    case BOT_BEHAVIOR_SLEEPING: return "sleeping";
    case BOT_BEHAVIOR_NIGHT_DARK: return "night_dark";
    case BOT_BEHAVIOR_WAKING: return "waking";
    case BOT_BEHAVIOR_ENGAGED: return "engaged";
    default: return "unknown";
    }
}

const char *bot_conversation_name(bot_conversation_t conversation)
{
    switch (conversation) {
    case BOT_CONVERSATION_NONE: return "none";
    case BOT_CONVERSATION_LISTENING: return "listening";
    case BOT_CONVERSATION_SUBMITTED: return "submitted";
    case BOT_CONVERSATION_THINKING: return "thinking";
    case BOT_CONVERSATION_SPEAKING: return "speaking";
    case BOT_CONVERSATION_WAITING: return "waiting";
    case BOT_CONVERSATION_ENDING: return "ending";
    default: return "unknown";
    }
}

const char *bot_command_result_name(bot_command_result_t result)
{
    switch (result) {
    case BOT_COMMAND_ACCEPTED: return "accepted";
    case BOT_COMMAND_REJECTED_STALE: return "rejected_stale";
    case BOT_COMMAND_REJECTED_EXPIRED: return "rejected_expired";
    case BOT_COMMAND_REJECTED_UNTRUSTED: return "rejected_untrusted";
    case BOT_COMMAND_REJECTED_PASSIVE: return "rejected_passive";
    case BOT_COMMAND_REJECTED_SETTINGS: return "rejected_settings";
    case BOT_COMMAND_REJECTED_INVALID: return "rejected_invalid";
    case BOT_COMMAND_REJECTED_FAULT: return "rejected_fault";
    default: return "unknown";
    }
}

#include "bot_runtime.h"
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
    bool revoking, identity_exhausted, passive_sleep_latched;
    uint32_t waking_since_ms, last_wake_ms, wake_barrier_ms, next_session_id;
    uint32_t event_times[BOT_EVENT_COUNT];
    bool event_seen[BOT_EVENT_COUNT];
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
    const char *expression = "idle";
    switch (runtime->state.behavior) {
    case BOT_BEHAVIOR_BORED: expression = "bored"; break;
    case BOT_BEHAVIOR_DROWSY: expression = "drowsy"; break;
    case BOT_BEHAVIOR_SLEEPING:
    case BOT_BEHAVIOR_NIGHT_DARK: expression = "sleeping"; break;
    case BOT_BEHAVIOR_WAKING: expression = "surprised"; break;
    case BOT_BEHAVIOR_ENGAGED:
        switch (runtime->state.conversation) {
        case BOT_CONVERSATION_LISTENING: expression = "listening"; break;
        case BOT_CONVERSATION_THINKING: expression = "thinking"; break;
        case BOT_CONVERSATION_SPEAKING: expression = "speaking"; break;
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
    bool engaged = is_active(runtime) && !state->settings_open && !state->critical_fault &&
                   state->token.session_id != 0;
    state->display_awake_requested = state->settings_open ||
        (state->behavior != BOT_BEHAVIOR_NIGHT_DARK &&
         (state->behavior != BOT_BEHAVIOR_SLEEPING || !runtime->config.sleep_display));
    state->camera_requested = engaged;
    state->audio_session_requested = engaged && !runtime->revoking;
    state->audio_uplink_permitted = state->audio_session_requested && runtime->config.allow_audio_uplink &&
                                   state->audio_available && secure_link(runtime);
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
    bot_behavior_t next = BOT_BEHAVIOR_IDLE;
    if (state->clock_valid && state->night_policy) {
        next = BOT_BEHAVIOR_NIGHT_DARK;
    } else {
        int32_t signed_elapsed = (int32_t)(now_ms - state->last_interaction_ms);
        uint32_t elapsed = signed_elapsed >= 0 ? (uint32_t)signed_elapsed : 0;
        if (elapsed >= runtime->config.idle_to_sleep_ms) runtime->passive_sleep_latched = true;
        if (runtime->passive_sleep_latched) next = BOT_BEHAVIOR_SLEEPING;
        else if (elapsed >= runtime->config.idle_to_drowsy_ms) next = BOT_BEHAVIOR_DROWSY;
        else if (elapsed >= runtime->config.idle_to_bored_ms) next = BOT_BEHAVIOR_BORED;
    }
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

static void end_session(bot_runtime_t *runtime, uint32_t now_ms, bool fault)
{
    bot_session_token_t old = runtime->state.token;
    runtime->state.critical_fault |= fault;
    advance_epoch(runtime);
    runtime->state.token.session_id = 0;
    runtime->state.token.turn_id = 0;
    runtime->state.token.focus_generation = 0;
    runtime->state.utterance_active = false;
    runtime->state.playback_id = 0;
    runtime->state.last_interaction_ms = now_ms;
    runtime->passive_sleep_latched = false;
    runtime->state.caption[0] = '\0';
    runtime->wake_barrier_valid = true;
    runtime->wake_barrier_ms = now_ms;
    runtime->state.conversation = BOT_CONVERSATION_ENDING;
    invalidate_output(runtime, &old);
    set_passive(runtime, now_ms);
    derive_permissions(runtime);
}

static bool begin_turn(bot_runtime_t *runtime, uint32_t now_ms)
{
    bot_session_token_t old = runtime->state.token;
    if (runtime->state.token.turn_id == UINT32_MAX || !advance_epoch(runtime)) {
        runtime->identity_exhausted = true;
        end_session(runtime, now_ms, true);
        return false;
    }
    runtime->state.token.turn_id++;
    runtime->state.token.focus_generation = 0;
    runtime->state.utterance_active = false;
    runtime->state.playback_id = 0;
    runtime->state.caption[0] = '\0';
    invalidate_output(runtime, &old);
    runtime->state.conversation = BOT_CONVERSATION_LISTENING;
    runtime->state.last_interaction_ms = now_ms;
    reset_expression(runtime);
    derive_permissions(runtime);
    return true;
}

static bool begin_session(bot_runtime_t *runtime, uint32_t now_ms)
{
    if (runtime->next_session_id == UINT32_MAX || !advance_epoch(runtime)) {
        runtime->identity_exhausted = true;
        end_session(runtime, now_ms, true);
        return false;
    }
    runtime->next_session_id++;
    runtime->state.token.session_id = runtime->next_session_id;
    runtime->state.token.turn_id = 1;
    runtime->state.token.focus_generation = 0;
    runtime->state.behavior = BOT_BEHAVIOR_WAKING;
    runtime->state.conversation = BOT_CONVERSATION_LISTENING;
    runtime->state.utterance_active = false;
    runtime->state.playback_id = 0;
    runtime->state.last_interaction_ms = now_ms;
    runtime->passive_sleep_latched = false;
    runtime->state.caption[0] = '\0';
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
    end_session(runtime, *now_ms, signals.critical_overflow);
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
    case BOT_EVENT_UTTERANCE_STARTED:
    case BOT_EVENT_UTTERANCE_ENDED:
    case BOT_EVENT_PLAYBACK_STARTED:
    case BOT_EVENT_PLAYBACK_DONE:
    case BOT_EVENT_PLAYBACK_FAILED:
    case BOT_EVENT_WAKE_STATUS:
        return event->source == BOT_SOURCE_AUDIO;
    case BOT_EVENT_AUDIO_HEALTH:
        return event->source == BOT_SOURCE_AUDIO || event->source == BOT_SOURCE_PLATFORM;
    case BOT_EVENT_TOUCH_WAKE:
    case BOT_EVENT_LOCAL_STOP:
    case BOT_EVENT_SETTINGS_OPEN:
    case BOT_EVENT_SETTINGS_CLOSE:
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

static void mark_wake(bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms)
{
    runtime->last_wake_ms = event->observed_ms;
    runtime->has_wake = true;
    if (is_active(runtime)) {
        begin_turn(runtime, now_ms);
    } else {
        begin_session(runtime, now_ms);
    }
}

static bool event_order_valid(bot_runtime_t *runtime, const bot_event_t *event, uint32_t now_ms)
{
    if (!within_age(now_ms, event->observed_ms, runtime->config.event_max_age_ms)) return false;
    if (runtime->event_seen[event->kind] &&
        !within_age(now_ms, runtime->event_times[event->kind], runtime->config.event_max_age_ms))
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
        end_session(runtime, now_ms, true);
        return;
    }
    if (event->kind < 0 || event->kind >= BOT_EVENT_COUNT || !source_allowed(event) ||
        !event_order_valid(runtime, event, now_ms)) {
        state->rejected_events++;
        return;
    }
    switch (event->kind) {
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
        if (stopped_this_step || !event->data.touch.intentional || !may_wake(runtime, event, now_ms)) {
            state->rejected_events++;
            break;
        }
        mark_wake(runtime, event, now_ms);
        break;
    case BOT_EVENT_LOCAL_STOP:
        end_session(runtime, now_ms, false);
        break;
    case BOT_EVENT_SETTINGS_OPEN:
        state->settings_open = true;
        end_session(runtime, now_ms, false);
        break;
    case BOT_EVENT_SETTINGS_CLOSE:
        state->settings_open = false;
        state->last_interaction_ms = now_ms;
        runtime->passive_sleep_latched = false;
        runtime->wake_barrier_valid = true;
        runtime->wake_barrier_ms = now_ms;
        set_passive(runtime, now_ms);
        break;
    case BOT_EVENT_UTTERANCE_STARTED:
        if (!is_active(runtime) || state->critical_fault || state->settings_open ||
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
            begin_turn(runtime, now_ms);
            break;
        } else if (state->conversation != BOT_CONVERSATION_LISTENING) {
            state->rejected_events++;
            break;
        }
        state->conversation = BOT_CONVERSATION_LISTENING;
        state->utterance_active = true;
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
        state->conversation = BOT_CONVERSATION_THINKING;
        state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_PLAYBACK_STARTED:
        if (!is_active(runtime) || state->settings_open || state->critical_fault ||
            state->utterance_active || event->data.playback.id == 0 ||
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
        if (!is_active(runtime) || !token_matches(runtime, &event->token, false) ||
            state->conversation != BOT_CONVERSATION_SPEAKING ||
            event->data.playback.id == 0 || event->data.playback.id != state->playback_id) {
            state->rejected_events++;
            break;
        }
        state->playback_id = 0;
        state->conversation = BOT_CONVERSATION_WAITING;
        state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_EVENT_LINK_STATE: {
        bool was_secure = secure_link(runtime);
        state->link_connected = event->data.link.connected;
        state->link_authenticated = state->link_connected && event->data.link.authenticated;
        state->link_encrypted = state->link_connected && event->data.link.encrypted;
        if (was_secure && !secure_link(runtime) && is_active(runtime)) begin_turn(runtime, now_ms);
        break;
    }
    case BOT_EVENT_AUDIO_HEALTH: {
        bool was_available = state->audio_available;
        state->audio_available = event->data.health.available;
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
        if (!is_active(runtime)) set_passive(runtime, now_ms);
        break;
    case BOT_EVENT_SESSION_END:
        if (!is_active(runtime) || !token_matches(runtime, &event->token, false)) {
            state->rejected_events++;
            break;
        }
        end_session(runtime, now_ms, false);
        break;
    case BOT_EVENT_FAULT:
        end_session(runtime, now_ms, true);
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
        "surprised", "sleepy", "shy", "blink", "bored", "drowsy", "sleeping", "speaking" };
    if (!memchr(name, '\0', BOT_EXPRESSION_MAX_BYTES + 1)) return false;
    for (size_t i = 0; i < sizeof(names) / sizeof(names[0]); i++)
        if (strcmp(names[i], name) == 0) return true;
    return false;
}

static bot_command_result_t handle_command(bot_runtime_t *runtime, const bot_command_t *command,
                                           uint32_t now_ms)
{
    bot_runtime_snapshot_t *state = &runtime->state;
    if (command->kind < BOT_COMMAND_SET_EXPRESSION || command->kind > BOT_COMMAND_END_SESSION)
        return BOT_COMMAND_REJECTED_INVALID;
    if (state->critical_fault) return BOT_COMMAND_REJECTED_FAULT;
    if (state->settings_open) return BOT_COMMAND_REJECTED_SETTINGS;
    if (!secure_link(runtime)) return BOT_COMMAND_REJECTED_UNTRUSTED;
    if (!is_active(runtime)) return BOT_COMMAND_REJECTED_PASSIVE;
    if (!token_matches(runtime, &command->token, true)) return BOT_COMMAND_REJECTED_STALE;
    uint32_t ttl = command->ttl_ms ? command->ttl_ms : runtime->config.command_max_age_ms;
    if (ttl > runtime->config.command_max_age_ms) ttl = runtime->config.command_max_age_ms;
    if (!within_age(now_ms, command->received_ms, ttl)) return BOT_COMMAND_REJECTED_EXPIRED;
    switch (command->kind) {
    case BOT_COMMAND_SET_EXPRESSION:
        if (!known_expression(command->data.expression)) return BOT_COMMAND_REJECTED_INVALID;
        memset(state->expression, 0, sizeof(state->expression));
        memcpy(state->expression, command->data.expression, strlen(command->data.expression));
        break;
    case BOT_COMMAND_SET_CAPTION:
        if (!valid_text(command->data.caption, sizeof(command->data.caption)))
            return BOT_COMMAND_REJECTED_INVALID;
        memset(state->caption, 0, sizeof(state->caption));
        memcpy(state->caption, command->data.caption, strlen(command->data.caption));
        break;
    case BOT_COMMAND_THINKING:
        if (state->utterance_active ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_THINKING))
            return BOT_COMMAND_REJECTED_INVALID;
        state->conversation = BOT_CONVERSATION_THINKING;
        reset_expression(runtime);
        break;
    case BOT_COMMAND_REPLY_DONE:
        if (state->utterance_active ||
            (state->conversation != BOT_CONVERSATION_LISTENING &&
             state->conversation != BOT_CONVERSATION_THINKING))
            return BOT_COMMAND_REJECTED_INVALID;
        /* Audio completion is a local event. This receipt closes a text-only
         * reply; a remote server cannot declare hardware playback completed. */
        state->conversation = BOT_CONVERSATION_WAITING;
        state->last_interaction_ms = now_ms;
        reset_expression(runtime);
        break;
    case BOT_COMMAND_END_SESSION:
        end_session(runtime, now_ms, false);
        break;
    default: return BOT_COMMAND_REJECTED_INVALID;
    }
    return BOT_COMMAND_ACCEPTED;
}

bot_runtime_config_t bot_runtime_default_config(void)
{
    return (bot_runtime_config_t){
        .idle_to_bored_ms = CONFIG_BOT_RUNTIME_BORED_AFTER_MS,
        .idle_to_drowsy_ms = CONFIG_BOT_RUNTIME_DROWSY_AFTER_MS,
        .idle_to_sleep_ms = CONFIG_BOT_RUNTIME_SLEEP_AFTER_MS,
        .waking_ms = CONFIG_BOT_RUNTIME_WAKING_MS,
        .session_timeout_ms = CONFIG_BOT_RUNTIME_PROGRESS_TIMEOUT_MS,
        .wake_debounce_ms = CONFIG_BOT_RUNTIME_WAKE_DEBOUNCE_MS,
        .event_max_age_ms = 2000, .command_max_age_ms = 5000,
        .max_events_per_step = 32, .max_commands_per_step = 8,
        .sleep_display =
#ifdef CONFIG_BOT_RUNTIME_SLEEP_DISPLAY
            true,
#else
            false,
#endif
        .allow_audio_uplink = false, .allow_barge_in = false,
        .focus = attention_manager_default_config()
    };
}

static bool valid_config(const bot_runtime_config_t *config)
{
    return config->idle_to_bored_ms > 0 &&
        config->idle_to_bored_ms < config->idle_to_drowsy_ms &&
        config->idle_to_drowsy_ms < config->idle_to_sleep_ms &&
        config->idle_to_sleep_ms < INT32_MAX && config->waking_ms < INT32_MAX &&
        config->session_timeout_ms > config->waking_ms && config->session_timeout_ms < INT32_MAX &&
        config->wake_debounce_ms < INT32_MAX && config->event_max_age_ms > 0 &&
        config->event_max_age_ms < INT32_MAX && config->command_max_age_ms > 0 &&
        config->command_max_age_ms < INT32_MAX &&
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
    runtime->state.last_interaction_ms = now_ms;
    runtime->state.wake_status = BOT_WAKE_MODEL_MISSING;
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
    end_session(runtime, runtime->state.last_interaction_ms, false);
    publish_state(runtime);
    free(runtime);
}

void bot_runtime_step(bot_runtime_t *runtime, uint32_t now_ms)
{
    if (!runtime) return;
    now_ms = read_now_ms(runtime, now_ms);
    /* Debounce/barrier clocks are short leases, not a permanent comparison to
     * a boot-old timestamp. Expire them while stepping across timer wraps. */
    if (runtime->has_wake && !within_age(now_ms, runtime->last_wake_ms,
                                         runtime->config.wake_debounce_ms)) runtime->has_wake = false;
    if (runtime->wake_barrier_valid && !within_age(now_ms, runtime->wake_barrier_ms,
                                                  runtime->config.event_max_age_ms))
        runtime->wake_barrier_valid = false;
    for (unsigned kind = 0; kind < BOT_EVENT_COUNT; kind++) {
        if (runtime->event_seen[kind] && !within_age(now_ms, runtime->event_times[kind],
                                                    runtime->config.event_max_age_ms))
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
    if (!is_active(runtime)) {
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
    const vatt_snapshot_t *vision = runtime->has_vision &&
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
    if (is_active(runtime)) {
        if (!within_age(now_ms, runtime->state.last_interaction_ms, runtime->config.session_timeout_ms)) {
            end_session(runtime, now_ms, false);
        } else if (runtime->state.behavior == BOT_BEHAVIOR_WAKING &&
                   (int32_t)(now_ms - runtime->waking_since_ms) >= 0 &&
                   (uint32_t)(now_ms - runtime->waking_since_ms) >= runtime->config.waking_ms) {
            runtime->state.behavior = BOT_BEHAVIOR_ENGAGED;
            reset_expression(runtime);
        }
    } else {
        set_passive(runtime, now_ms);
    }
    derive_permissions(runtime);
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
    end_session(runtime, now_ms, false);
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

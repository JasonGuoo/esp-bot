/* Host-level integration test: real runtime and attention manager, minimal bus. */
#include "bot_runtime.h"
#include <assert.h>
#include <math.h>
#include <string.h>

struct bot_bus {
    bool has_event, has_vision, has_command;
    bot_event_t event;
    bot_command_t command;
    vatt_snapshot_t vision;
};

bool bot_bus_receive(bot_bus_t *bus, bot_event_t *event)
{
    if (!bus->has_event) return false;
    *event = bus->event;
    bus->has_event = false;
    return true;
}
bool bot_bus_receive_command(bot_bus_t *bus, bot_command_t *command)
{
    if (!bus->has_command) return false;
    *command = bus->command;
    bus->has_command = false;
    return true;
}
bot_bus_signals_t bot_bus_take_signals(bot_bus_t *bus)
{
    (void)bus;
    return (bot_bus_signals_t){0};
}
bool bot_bus_read_vision(bot_bus_t *bus, vatt_snapshot_t *vision)
{
    if (!bus->has_vision) return false;
    *vision = bus->vision;
    return true;
}
bool bot_bus_clear_vision(bot_bus_t *bus)
{
    bus->has_vision = false;
    return true;
}

static void camera_health(bot_bus_t *bus, uint32_t now, bool available)
{
    bus->event = (bot_event_t){.kind = BOT_EVENT_CAMERA_HEALTH,
        .source = BOT_SOURCE_VISION, .observed_ms = now,
        .data.health = {.available = available}};
    bus->has_event = true;
}

static void set_night(bot_bus_t *bus, uint32_t now, bool night)
{
    bus->event = (bot_event_t){.kind = BOT_EVENT_NIGHT_POLICY,
        .source = BOT_SOURCE_PLATFORM, .observed_ms = now,
        .data.night = {.clock_valid = true, .night = night}};
    bus->has_event = true;
}

static void set_face(bot_bus_t *bus, uint32_t now, float height)
{
    memset(&bus->vision, 0, sizeof(bus->vision));
    bus->vision.timestamp_ms = now;
    bus->vision.state = VATT_FACE_PRESENT;
    bus->vision.face_count = 1;
    vatt_face_t *face = &bus->vision.faces[0];
    face->track_id = 1;
    face->visible = true;
    face->last_seen_ms = now;
    face->observation.face_confidence = 0.9f;
    face->observation.bbox = (vatt_box_t){0, 0, 0.5f, height};
    bus->has_vision = true;
}

static void post_event(bot_bus_t *bus, bot_runtime_t *runtime, bot_event_t event)
{
    bus->event = event;
    bus->has_event = true;
    bot_runtime_step(runtime, event.observed_ms);
}

static void test_processing_requires_server_ack(bool failed_followup)
{
    bot_runtime_config_t config = bot_runtime_default_config();
    config.waking_ms = 50;
    config.session_timeout_ms = 1000;
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    assert(bot_runtime_create(&bus, &config, NULL, NULL, 1000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_AUDIO_HEALTH, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1010, .data.health={.available=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1020, .data.touch={.intentional=true}});
    bot_runtime_step(runtime, 1080);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_ENGAGED && state.conversation == BOT_CONVERSATION_LISTENING);
    const bot_session_token_t token = state.token;
    for (uint32_t t = 1100; t < 2000; t += 200) {
        post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_STARTED,
            .source=BOT_SOURCE_AUDIO, .observed_ms=t, .token=token});
        post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_ENDED,
            .source=BOT_SOURCE_AUDIO, .observed_ms=t+20, .token=token});
        bot_runtime_get_snapshot(runtime, &state);
        assert(state.conversation == BOT_CONVERSATION_LISTENING);
        assert(strcmp(state.expression, "listening") == 0);
        assert(state.last_interaction_ms == 1020); /* offline noise is not progress */
        if (t == 1100) {
            bus.command = (bot_command_t){.kind=BOT_COMMAND_THINKING, .token=token, .received_ms=1130};
            bus.has_command = true;
            bot_runtime_step(runtime, 1130);
            bot_runtime_get_snapshot(runtime, &state);
            assert(state.conversation == BOT_CONVERSATION_LISTENING && state.rejected_commands == 1);
        }
    }
    bot_runtime_step(runtime, 2021);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_IDLE && !state.audio_session_requested);
    bot_runtime_destroy(runtime);

    bus = (bot_bus_t){0};
    runtime = NULL;
    config.allow_audio_uplink = true;
    assert(bot_runtime_create(&bus, &config, NULL, NULL, 10000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_AUDIO_HEALTH, .source=BOT_SOURCE_AUDIO,
        .observed_ms=10010, .data.health={.available=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_LINK_STATE, .source=BOT_SOURCE_LINK,
        .observed_ms=10020, .data.link={.connected=true,.authenticated=true,.encrypted=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=10030, .data.touch={.intentional=true}});
    bot_runtime_step(runtime, 10090);
    bot_runtime_get_snapshot(runtime, &state);
    const bot_session_token_t secure_token = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_STARTED,
        .source=BOT_SOURCE_AUDIO, .observed_ms=10100, .token=secure_token});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_ENDED,
        .source=BOT_SOURCE_AUDIO, .observed_ms=10120, .token=secure_token});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.conversation == BOT_CONVERSATION_LISTENING && state.audio_uplink_permitted);
    bus.command = (bot_command_t){.kind=BOT_COMMAND_THINKING, .token=secure_token, .received_ms=10140};
    bus.has_command = true;
    bot_runtime_step(runtime, 10140);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.conversation == BOT_CONVERSATION_THINKING && strcmp(state.expression, "thinking") == 0);
    bus.command = (bot_command_t){.kind = BOT_COMMAND_REPLY_DONE, .token = secure_token,
        .received_ms = 10160, .data.caption = "蓝盆高 4 厘米"};
    bus.has_command = true;
    bot_runtime_step(runtime, 10160);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.reading_token.session_id == secure_token.session_id);
    assert(state.followup_armed && !state.audio_uplink_permitted);
    const bot_session_token_t armed_token = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_UTTERANCE_STARTED,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 10200, .token = secure_token});
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.utterance_active && state.followup_armed); /* old START never gets relabelled */
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_UTTERANCE_STARTED,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 10220, .token = armed_token});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.utterance_active && !state.followup_armed && state.audio_uplink_permitted);
    assert(state.token.epoch == armed_token.epoch); /* no second DMA fence at the first word */
    assert(state.token.turn_id != secure_token.turn_id);
    assert(state.reading_token.turn_id == secure_token.turn_id);
    if (failed_followup) {
        const bot_session_token_t followup = state.token;
        post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_UTTERANCE_ENDED,
            .source = BOT_SOURCE_AUDIO, .observed_ms = 10240, .token = followup});
        bus.command = (bot_command_t){.kind = BOT_COMMAND_REPLY_DONE, .token = followup,
            .received_ms = 10260};
        bus.has_command = true;
        bot_runtime_step(runtime, 10260);
    } else {
        bot_runtime_step(runtime, 12000);
    }
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.audio_uplink_permitted && state.token.session_id == 0);
    assert(state.display_awake_requested && strcmp(state.reading_text, "蓝盆高 4 厘米") == 0);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_RESULT_DISMISSED,
        .source = BOT_SOURCE_LOCAL_UI, .observed_ms = 12020});
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.reading_token.session_id && !*state.reading_text && !state.audio_uplink_permitted);
    bot_runtime_destroy(runtime);
}

static void test_passive_sound_reactions(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    assert(bot_runtime_create(&bus, NULL, NULL, NULL, 1000, &runtime) == ESP_OK);
    bot_event_t sound = {.kind = BOT_EVENT_SOUND_ACTIVITY, .source = BOT_SOURCE_AUDIO,
        .observed_ms = 1020, .data.sound = {.direction_valid = true, .azimuth_deg = 135, .quality = 4}};
    post_event(&bus, runtime, sound);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.sound_reaction && state.sound_direction_valid && state.sound_azimuth_deg == 135);
    assert(strcmp(state.expression, "curious") == 0 && state.behavior == BOT_BEHAVIOR_IDLE);
    assert(state.token.session_id == 0 && !state.audio_session_requested && !state.audio_uplink_permitted);
    bot_runtime_step(runtime, 2821);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.sound_reaction && !state.sound_direction_valid);

    sound.observed_ms = 2840;
    sound.data.sound.direction_valid = false;
    post_event(&bus, runtime, sound);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.sound_reaction && !state.sound_direction_valid);

    camera_health(&bus, 2860, true);
    set_face(&bus, 2860, 0.5f);
    bot_runtime_step(runtime, 2860);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face && strcmp(state.expression, "curious") != 0);

    set_night(&bus, 2880, true);
    bot_runtime_step(runtime, 2880);
    sound.observed_ms = 2900;
    post_event(&bus, runtime, sound);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_NIGHT_DARK && !state.sound_reaction);
    assert(state.token.session_id == 0 && !state.audio_uplink_permitted);

    set_night(&bus, 2920, false);
    bot_runtime_step(runtime, 2920);
    sound.observed_ms = 2940;
    sound.data.sound.direction_valid = true;
    sound.data.sound.azimuth_deg = NAN;
    uint32_t rejected = state.rejected_events;
    post_event(&bus, runtime, sound);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.sound_reaction && state.rejected_events == rejected + 1);
    sound.observed_ms = 2960;
    sound.source = BOT_SOURCE_LINK;
    sound.data.sound.azimuth_deg = 90;
    post_event(&bus, runtime, sound);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.sound_reaction && state.rejected_events == rejected + 2);
    bot_runtime_destroy(runtime);
}

static void light_sample(bot_runtime_t *runtime, uint8_t luma, uint32_t now)
{
    bot_runtime_observe_light(runtime, luma, now, now);
    bot_runtime_step(runtime, now);
}

static void test_ambient_sleep_and_recovery(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    assert(bot_runtime_create(&bus, NULL, NULL, NULL, 1000, &runtime) == ESP_OK);
    /* Brief darkness and a missing camera never count as continuous darkness. */
    light_sample(runtime, 10, 1000);
    light_sample(runtime, 10, 5000);
    bot_runtime_step(runtime, 10000);
    light_sample(runtime, 10, 10001);
    for (uint32_t t = 11001; t <= 39001; t += 1000) light_sample(runtime, 10, t);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.ambient_dark && state.behavior == BOT_BEHAVIOR_IDLE);
    light_sample(runtime, 10, 40001);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.ambient_dark && state.behavior == BOT_BEHAVIOR_SLEEPING);
    assert(!state.display_awake_requested && !state.camera_requested && !state.ambient_probe);
    assert(state.token.session_id == 0 && !state.audio_uplink_permitted);
    bot_runtime_step(runtime, 70000);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.ambient_probe && !state.ambient_sample_valid);
    bot_runtime_step(runtime, 70001);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.ambient_probe && state.camera_requested && !state.display_awake_requested);
    /* A short bright flash cannot wake the robot. */
    light_sample(runtime, 80, 71001);
    light_sample(runtime, 20, 72001);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.ambient_dark);
    for (uint32_t t = 73001; t <= 76001; t += 1000) light_sample(runtime, 80, t);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.ambient_dark && state.behavior == BOT_BEHAVIOR_IDLE);
    assert(state.display_awake_requested && state.camera_requested && !state.ambient_probe);
    assert(state.token.session_id == 0 && !state.audio_session_requested);
    bot_runtime_destroy(runtime);
}

static void test_ambient_session_and_night_priority(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    assert(bot_runtime_create(&bus, NULL, NULL, NULL, 1000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE,
        .source=BOT_SOURCE_LOCAL_UI, .observed_ms=1010, .data.touch={.intentional=true}});
    for (uint32_t t = 2000; t <= 32000; t += 1000) light_sample(runtime, 10, t);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.ambient_dark && state.behavior == BOT_BEHAVIOR_ENGAGED);
    assert(state.display_awake_requested && state.token.session_id != 0);
    set_night(&bus, 32010, true);
    bot_runtime_step(runtime, 32010);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_LOCAL_STOP,
        .source=BOT_SOURCE_LOCAL_UI, .observed_ms=32020});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_NIGHT_DARK && !state.camera_requested);
    bot_runtime_step(runtime, 92020);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.ambient_probe && state.behavior == BOT_BEHAVIOR_NIGHT_DARK);
    set_night(&bus, 92030, false);
    bot_runtime_step(runtime, 92030);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_SLEEPING && state.ambient_dark);
    bot_runtime_destroy(runtime);
}

typedef struct {
    unsigned queued, cancelled, previews;
    uint8_t gain;
    bool fail_enqueue, hold_tail;
    bot_runtime_snapshot_t published;
    const bot_response_entry_t *entry;
} response_probe_t;

static void response_state(void *context, const bot_runtime_snapshot_t *state)
{
    ((response_probe_t *)context)->published = *state;
}

static bool queue_response(void *context, const bot_session_token_t *token,
                            const bot_response_entry_t *entry, uint8_t gain_percent)
{
    response_probe_t *probe = context;
    /* The speaker adapter must never run before the microphone gate closes. */
    assert(probe->published.local_response_active);
    assert(!probe->published.audio_session_requested && !probe->published.audio_uplink_permitted);
    assert(token->epoch == probe->published.token.epoch);
    assert(entry && entry->pcm_samples > 0 && entry->audio_bytes > 0);
    probe->entry = entry;
    probe->gain = gain_percent;
    probe->queued++;
    return !probe->fail_enqueue;
}
static bool queue_preview(void *context, const bot_session_token_t *token,
                           const bot_response_entry_t *entry, uint8_t gain_percent)
{
    response_probe_t *probe = context;
    assert(probe->published.settings_open && probe->published.sound_preview_active);
    assert(!probe->published.audio_session_requested && !probe->published.audio_uplink_permitted);
    assert(token->epoch && !token->session_id && !token->turn_id);
    assert(entry && gain_percent <= 100);
    probe->previews++;
    return true;
}

static void cancel_response(void *context)
{
    ((response_probe_t *)context)->cancelled++;
}
static bool response_tail_clear(void *context)
{
    return !((response_probe_t *)context)->hold_tail;
}

static void test_wake_response_ownership(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    response_probe_t probe = {0};
    bot_runtime_config_t config = bot_runtime_default_config();
    config.waking_ms = 50;
    config.allow_audio_uplink = true;
    config.voice_settings.night_gain_percent = 20;
    const bot_runtime_ops_t ops = {.apply_state = response_state,
        .play_wake_response = queue_response, .stop_output = cancel_response,
        .wake_response_tail_clear = response_tail_clear};
    assert(bot_runtime_create(&bus, &config, &ops, &probe, 1000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_AUDIO_HEALTH,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1010, .data.health = {.available = true}});
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_WAKE_STATUS,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1020, .data.wake_status = {.status = BOT_WAKE_READY}});
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_NIGHT_POLICY,
        .source = BOT_SOURCE_PLATFORM, .observed_ms = 1030,
        .data.night = {.clock_valid = true, .night = true, .local_hour = 23}});
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_LINK_STATE,
        .source = BOT_SOURCE_LINK, .observed_ms = 1040,
        .data.link = {.connected = true, .authenticated = true, .encrypted = true}});
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_SOUND_ACTIVITY,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1050});
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_WAKE_DETECTED,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1060, .data.wake = {.phrase = BOT_WAKE_PHRASE_NONE}});
    assert(probe.queued == 0);
    bot_event_t wake = {.kind = BOT_EVENT_WAKE_DETECTED, .source = BOT_SOURCE_AUDIO,
        .observed_ms = 1100, .data.wake = {.phrase = BOT_WAKE_PHRASE_XIAODAI_XIAODAI}};
    post_event(&bus, runtime, wake);
    bot_runtime_get_snapshot(runtime, &state);
    assert(probe.queued == 1 && state.local_response_active);
    assert(strcmp(probe.entry->scene, "night") == 0 && probe.entry->gain_percent <= 30);
    assert(probe.gain == 20); /* user's relative night setting applied once by the player */
    assert(strcmp(state.caption, probe.entry->text) == 0);
    assert(strcmp(state.expression, probe.entry->expression) == 0);
    assert(!state.audio_session_requested && !state.audio_uplink_permitted);
    const bot_session_token_t first = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_UTTERANCE_STARTED,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1120, .token = first});
    bus.command = (bot_command_t){.kind = BOT_COMMAND_SET_CAPTION, .token = first,
        .received_ms = 1125, .data.caption = "remote must not interrupt local acknowledgement"};
    bus.has_command = true;
    bot_runtime_step(runtime, 1125);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.utterance_active && strcmp(state.caption, probe.entry->text) == 0);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_PLAYBACK_STARTED,
        .source = BOT_SOURCE_AUDIO, .observed_ms = 1130, .token = first,
        .data.playback = {.id = 41, .local_response = true}});
    /* Going offline cannot interrupt an acknowledgement that needs no server. */
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_LINK_STATE,
        .source = BOT_SOURCE_LINK, .observed_ms = 1150});
    bot_runtime_step(runtime, 2000);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.conversation == BOT_CONVERSATION_SPEAKING && state.local_response_active);
    assert(state.token.epoch == first.epoch && strcmp(state.expression, probe.entry->expression) == 0);
    bot_event_t done = {.kind = BOT_EVENT_PLAYBACK_DONE, .source = BOT_SOURCE_AUDIO,
        .observed_ms = 2150, .token = first, .data.playback = {.id = 41, .local_response = true}};
    probe.hold_tail = true;
    post_event(&bus, runtime, done);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.local_response_active && !state.audio_session_requested);
    assert(!state.audio_uplink_permitted && strcmp(state.expression, probe.entry->expression) == 0);
    probe.hold_tail = false;
    bot_runtime_step(runtime, 2200);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.local_response_active && state.audio_session_requested && !state.audio_uplink_permitted);
    assert(state.conversation == BOT_CONVERSATION_LISTENING && !state.caption[0]);

    /* New turn cancellation makes a queued old playback completion powerless. */
    wake.observed_ms = 4000;
    post_event(&bus, runtime, wake);
    bot_runtime_get_snapshot(runtime, &state);
    const bot_session_token_t second = state.token;
    assert(probe.queued == 2 && state.local_response_active);
    done.observed_ms = 4020;
    post_event(&bus, runtime, done);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.local_response_active && state.token.epoch == second.epoch);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_SETTINGS_OPEN,
        .source = BOT_SOURCE_LOCAL_UI, .observed_ms = 4040});
    done.observed_ms = 4060;
    done.token = second;
    post_event(&bus, runtime, done);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.settings_open && !state.local_response_active && !state.audio_session_requested);
    assert(state.token.session_id == 0 && probe.cancelled > 0);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_SETTINGS_CLOSE,
        .source = BOT_SOURCE_LOCAL_UI, .observed_ms = 4080});

    /* A lost output callback has a finite recovery bound. */
    wake.observed_ms = 7000;
    post_event(&bus, runtime, wake);
    bot_runtime_step(runtime, 23001);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.local_response_active && state.audio_session_requested);
    assert(state.conversation == BOT_CONVERSATION_LISTENING);

    /* Queue and pre-playback decoder failures also return to listening. */
    probe.fail_enqueue = true;
    wake.observed_ms = 25000;
    post_event(&bus, runtime, wake);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.local_response_active && state.audio_session_requested);
    probe.fail_enqueue = false;
    wake.observed_ms = 28000;
    post_event(&bus, runtime, wake);
    bot_runtime_get_snapshot(runtime, &state);
    post_event(&bus, runtime, (bot_event_t){.kind = BOT_EVENT_LOCAL_RESPONSE_FAILED,
        .source = BOT_SOURCE_SYSTEM, .observed_ms = 28020, .token = state.token});
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.local_response_active && state.audio_session_requested);
    bot_runtime_destroy(runtime);
}

static void test_sound_preview_and_offline_commands(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    response_probe_t probe = {0};
    const bot_runtime_ops_t ops = {.apply_state = response_state, .stop_output = cancel_response,
        .play_sound_preview = queue_preview};
    assert(bot_runtime_create(&bus, NULL, &ops, &probe, 1000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_AUDIO_HEALTH, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1010, .data.health={.available=true}});
    bot_event_t local = {.kind=BOT_EVENT_LOCAL_COMMAND, .source=BOT_SOURCE_AUDIO, .observed_ms=1020,
        .data.local_command={.command=BOT_VOICE_COMMAND_VOLUME_UP, .score=.95f}};
    post_event(&bus, runtime, local);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.voice_settings.volume_percent == 30 && !state.token.session_id);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_SOUND_PREVIEW, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1030});
    assert(probe.previews == 0); /* explicit audition is still settings-only */
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_SETTINGS_OPEN, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1040});
    bot_voice_settings_t settings = bot_voice_default_settings();
    settings.volume_percent = 40;
    settings.spoken_answers = true;
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_VOICE_SETTINGS, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1050, .data.voice_settings=settings});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_SOUND_PREVIEW, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1060});
    bot_runtime_get_snapshot(runtime, &state);
    assert(probe.previews == 1 && state.sound_preview_active && !state.token.session_id);
    const bot_session_token_t preview = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_PLAYBACK_STARTED, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1070, .token=preview, .data.playback={.id=42,.preview=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_SETTINGS_CLOSE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1080});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_PLAYBACK_DONE, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1090, .token=preview, .data.playback={.id=42,.preview=true}});
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.sound_preview_active && !state.audio_session_requested && !state.audio_uplink_permitted);
    assert(!state.token.session_id && probe.cancelled > 0);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1100, .data.touch={.intentional=true}});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.audio_session_requested && !state.link_authenticated);
    local.token = state.token;
    local.observed_ms = 1120;
    post_event(&bus, runtime, local);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.voice_settings.volume_percent == 45 && state.followup_armed);
    assert(!state.audio_uplink_permitted && state.token.epoch != local.token.epoch);
    local.observed_ms = 1130;
    post_event(&bus, runtime, local); /* the consumed old token cannot execute twice */
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.voice_settings.volume_percent == 45);
    local.token = state.token;
    local.observed_ms = 1140;
    local.data.local_command.command = BOT_VOICE_COMMAND_SLEEP;
    post_event(&bus, runtime, local);
    bot_runtime_step(runtime, 1150);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_SLEEPING && !state.token.session_id);
    assert(!state.display_awake_requested && !state.camera_requested && !state.audio_session_requested);
    set_night(&bus, 1160, false);
    bot_runtime_step(runtime, 1160);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_SLEEPING); /* daylight cannot undo explicit sleep */
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=4000, .data.touch={.intentional=true}});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_WAKING && state.audio_session_requested);
    bot_runtime_destroy(runtime);
}

static void test_spoken_result_hardware_order(bool hardware_first)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    bot_runtime_config_t config = bot_runtime_default_config();
    config.voice_settings.spoken_answers = true;
    config.allow_audio_uplink = true;
    assert(bot_runtime_create(&bus, &config, NULL, NULL, 1000, &runtime) == ESP_OK);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_AUDIO_HEALTH, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1010, .data.health={.available=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_LINK_STATE, .source=BOT_SOURCE_LINK,
        .observed_ms=1020, .data.link={.connected=true,.authenticated=true,.encrypted=true}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI,
        .observed_ms=1030, .data.touch={.intentional=true}});
    bot_runtime_get_snapshot(runtime, &state);
    const bot_session_token_t token = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_CAPTURE_READY, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1040, .token=token, .data.capture={.ready=true}});
    bus.event = (bot_event_t){.kind=BOT_EVENT_CAPTURE_READY, .source=BOT_SOURCE_LINK,
        .observed_ms=1035, .token=token, .data.capture={.ready=true}};
    bus.has_event = true;
    bot_runtime_step(runtime, 1050); /* a separate producer may arrive slightly out of order */
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.capture_ready && state.transport_ready);
    assert(!state.utterance_seen && !state.audio_uplink_permitted); /* silence cannot start a task */
    bus.command = (bot_command_t){.kind=BOT_COMMAND_REPLY_PRESENT, .token=token,
        .received_ms=1100, .data.caption="这张图片的说明"};
    if (!hardware_first) {
        bus.has_command = true;
        bot_runtime_step(runtime, 1100);
        bot_runtime_get_snapshot(runtime, &state);
        assert(state.token.epoch == token.epoch && !state.followup_armed);
    }
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_PLAYBACK_STARTED, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1110, .token=token, .data.playback={.id=23}});
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_PLAYBACK_PROGRESS, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1115, .token=token, .data.playback={.id=23}});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.last_interaction_ms == 1115 && state.conversation == BOT_CONVERSATION_SPEAKING);
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_PLAYBACK_DONE, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1120, .token=token, .data.playback={.id=23}});
    if (hardware_first) {
        bot_runtime_get_snapshot(runtime, &state);
        assert(state.token.epoch == token.epoch && !state.followup_armed);
        bus.command.received_ms = 1130;
        bus.has_command = true;
        bot_runtime_step(runtime, 1130);
    }
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.followup_armed && state.conversation == BOT_CONVERSATION_WAITING);
    assert(state.token.epoch != token.epoch && !state.audio_uplink_permitted);
    assert(!state.capture_ready && !state.transport_ready);
    assert(state.reading_token.epoch == token.epoch && !strcmp(state.reading_text,"这张图片的说明"));
    bus.command = (bot_command_t){.kind=BOT_COMMAND_IMAGE_FAILED, .token=token,
        .received_ms=1140, .data.caption="图片下载失败"};
    bus.has_command = true;
    bot_runtime_step(runtime, 1140);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!strcmp(state.image_notice,"图片下载失败")); /* retained result lease */
    const bot_session_token_t next = state.token;
    post_event(&bus, runtime, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_STARTED, .source=BOT_SOURCE_AUDIO,
        .observed_ms=1150, .token=next});
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.utterance_active && state.token.epoch == next.epoch && state.audio_uplink_permitted);
    bot_runtime_destroy(runtime);
}

int main(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_snapshot_t state;
    assert(bot_runtime_create(&bus, NULL, NULL, NULL, 1000, &runtime) == ESP_OK);
    bot_runtime_step(runtime, 1000);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_IDLE && state.camera_requested);
    assert(!state.near_face && state.token.session_id == 0);

    camera_health(&bus, 1020, true);
    set_face(&bus, 1020, 0.42f);
    bot_runtime_step(runtime, 1020);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face && state.near_face_area_permille == 210);
    assert(state.behavior == BOT_BEHAVIOR_IDLE && state.conversation == BOT_CONVERSATION_NONE);
    assert(state.token.session_id == 0 && !state.audio_session_requested && !state.audio_uplink_permitted);
    assert(!state.visual_tracking_permitted && !state.focus.localization_valid);

    set_face(&bus, 1040, 0.34f);
    bot_runtime_step(runtime, 1040);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face); /* short loss grace */
    set_face(&bus, 1441, 0.34f);
    bot_runtime_step(runtime, 1441);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.near_face);

    set_face(&bus, 1460, 0.5f);
    bot_runtime_step(runtime, 1460);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face);
    camera_health(&bus, 1480, false);
    bot_runtime_step(runtime, 1480);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face && state.camera_requested); /* brief camera dropout */
    bot_runtime_step(runtime, 1861);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.near_face && state.camera_requested);

    set_night(&bus, 1880, true);
    bot_runtime_step(runtime, 1880);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_NIGHT_DARK && !state.camera_requested);
    assert(!state.near_face && state.token.session_id == 0);
    bot_runtime_step(runtime, 1900);
    assert(!bus.has_vision);

    set_night(&bus, 1920, false);
    bot_runtime_step(runtime, 1920);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.behavior == BOT_BEHAVIOR_IDLE && state.camera_requested && !state.near_face);
    bool saw_expression = false;
    for (uint32_t t = 1940; t <= 13000; t += 20) {
        bot_runtime_step(runtime, t);
        bot_runtime_get_snapshot(runtime, &state);
        assert(state.behavior == BOT_BEHAVIOR_IDLE && state.token.session_id == 0);
        if (strcmp(state.expression, "idle") != 0) saw_expression = true;
    }
    assert(saw_expression);
    camera_health(&bus, 13020, true);
    set_face(&bus, 13020, 0.5f);
    bot_runtime_step(runtime, 13020);
    bot_runtime_get_snapshot(runtime, &state);
    assert(state.near_face && strcmp(state.expression, "idle") == 0 && state.token.session_id == 0);
    set_night(&bus, 13040, true);
    bot_runtime_step(runtime, 13040);
    bot_runtime_get_snapshot(runtime, &state);
    assert(!state.near_face && !state.camera_requested && state.behavior == BOT_BEHAVIOR_NIGHT_DARK);
    bot_runtime_destroy(runtime);
    test_processing_requires_server_ack(false);
    test_processing_requires_server_ack(true);
    test_passive_sound_reactions();
    test_ambient_sleep_and_recovery();
    test_ambient_session_and_night_priority();
    test_wake_response_ownership();
    test_sound_preview_and_offline_commands();
    test_spoken_result_hardware_order(false);
    test_spoken_result_hardware_order(true);
    return 0;
}

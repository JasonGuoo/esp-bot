/* Host test: the proactive line engine (UX review §12.2). Every guardrail the
 * device owns is checked against the real runtime with a minimal fake bus. */
#include "bot_runtime.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

struct bot_bus {
    bool has_event, has_vision, has_command;
    bot_event_t event;
    bot_command_t command;
    vatt_snapshot_t vision;
    bot_bus_signals_t signals;
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
bool bot_bus_events_pending(bot_bus_t *bus) { return bus->has_event; }
bot_bus_signals_t bot_bus_take_signals(bot_bus_t *bus)
{
    bot_bus_signals_t signals = bus->signals;
    bus->signals = (bot_bus_signals_t){0};
    return signals;
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


typedef struct { bot_bus_t bus; bot_runtime_t *rt; bot_runtime_snapshot_t s; uint32_t now; bool face; } fx_t;

static bool is_engaged_or_waking(const fx_t *f)
{
    return f->s.behavior == BOT_BEHAVIOR_ENGAGED || f->s.behavior == BOT_BEHAVIOR_WAKING;
}

static void step(fx_t *f, uint32_t dt)
{
    f->now += dt;
    if (f->face) set_face(&f->bus, f->now, 0.3f);
    else { f->bus.has_vision = false; }
    bot_runtime_step(f->rt, f->now);
    bot_runtime_get_snapshot(f->rt, &f->s);
}

static void post(fx_t *f, bot_event_t event)
{
    f->now += 5;
    event.observed_ms = f->now;
    f->bus.event = event;
    f->bus.has_event = true;
    step(f, 0);
}

static void run(fx_t *f, uint32_t ms) { for (uint32_t t = 0; t < ms; t += 100) step(f, 100); }

static void setup_muted(fx_t *f, bool spoken, bool muted)
{
    memset(f, 0, sizeof(*f));
    bot_runtime_config_t config = bot_runtime_default_config();
    config.waking_ms = 50;
    config.proactive_idle_ms = 2000; /* shortened for the test; 60 s on the device */
    config.voice_settings.spoken_answers = spoken;
    config.voice_settings.muted = muted;
    f->now = 1000;
    assert(bot_runtime_create(&f->bus, &config, NULL, NULL, f->now, &f->rt) == ESP_OK);
    post(f, (bot_event_t){.kind=BOT_EVENT_AUDIO_HEALTH, .source=BOT_SOURCE_AUDIO, .data.health={.available=true}});
    post(f, (bot_event_t){.kind=BOT_EVENT_CAMERA_HEALTH, .source=BOT_SOURCE_VISION, .data.health={.available=true}});
    post(f, (bot_event_t){.kind=BOT_EVENT_LINK_STATE, .source=BOT_SOURCE_LINK,
        .data.link={.connected=true,.authenticated=true,.encrypted=true}});
}

static void setup(fx_t *f, bool spoken) { setup_muted(f, spoken, false); }

static bot_command_result_t offer(fx_t *f, uint32_t id, uint32_t ttl_ms, bool show_if_absent, const char *text)
{
    f->bus.command = (bot_command_t){.kind=BOT_COMMAND_PROACTIVE_OFFER, .request_id=id, .received_ms=f->now,
        .ttl_ms=ttl_ms, .presentation_sequence=show_if_absent ? 1U : 0U};
    snprintf(f->bus.command.data.caption, sizeof(f->bus.command.data.caption), "%s", text);
    f->bus.has_command = true;
    const uint32_t rejected = f->s.rejected_commands;
    step(f, 1);
    return f->s.rejected_commands == rejected ? BOT_COMMAND_ACCEPTED : BOT_COMMAND_REJECTED_INVALID;
}

static bool speaking_unprompted(const fx_t *f)
{
    return f->s.proactive_active && f->s.activation == BOT_ACTIVATION_PROACTIVE &&
        f->s.behavior == BOT_BEHAVIOR_ENGAGED;
}

static void assert_never_listening(const fx_t *f)
{
    assert(!f->s.audio_session_requested && !f->s.audio_uplink_permitted && !f->s.capture_ready &&
           !f->s.followup_armed && f->s.conversation != BOT_CONVERSATION_LISTENING &&
           f->s.conversation != BOT_CONVERSATION_WAITING);
}

static void test_speaks_at_a_quiet_moment_without_any_microphone_lease(void)
{
    fx_t f; setup(&f, true);
    assert(offer(&f, 41, 600000, false, "早上好呀！") == BOT_COMMAND_ACCEPTED);
    run(&f, 1000); /* nobody in front of the device: the Mac's proposal just waits */
    assert(!speaking_unprompted(&f));
    f.face = true;
    run(&f, 1000); /* a face, but not yet for 1.5 s */
    assert(!speaking_unprompted(&f));
    run(&f, 800);
    assert(speaking_unprompted(&f) && f.s.proactive_offer_id == 41);
    assert(f.s.conversation == BOT_CONVERSATION_THINKING && !strcmp(f.s.expression, "happy"));
    assert_never_listening(&f);
    const bot_session_token_t token = f.s.token;
    /* Room speech or a VAD edge never turns the line into a listening turn. */
    post(&f, (bot_event_t){.kind=BOT_EVENT_UTTERANCE_STARTED, .source=BOT_SOURCE_AUDIO, .token=token,
        .data.utterance={.echo_safe=true}});
    assert(speaking_unprompted(&f) && !f.s.utterance_active);
    assert_never_listening(&f);
    f.bus.command = (bot_command_t){.kind=BOT_COMMAND_PRESENTATION_SEGMENT, .token=token, .received_ms=f.now,
        .presentation_sequence=1};
    strcpy(f.bus.command.data.caption, "早上好呀！");
    f.bus.has_command = true;
    step(&f, 1);
    assert(speaking_unprompted(&f));
    f.bus.command = (bot_command_t){.kind=BOT_COMMAND_PRESENTATION_DONE, .token=token, .received_ms=f.now};
    f.bus.has_command = true;
    step(&f, 1);
    /* Done: back to passive, no follow-up window, no microphone. */
    assert(!is_engaged_or_waking(&f) && f.s.activation == BOT_ACTIVATION_NONE && !f.s.proactive_active);
    assert(!f.s.proactive_offer_id && !f.s.reading_notice[0]);
    assert_never_listening(&f);
    bot_runtime_destroy(f.rt);
}

static void finish_line(fx_t *f)
{
    f->bus.command = (bot_command_t){.kind=BOT_COMMAND_PRESENTATION_DONE, .token=f->s.token, .received_ms=f->now};
    f->bus.has_command = true;
    step(f, 1);
}

static void test_night_and_settings_delay_the_line(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    post(&f, (bot_event_t){.kind=BOT_EVENT_NIGHT_POLICY, .source=BOT_SOURCE_PLATFORM,
        .data.night={.clock_valid=true, .night=true, .local_hour=23}});
    assert(offer(&f, 7, 600000, false, "晚上好") == BOT_COMMAND_ACCEPTED);
    run(&f, 5000);
    assert(!speaking_unprompted(&f) && f.s.behavior == BOT_BEHAVIOR_NIGHT_DARK);
    post(&f, (bot_event_t){.kind=BOT_EVENT_NIGHT_POLICY, .source=BOT_SOURCE_PLATFORM,
        .data.night={.clock_valid=true, .night=false, .local_hour=8}});
    post(&f, (bot_event_t){.kind=BOT_EVENT_SETTINGS_OPEN, .source=BOT_SOURCE_LOCAL_UI, .settings_lease=1});
    run(&f, 5000);
    assert(!speaking_unprompted(&f) && f.s.settings_open);
    post(&f, (bot_event_t){.kind=BOT_EVENT_SETTINGS_CLOSE, .source=BOT_SOURCE_LOCAL_UI, .settings_lease=1});
    run(&f, 5000);
    assert(speaking_unprompted(&f) && f.s.proactive_offer_id == 7); /* both guards were the only blockers */
    assert_never_listening(&f);
    bot_runtime_destroy(f.rt);
}

static void test_muted_or_text_only_device_never_speaks_unprompted(void)
{
    for (int muted = 0; muted < 2; muted++) {
        fx_t f; setup_muted(&f, muted, muted); /* text-only, then muted */
        f.face = true;
        assert(offer(&f, 9, 600000, false, "你好") == BOT_COMMAND_ACCEPTED);
        run(&f, 6000);
        assert(!speaking_unprompted(&f) && !is_engaged_or_waking(&f));
        bot_runtime_destroy(f.rt);
    }
}

static void test_people_talking_nearby_block_the_moment(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    assert(offer(&f, 11, 600000, false, "你好呀") == BOT_COMMAND_ACCEPTED);
    for (int i = 0; i < 8; i++) {
        post(&f, (bot_event_t){.kind=BOT_EVENT_SOUND_ACTIVITY, .source=BOT_SOURCE_AUDIO,
            .data.sound={.speech=true}});
        run(&f, 1000);
        assert(!speaking_unprompted(&f));
    }
    run(&f, 4000);
    assert(!speaking_unprompted(&f)); /* still inside the 5 s quiet window */
    run(&f, 1500);
    assert(speaking_unprompted(&f));
    bot_runtime_destroy(f.rt);
}

static void test_active_conversation_is_never_interrupted(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    run(&f, 3000);
    post(&f, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI, .data.touch={.intentional=true}});
    step(&f, 100);
    assert(f.s.activation == BOT_ACTIVATION_TOUCH && f.s.audio_session_requested);
    assert(offer(&f, 13, 600000, false, "你好") == BOT_COMMAND_ACCEPTED);
    for (int i = 0; i < 30; i++) {
        run(&f, 100);
        assert(f.s.activation != BOT_ACTIVATION_PROACTIVE && !f.s.proactive_active);
    }
    post(&f, (bot_event_t){.kind=BOT_EVENT_LOCAL_STOP, .source=BOT_SOURCE_LOCAL_UI});
    run(&f, 1000);
    assert(!speaking_unprompted(&f)); /* just interacted: wait out the idle window */
    run(&f, 2000);
    assert(speaking_unprompted(&f));
    bot_runtime_destroy(f.rt);
}

static void test_untrusted_link_and_bad_offers_are_rejected(void)
{
    fx_t f; setup(&f, true);
    assert(offer(&f, 0, 1000, false, "你好") != BOT_COMMAND_ACCEPTED);        /* no id */
    assert(offer(&f, 1, 0, false, "你好") != BOT_COMMAND_ACCEPTED);           /* no lifetime */
    assert(offer(&f, 1, 1800001, false, "你好") != BOT_COMMAND_ACCEPTED);     /* > 30 min */
    assert(offer(&f, 1, 1000, false, "") != BOT_COMMAND_ACCEPTED);            /* no text */
    post(&f, (bot_event_t){.kind=BOT_EVENT_LINK_STATE, .source=BOT_SOURCE_LINK,
        .data.link={.connected=true,.authenticated=false,.encrypted=false}});
    assert(offer(&f, 2, 1000, false, "你好") != BOT_COMMAND_ACCEPTED);
    bot_runtime_destroy(f.rt);
}

static void test_device_gap_and_expiry_reports(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    assert(offer(&f, 21, 600000, false, "早上好") == BOT_COMMAND_ACCEPTED);
    run(&f, 3000);
    assert(speaking_unprompted(&f));
    finish_line(&f);
    assert(!is_engaged_or_waking(&f));
    /* A second line within 20 min is never spoken; it expires unseen. */
    assert(offer(&f, 22, 8000, false, "又见面啦") == BOT_COMMAND_ACCEPTED);
    run(&f, 7000);
    assert(!speaking_unprompted(&f) && f.s.proactive_report_seq == 0);
    run(&f, 1500);
    assert(f.s.proactive_report_id == 22 && f.s.proactive_report_status == 1 && f.s.proactive_report_seq == 1);
    assert(!f.s.caption[0]);
    /* A reminder nobody heard stays on screen instead (and is reported shown). */
    f.face = false;
    assert(offer(&f, 23, 1000, true, "提醒一下：该喝水啦") == BOT_COMMAND_ACCEPTED);
    run(&f, 1200);
    assert(f.s.proactive_report_id == 23 && f.s.proactive_report_status == 2 && f.s.proactive_report_seq == 2);
    assert(!strcmp(f.s.caption, "提醒一下：该喝水啦") && !is_engaged_or_waking(&f));
    /* A newer proposal supersedes an older one, which is reported unspoken. */
    assert(offer(&f, 24, 600000, false, "一") == BOT_COMMAND_ACCEPTED);
    assert(offer(&f, 25, 600000, false, "二") == BOT_COMMAND_ACCEPTED);
    assert(f.s.proactive_report_id == 24 && f.s.proactive_report_status == 1 && f.s.proactive_report_seq == 3);
    bot_runtime_destroy(f.rt);
}

static void test_a_real_wake_replaces_the_line_and_a_mac_decline_is_silent(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    assert(offer(&f, 31, 600000, false, "你好") == BOT_COMMAND_ACCEPTED);
    run(&f, 3000);
    assert(speaking_unprompted(&f));
    const uint32_t proactive_session = f.s.token.session_id;
    post(&f, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE, .source=BOT_SOURCE_LOCAL_UI, .data.touch={.intentional=true}});
    assert(f.s.activation == BOT_ACTIVATION_TOUCH && f.s.token.session_id != proactive_session);
    assert(f.s.audio_session_requested && !f.s.proactive_active && f.s.conversation == BOT_CONVERSATION_LISTENING);
    bot_runtime_destroy(f.rt);

    setup(&f, true);
    f.face = true;
    assert(offer(&f, 32, 600000, false, "你好") == BOT_COMMAND_ACCEPTED);
    run(&f, 3000);
    assert(speaking_unprompted(&f));
    post(&f, (bot_event_t){.kind=BOT_EVENT_SESSION_END, .source=BOT_SOURCE_SYSTEM, .token=f.s.token,
        .data.session_end={.code=BOT_SESSION_END_EXPIRED}});
    assert(!is_engaged_or_waking(&f) && !f.s.caption[0] && !f.s.reading_notice[0]);
    assert_never_listening(&f);
    bot_runtime_destroy(f.rt);
}

int main(void)
{
    test_speaks_at_a_quiet_moment_without_any_microphone_lease();
    test_night_and_settings_delay_the_line();
    test_muted_or_text_only_device_never_speaks_unprompted();
    test_people_talking_nearby_block_the_moment();
    test_active_conversation_is_never_interrupted();
    test_untrusted_link_and_bad_offers_are_rejected();
    test_device_gap_and_expiry_reports();
    test_a_real_wake_replaces_the_line_and_a_mac_decline_is_silent();
    puts("proactive tests passed");
    return 0;
}

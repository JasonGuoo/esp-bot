/* Passive-wake regression against the real runtime with a minimal fake bus. */
#include "bot_runtime.h"
#include <assert.h>
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

static void test_offers_cannot_start_passive_speech(void)
{
    fx_t f; setup(&f, true);
    f.face = true;
    run(&f, 2500);
    assert(offer(&f, 41, 600000, false, "早上好呀！") != BOT_COMMAND_ACCEPTED);
    assert(offer(&f, 42, 600000, true, "提醒一下") != BOT_COMMAND_ACCEPTED);
    run(&f, 5000);
    assert(!is_engaged_or_waking(&f));
    assert(!f.s.proactive_active && !f.s.audio_session_requested && !f.s.audio_uplink_permitted);
    assert(!f.s.caption[0]);

    post(&f, (bot_event_t){.kind=BOT_EVENT_TOUCH_WAKE,.source=BOT_SOURCE_LOCAL_UI,
        .data.touch={.intentional=true}});
    assert(is_engaged_or_waking(&f) && f.s.audio_session_requested);
    assert(offer(&f, 43, 600000, false, "你好") != BOT_COMMAND_ACCEPTED);
    assert(is_engaged_or_waking(&f) && f.s.activation==BOT_ACTIVATION_TOUCH);
    bot_runtime_destroy(f.rt);
}

int main(void)
{
    test_offers_cannot_start_passive_speech();
    puts("passive wake tests passed");
    return 0;
}

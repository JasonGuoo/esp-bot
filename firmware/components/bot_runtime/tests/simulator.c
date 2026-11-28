// Driver-free, line-oriented adapter. All behavior decisions belong to bot_runtime.
#include "host_bus.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static void json_string(const char *s)
{
    putchar('"');
    for (; *s; ++s) {
        unsigned char c = (unsigned char)*s;
        if (c == '"' || c == '\\') putchar('\\');
        if (c < 32) printf("\\u%04x", c);
        else putchar(c);
    }
    putchar('"');
}

static void snapshot(const bot_runtime_snapshot_t *s, uint32_t now)
{
    printf("{\"ms\":%u,\"behavior\":\"%s\",\"conversation\":\"%s\","
           "\"runtime\":{\"epoch\":%u,\"session_id\":%u,\"turn_id\":%u,\"focus_generation\":%u},"
           "\"uplink\":%s,\"followup\":%s,\"rejected\":%u,\"reading\":",
           now, bot_behavior_name(s->behavior), bot_conversation_name(s->conversation),
           s->token.epoch, s->token.session_id, s->token.turn_id, s->token.focus_generation,
           s->audio_uplink_permitted ? "true" : "false", s->followup_armed ? "true" : "false",
           s->rejected_commands);
    json_string(s->reading_text);
    printf(",\"supplement\":%s}\n", s->supplement_turn ? "true" : "false");
    fflush(stdout);
}

int main(void)
{
    bot_bus_t bus = {0};
    bot_runtime_t *runtime = NULL;
    bot_runtime_config_t config = bot_runtime_default_config();
    config.allow_audio_uplink = true;
    uint32_t now = 1000;
    if (bot_runtime_create(&bus, &config, NULL, NULL, now, &runtime) != ESP_OK) return 1;
    bot_session_token_t thinking_token = {0};
    bot_runtime_snapshot_t state;
    char line[1024];
    int status = 0;
    while (fgets(line, sizeof(line), stdin)) {
        if (!strchr(line, '\n') && !feof(stdin)) { status=2; break; }
        line[strcspn(line, "\r\n")] = 0;
        bot_runtime_get_snapshot(runtime, &state);
        now += 20;
        bot_event_t event = {.observed_ms=now, .token=state.token};
        if (!strcmp(line, "touch") || !strcmp(line, "supplement")) {
            event.kind=BOT_EVENT_TOUCH_WAKE; event.source=BOT_SOURCE_LOCAL_UI;
            event.data.touch.intentional=true;
            event.data.touch.supplement=!strcmp(line,"supplement");
        } else if (!strcmp(line,"sound")) {
            event.kind=BOT_EVENT_SOUND_ACTIVITY; event.source=BOT_SOURCE_AUDIO;
        } else if (!strcmp(line,"audio-ready")) {
            event.kind=BOT_EVENT_AUDIO_HEALTH; event.source=BOT_SOURCE_AUDIO;
            event.data.health.available=true;
        } else if (!strcmp(line,"link") || !strcmp(line,"disconnect")) {
            event.kind=BOT_EVENT_LINK_STATE; event.source=BOT_SOURCE_LINK;
            bool connected=!strcmp(line,"link");
            event.data.link.connected=event.data.link.authenticated=event.data.link.encrypted=connected;
        } else if (!strcmp(line,"onset") || !strcmp(line,"end")) {
            event.kind=!strcmp(line,"onset") ? BOT_EVENT_UTTERANCE_STARTED : BOT_EVENT_UTTERANCE_ENDED;
            event.source=BOT_SOURCE_AUDIO;
        } else if (!strcmp(line,"stop")) {
            bus.signals=(bot_bus_signals_t){.local_stop=true,.observed_ms=now};
        } else if (!strcmp(line,"thinking") || !strncmp(line,"reply ",6)) {
            bool thinking=!strcmp(line,"thinking");
            if (thinking) thinking_token=state.token;
            bus.command=(bot_command_t){.kind=thinking ? BOT_COMMAND_THINKING : BOT_COMMAND_REPLY_DONE,
                .received_ms=now,.token=thinking_token};
            if (!thinking) {
                if (strlen(line+6) >= sizeof(bus.command.data.caption)) { status=2; break; }
                strcpy(bus.command.data.caption,line+6);
            }
            bus.has_command=true;
        } else if (!strncmp(line,"tick ",5)) {
            char *end;
            long ms=strtol(line+5,&end,10);
            if (end==line+5 || *end || ms<1 || ms>600000) { status=2; break; }
            now-=20; // Virtual time, with the same nominal owner cadence as the board.
            while (ms>20) { now+=20; bot_runtime_step(runtime,now); ms-=20; }
            now+=(uint32_t)ms;
        } else if (strcmp(line,"state")) {
            status=2; break;
        }
        if (event.source != BOT_SOURCE_PLATFORM) { bus.event=event; bus.has_event=true; }
        bot_runtime_step(runtime,now);
        bot_runtime_get_snapshot(runtime,&state);
        snapshot(&state,now);
    }
    bot_runtime_destroy(runtime);
    if (status) fputs("Invalid simulator input. Run tools/sim-bot --help.\n",stderr);
    return status || ferror(stdin) ? 2 : 0;
}

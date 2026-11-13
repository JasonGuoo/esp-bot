#include "bot_voice.h"
#include <stddef.h>

bot_voice_settings_t bot_voice_default_settings(void)
{
    return (bot_voice_settings_t){ .volume_percent = 30, .muted = false,
        .wake_threshold_percent = 80, .night_gain_percent = 30,
        .spoken_answers = false };
}

bool bot_voice_settings_valid(const bot_voice_settings_t *s)
{
    return s && s->volume_percent <= BOT_VOICE_VOLUME_MAX &&
        s->wake_threshold_percent >= BOT_VOICE_WAKE_THRESHOLD_MIN &&
        s->wake_threshold_percent <= BOT_VOICE_WAKE_THRESHOLD_MAX &&
        s->night_gain_percent >= BOT_VOICE_NIGHT_GAIN_MIN && s->night_gain_percent <= 100;
}

bool bot_voice_settings_equal(const bot_voice_settings_t *a, const bot_voice_settings_t *b)
{
    return a && b && a->volume_percent == b->volume_percent && a->muted == b->muted &&
        a->wake_threshold_percent == b->wake_threshold_percent &&
        a->night_gain_percent == b->night_gain_percent && a->spoken_answers == b->spoken_answers;
}

bool bot_voice_command_valid(bot_voice_command_t command)
{
    return command >= BOT_VOICE_COMMAND_VOLUME_UP && command <= BOT_VOICE_COMMAND_UNMUTE;
}

const char *bot_voice_command_name(bot_voice_command_t command)
{
    switch (command) {
    case BOT_VOICE_COMMAND_VOLUME_UP: return "volume_up";
    case BOT_VOICE_COMMAND_VOLUME_DOWN: return "volume_down";
    case BOT_VOICE_COMMAND_QUIET: return "quiet";
    case BOT_VOICE_COMMAND_STOP: return "stop";
    case BOT_VOICE_COMMAND_SLEEP: return "sleep";
    case BOT_VOICE_COMMAND_THEME_BLUE: return "theme_blue";
    case BOT_VOICE_COMMAND_HAPPY: return "happy";
    case BOT_VOICE_COMMAND_REPEAT: return "repeat";
    case BOT_VOICE_COMMAND_UNMUTE: return "unmute";
    default: return "none";
    }
}

/* Shared, driver-free voice settings and intent types. No behavior authority. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

#define BOT_VOICE_VOLUME_MAX 60
#define BOT_VOICE_WAKE_THRESHOLD_MIN 40
#define BOT_VOICE_WAKE_THRESHOLD_MAX 99
#define BOT_VOICE_NIGHT_GAIN_MIN 10

typedef struct {
    uint8_t volume_percent;          /* ES8389 output level, 0..60; not acoustic SPL */
    bool muted;                     /* remembers volume while output is muted */
    uint8_t wake_threshold_percent;  /* native model threshold, not a probability */
    uint8_t night_gain_percent;      /* relative reply PCM gain during known night, 10..100 */
    bool spoken_answers;            /* opt-in Mac TTS; text/image remains enabled */
} bot_voice_settings_t;

/* IDs are separate from whole-keyword wake command 1 in the MultiNet graph. */
typedef enum {
    BOT_VOICE_COMMAND_NONE = 0,
    BOT_VOICE_COMMAND_VOLUME_UP = 10,
    BOT_VOICE_COMMAND_VOLUME_DOWN,
    BOT_VOICE_COMMAND_QUIET,
    BOT_VOICE_COMMAND_STOP,
    BOT_VOICE_COMMAND_SLEEP,
    BOT_VOICE_COMMAND_THEME_BLUE,
    BOT_VOICE_COMMAND_HAPPY,
    BOT_VOICE_COMMAND_REPEAT,
    BOT_VOICE_COMMAND_UNMUTE,
} bot_voice_command_t;

/* Small, metadata-only snapshot for the sound settings page. Never audio data. */
typedef struct {
    bool mic_available, model_ready, capture_ready, link_ready, playback_blocked;
    bool settings_applied, wake_score_available;
    uint16_t peak0, peak1;
    float last_wake_score;
    uint32_t capture_frames, read_errors, playback_underruns;
    uint32_t buffered_samples, buffer_high_water, buffered_drops;
    uint32_t wake_count, local_command_count, last_capture_ready_ms;
    int32_t last_error;
} bot_voice_diagnostics_t;

bot_voice_settings_t bot_voice_default_settings(void);
bool bot_voice_settings_valid(const bot_voice_settings_t *settings);
bool bot_voice_settings_equal(const bot_voice_settings_t *a, const bot_voice_settings_t *b);
bool bot_voice_command_valid(bot_voice_command_t command);
const char *bot_voice_command_name(bot_voice_command_t command);
#ifdef __cplusplus
}
#endif

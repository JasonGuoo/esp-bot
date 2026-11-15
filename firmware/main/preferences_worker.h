#pragma once
#include "bot_platform.h"

/* Full desired snapshot: overwriting a pending request never loses a change
 * to another preference. Revisions are application-owned and zero means idle. */
typedef struct {
    uint32_t voice_revision, theme_revision, orientation_revision;
    bot_voice_settings_t voice;
    char theme[32];
    uint8_t orientation;
} bot_preferences_request_t;

typedef struct {
    uint32_t voice_revision, theme_revision, orientation_revision;
    esp_err_t voice_error, theme_error, orientation_error;
} bot_preferences_result_t;

esp_err_t bot_preferences_start(bot_platform_t *platform);
bool bot_preferences_submit(const bot_preferences_request_t *request);
bool bot_preferences_take_result(bot_preferences_result_t *result);

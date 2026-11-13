/* Private sound-page renderer. Hardware and behavior stay with the app owner. */
#pragma once
#include <stdbool.h>
#include "bot_voice.h"
#include "lvgl.h"

typedef struct face_sound_settings face_sound_settings_t;
face_sound_settings_t *face_sound_settings_create(lv_obj_t *parent,
                                                 void (*on_back)(void *), void *context);
void face_sound_settings_destroy(face_sound_settings_t *settings);
lv_obj_t *face_sound_settings_root(face_sound_settings_t *settings);
void face_sound_settings_open(face_sound_settings_t *settings);
void face_sound_settings_layout(face_sound_settings_t *settings, int width, int height);
void face_sound_settings_set_palette(face_sound_settings_t *settings,
                                     lv_color_t background, lv_color_t tile,
                                     lv_color_t text, lv_color_t accent);
void face_sound_settings_on_request(face_sound_settings_t *settings,
    bool (*callback)(const bot_voice_settings_t *, void *), void *context);
void face_sound_settings_on_preview(face_sound_settings_t *settings,
    bool (*callback)(void *), void *context);
void face_sound_settings_set_value(face_sound_settings_t *settings,
                                   const bot_voice_settings_t *value);
void face_sound_settings_set_diagnostics(face_sound_settings_t *settings,
                                         const bot_voice_diagnostics_t *value);
void face_sound_settings_set_notice(face_sound_settings_t *settings, const char *notice);

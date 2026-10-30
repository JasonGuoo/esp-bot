/* Private to the face component: full-screen settings navigation. */
#pragma once
#include <stdbool.h>
#include "lvgl.h"

typedef struct face_settings face_settings_t;
typedef void (*face_settings_apply_cb_t)(const char *theme_id, void *context);

face_settings_t *face_settings_create(lv_obj_t *parent,
                                      face_settings_apply_cb_t on_apply,
                                      void *context);
void face_settings_destroy(face_settings_t *settings);
void face_settings_open(face_settings_t *settings, const char *active_theme_id);
void face_settings_close(face_settings_t *settings);
bool face_settings_is_open(const face_settings_t *settings);
void face_settings_set_active_theme(face_settings_t *settings,
                                    const char *active_theme_id);
void face_settings_layout(face_settings_t *settings, int width, int height);

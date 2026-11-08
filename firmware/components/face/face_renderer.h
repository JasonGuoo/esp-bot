/*
 * Renderer plug-in interface. Private implementation detail of the Face module.
 * Business state and theme metadata live elsewhere; drawing lives in drivers.
 *
 * To introduce another eye aesthetic:
 * 1. implement a face_renderer_driver_t in its own source file;
 * 2. register the new driver in face_renderer.c;
 * 3. add a theme manifest entry whose renderer matches the new driver ID.
 * face.c, Brain, and settings navigation DO NOT need renderer-specific changes.
 */
#pragma once

#include <stdbool.h>
#include "lvgl.h"
#include "face_themes_generated.h"
#include "face_expressions_generated.h"

typedef struct {
    face_expression_t expression;
    int gaze_x;
    int gaze_y;
    bool blinking;
    bool transition_active;
    uint16_t transition_progress; /* 0..256, eased by LVGL */
    uint16_t blink_progress;      /* 0..256, eased by LVGL */
} face_render_state_t;

typedef struct face_renderer face_renderer_t;

typedef struct {
    const char *id;
    void *(*create)(lv_obj_t *parent);
    void (*render)(void *instance, const face_theme_t *theme,
                   const face_render_state_t *state, int width, int height);
    void (*destroy)(void *instance);
} face_renderer_driver_t;

/* Select by a string from the manifest, never by a conditional in face.c. */
face_renderer_t *face_renderer_create(const char *renderer_id, lv_obj_t *parent);
void face_renderer_render(face_renderer_t *instance, const face_theme_t *theme,
                          const face_render_state_t *state, int width, int height);
void face_renderer_destroy(face_renderer_t *instance);
const char *face_renderer_id(const face_renderer_t *instance);
bool face_renderer_supported(const char *renderer_id);
/* Older packs use the nearest established pose for the extended vocabulary. */
face_expression_t face_renderer_legacy_expression(face_expression_t expression);

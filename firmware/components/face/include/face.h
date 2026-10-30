/**
 * ESP Bot reusable face component, LVGL 9.
 *
 * No Wi-Fi, microphone, LLM, camera or board dependency. Call on the LVGL task,
 * or while holding the BSP's display lock from another FreeRTOS task.
 */
#pragma once
#include <stdbool.h>
#include "lvgl.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct face_view face_view_t;

/** Create a full-screen face as a child of parent. Caller owns the returned view. */
face_view_t *face_create(lv_obj_t *parent);

/** Delete its LVGL timer, screen children, and heap allocations. */
void face_destroy(face_view_t *view);

/** Theme IDs: mint, sakura, lavender, butter, sky. False on unknown ID. */
bool face_set_theme(face_view_t *view, const char *theme_id);

/** idle, listening, happy, curious, thinking, surprised, sleepy, shy, blink. */
bool face_set_expression(face_view_t *view, const char *expression);

/** -100..100 gaze offset; values are clamped and eased by the component timer. */
void face_set_gaze(face_view_t *view, int x, int y);

/** Set caption text or NULL/"" to hide; truncates at 383 UTF-8 bytes. */
void face_set_subtitle(face_view_t *view, const char *text);

/** Reveal small top-right settings control for a few seconds. */
void face_reveal_settings(face_view_t *view);

/** Current identifiers for diagnostics and other modules; owned by component. */
const char *face_theme_id(const face_view_t *view);
const char *face_expression_id(const face_view_t *view);

/** Root LVGL object, for screen layout or integration. */
lv_obj_t *face_root(face_view_t *view);

#ifdef __cplusplus
}
#endif

/**
 * ESP Bot reusable face component, LVGL 9.
 *
 * Independent from Wi-Fi, microphone, LLM, camera and board drivers. Call on the LVGL task,
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

/** idle, listening, happy, curious, thinking, surprised, sleepy, shy, blink,
 * bored, drowsy, sleeping, speaking. */
bool face_set_expression(face_view_t *view, const char *expression);

/** -100..100 gaze offset; values are clamped and eased by the component timer. */
void face_set_gaze(face_view_t *view, int x, int y);

/** Set caption text or NULL/"" to hide; truncates at 383 UTF-8 bytes. */
void face_set_subtitle(face_view_t *view, const char *text);

/** Reveal the top-right gear for five seconds; opens Settings -> Appearance. */
void face_reveal_settings(face_view_t *view);

typedef enum {
    FACE_EVENT_TOUCH, FACE_EVENT_STOP,
    FACE_EVENT_SETTINGS_OPENED, FACE_EVENT_SETTINGS_CLOSED
} face_event_t;
/** UI events are intent only; runtime owns behavior. Callback runs under the
 * LVGL lock and must only enqueue work. No driver/network calls in callback. */
typedef void (*face_event_cb_t)(face_event_t event, void *context);
void face_on_event(face_view_t *view, face_event_cb_t callback, void *context);
void face_set_stop_visible(face_view_t *view, bool visible);
/** Pause only the rendering animation timer, e.g. behind logical blackout.
 * Does not disable board touch or decide when to sleep. */
void face_set_animation_active(face_view_t *view, bool active);

/** Invoked only when a person taps Apply in Appearance.
 *  Persistence belongs to the application, NOT to the reusable Face component.
 *  Programmatic face_set_theme() does not persist automatically.
 */
typedef void (*face_theme_applied_cb_t)(const char *theme_id, void *context);
void face_on_theme_applied(face_view_t *view,
                           face_theme_applied_cb_t callback, void *context);

/** Current identifiers for diagnostics and other modules; owned by component. */
const char *face_theme_id(const face_view_t *view);
const char *face_expression_id(const face_view_t *view);

/** Root LVGL object, for screen layout or integration. */
lv_obj_t *face_root(face_view_t *view);

#ifdef __cplusplus
}
#endif

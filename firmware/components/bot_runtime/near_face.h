#pragma once
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include "vision_attention.h"

/* The face box is already normalized to the camera image. This is a proximity
 * cue, not evidence that the person is looking at the robot. */
typedef struct {
    bool active;
    bool has_capture;
    uint32_t last_close_ms, last_capture_ms;
} bot_near_face_t;

static inline bool bot_near_face_fresh(uint32_t now, uint32_t then, uint32_t max_age)
{
    const int32_t age = (int32_t)(now - then);
    return age >= 0 && (uint32_t)age <= max_age;
}

static inline float bot_near_face_largest_area(const vatt_snapshot_t *frame,
                                               uint32_t now, uint32_t max_age,
                                               float min_confidence)
{
    if (!frame || frame->state == VATT_UNAVAILABLE || frame->face_count > VATT_MAX_FACES ||
        !bot_near_face_fresh(now, frame->timestamp_ms, max_age)) return 0.0f;
    float largest = 0.0f;
    for (uint8_t i = 0; i < frame->face_count; ++i) {
        const vatt_face_t *face = &frame->faces[i];
        const vatt_observation_t *o = &face->observation;
        const vatt_box_t *b = &o->bbox;
        if (!face->visible || !face->track_id ||
            !bot_near_face_fresh(now, face->last_seen_ms, max_age) ||
            (int32_t)(frame->timestamp_ms - face->last_seen_ms) < 0 ||
            !isfinite(o->face_confidence) || o->face_confidence < min_confidence ||
            o->face_confidence > 1.0f || !isfinite(b->left) || !isfinite(b->top) ||
            !isfinite(b->right) || !isfinite(b->bottom) || b->left < 0.0f ||
            b->top < 0.0f || b->right > 1.0f || b->bottom > 1.0f ||
            b->right <= b->left || b->bottom <= b->top) continue;
        const float area = (b->right - b->left) * (b->bottom - b->top);
        if (area > largest) largest = area;
    }
    return largest;
}

/* Enter above 20%. A small exit margin and a 400ms loss grace avoid switching
 * expression on every noisy detection near the boundary. A fresh detection can
 * arrive some time after capture, so start the grace when it is first received,
 * and never extend it by rereading the same frame. Settings, night and critical
 * faults clear the reaction immediately. */
static inline bool bot_near_face_step(bot_near_face_t *reaction,
                                      const vatt_snapshot_t *frame,
                                      uint32_t now, uint32_t max_age,
                                      float min_confidence, bool permitted,
                                      uint16_t *area_permille)
{
    if (!permitted) {
        *reaction = (bot_near_face_t){0};
        *area_permille = 0;
        return false;
    }
    const float area = bot_near_face_largest_area(frame, now, max_age, min_confidence);
    *area_permille = (uint16_t)(area * 1000.0f + 0.5f);
    if (area > 0.20f || (reaction->active && area >= 0.18f)) {
        reaction->active = true;
        if (!reaction->has_capture || (int32_t)(frame->timestamp_ms - reaction->last_capture_ms) > 0) {
            reaction->last_close_ms = now;
            reaction->last_capture_ms = frame->timestamp_ms;
            reaction->has_capture = true;
        }
    } else if (reaction->active && !bot_near_face_fresh(now, reaction->last_close_ms, 400)) {
        reaction->active = false;
    }
    return reaction->active;
}

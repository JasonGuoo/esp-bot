#pragma once
#include <stdbool.h>
#include <stdint.h>
#include <string.h>
#include "face_expressions_generated.h"

/* EVE contours all share the same four-cubic topology. Retaining the current
 * contour makes an interrupted expression transition continue without a jump. */
enum { FACE_EVE_SHAPE_POINTS = 26, FACE_EVE_MIX_FULL = 256 };
typedef struct {
    int16_t start[2][FACE_EVE_SHAPE_POINTS];
    int16_t current[2][FACE_EVE_SHAPE_POINTS];
    face_expression_t expression;
    bool initialized;
} face_eve_pose_t;

static inline int16_t face_eve_mix(int16_t from, int16_t to, uint16_t amount)
{
    if (amount > FACE_EVE_MIX_FULL) amount = FACE_EVE_MIX_FULL;
    return (int16_t)(from + ((int32_t)to - from) * amount / FACE_EVE_MIX_FULL);
}

/* Reflections are reserved for explicitly extraordinary joy. A closing blink
 * fades them with the eye, and ordinary smiles can never inherit them. */
static inline uint16_t face_eve_glint_strength(face_expression_t expression, bool transitioning,
                                                uint16_t progress, uint16_t blink)
{
    if (expression != FACE_EX_OVERJOYED) return 0;
    if (progress > FACE_EVE_MIX_FULL) progress = FACE_EVE_MIX_FULL;
    if (blink > FACE_EVE_MIX_FULL) blink = FACE_EVE_MIX_FULL;
    const uint16_t level = transitioning ? progress : FACE_EVE_MIX_FULL;
    return (uint16_t)(level * (FACE_EVE_MIX_FULL - blink) / FACE_EVE_MIX_FULL);
}

static inline void face_eve_pose_step(face_eve_pose_t *pose, face_expression_t expression,
                                      bool transitioning, uint16_t progress,
                                      uint16_t blink, int16_t out[2][FACE_EVE_SHAPE_POINTS])
{
    if (expression < 0 || expression >= FACE_EX_COUNT) expression = FACE_EX_IDLE;
    if (!pose->initialized) {
        for (int eye = 0; eye < 2; ++eye) {
            const int16_t *target = k_face_eve_shapes[k_face_eve_eyes[expression][eye]];
            memcpy(pose->current[eye], target, sizeof(pose->current[eye]));
            memcpy(pose->start[eye], target, sizeof(pose->start[eye]));
        }
        pose->expression = expression;
        pose->initialized = true;
    } else if (pose->expression != expression) {
        memcpy(pose->start, pose->current, sizeof(pose->start));
        pose->expression = expression;
    }
    const uint16_t mix = transitioning ? progress : FACE_EVE_MIX_FULL;
    for (int eye = 0; eye < 2; ++eye) {
        const int16_t *target = k_face_eve_shapes[k_face_eve_eyes[expression][eye]];
        const int16_t *closed = k_face_eve_shapes[k_face_eve_eyes[FACE_EX_BLINK][eye]];
        for (int i = 0; i < FACE_EVE_SHAPE_POINTS; ++i) {
            const int16_t base = face_eve_mix(pose->start[eye][i], target[i], mix);
            pose->current[eye][i] = base;
            out[eye][i] = face_eve_mix(base, closed[i], blink);
        }
    }
}

#include "eve_pose.h"
#include <assert.h>
#include <string.h>

int main(void)
{
    assert(face_eve_glint_strength(FACE_EX_IDLE, false, 0, 0) == 0);
    assert(face_eve_glint_strength(FACE_EX_HAPPY, false, 0, 0) == 0);
    assert(face_eve_glint_strength(FACE_EX_EXCITED, false, 0, 0) == 0);
    assert(face_eve_glint_strength(FACE_EX_OVERJOYED, false, 0, 0) == 256);
    assert(face_eve_glint_strength(FACE_EX_OVERJOYED, true, 128, 0) == 128);
    assert(face_eve_glint_strength(FACE_EX_OVERJOYED, false, 0, 256) == 0);
    face_eve_pose_t pose = {0};
    int16_t out[2][FACE_EVE_SHAPE_POINTS], midway[2][FACE_EVE_SHAPE_POINTS];
    face_eve_pose_step(&pose, FACE_EX_IDLE, false, 0, 0, out);
    assert(memcmp(out[0], k_face_eve_shapes[k_face_eve_eyes[FACE_EX_IDLE][0]],
                  sizeof(out[0])) == 0);
    face_eve_pose_step(&pose, FACE_EX_HAPPY, true, 0, 0, out);
    assert(memcmp(out[0], k_face_eve_shapes[k_face_eve_eyes[FACE_EX_IDLE][0]],
                  sizeof(out[0])) == 0);
    face_eve_pose_step(&pose, FACE_EX_HAPPY, true, 128, 0, midway);
    assert(memcmp(midway[0], out[0], sizeof(out[0])) != 0);
    /* A new target starts from the current contour, not the previous target. */
    face_eve_pose_step(&pose, FACE_EX_CURIOUS, true, 0, 0, out);
    assert(memcmp(out, midway, sizeof(out)) == 0);
    face_eve_pose_step(&pose, FACE_EX_CURIOUS, true, 256, 0, out);
    assert(memcmp(out[1], k_face_eve_shapes[k_face_eve_eyes[FACE_EX_CURIOUS][1]],
                  sizeof(out[1])) == 0);
    face_eve_pose_step(&pose, FACE_EX_CURIOUS, false, 0, 256, out);
    assert(memcmp(out[0], k_face_eve_shapes[k_face_eve_eyes[FACE_EX_BLINK][0]],
                  sizeof(out[0])) == 0);
    face_eve_pose_step(&pose, FACE_EX_CURIOUS, false, 0, 0, out);
    assert(memcmp(out[1], k_face_eve_shapes[k_face_eve_eyes[FACE_EX_CURIOUS][1]],
                  sizeof(out[1])) == 0);
    return 0;
}

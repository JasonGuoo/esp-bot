#pragma once

/* Move toward the requested position without crossing it. Inputs are the
 * face component's clamped -100..100 gaze positions. */
static inline int face_gaze_approach(int current, int target, int smoothing)
{
    const int factor = smoothing < 1 ? 1 : smoothing > 20 ? 20 : smoothing;
    const int delta = target - current;
    if (delta > 0) return current + (delta + factor - 1) / factor;
    if (delta < 0) return current + (delta - factor + 1) / factor;
    return current;
}

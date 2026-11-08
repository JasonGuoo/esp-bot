#pragma once
/* Both EVE eyes follow one gaze vector without changing their proportions. */
static inline int face_eve_clamp_gaze(int x)
{
    return x < -100 ? -100 : x > 100 ? 100 : x;
}

static inline int face_eve_gaze_offset_x(int display_width, int x)
{
    return face_eve_clamp_gaze(x) * display_width * 115 / 100000;
}


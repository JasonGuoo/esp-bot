#include "eve_gaze.h"
#include <assert.h>

int main(void)
{
    const int screen = 800, base = 232;
    assert(face_eve_gaze_offset_x(screen, 0) == 0);
    assert(face_eve_gaze_offset_x(screen, -100) == -92);
    assert(face_eve_gaze_offset_x(screen, 100) == 92);
    assert(face_eve_gaze_offset_x(screen, 900) == 92);
    for (int x = -100; x <= 100; ++x) {
        for (int eye = 0; eye < 2; ++eye) {
            const int center = screen * (eye ? 71 : 29) / 100 + face_eve_gaze_offset_x(screen, x);
            /* Includes 4% oversized control points of the discovery pose. */
            assert(center - base * 54 / 100 >= 8);
            assert(center + base * 54 / 100 <= screen - 8);
        }
    }
    return 0;
}

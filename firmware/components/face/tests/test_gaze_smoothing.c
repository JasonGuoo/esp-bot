#include "gaze_smoothing.h"
#include <assert.h>

int main(void)
{
    const int factors[] = {0, 1, 5, 20, 25};
    for (int current = -100; current <= 100; ++current) {
        for (int target = -100; target <= 100; ++target) {
            for (unsigned f = 0; f < sizeof(factors) / sizeof(factors[0]); ++f) {
                int position = current;
                for (int i = 0; i < 201 && position != target; ++i) {
                    const int next = face_gaze_approach(position, target, factors[f]);
                    if (position < target) assert(next > position && next <= target);
                    else assert(next < position && next >= target);
                    position = next;
                }
                assert(position == target);
            }
        }
    }
    return 0;
}

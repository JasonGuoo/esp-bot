#include "idle_attention.h"
#include <stddef.h>

static uint32_t next_random(idle_attention_t *idle) {
    uint32_t v = idle->random;
    v ^= v << 13;
    v ^= v >> 17;
    v ^= v << 5;
    return idle->random = v;
}

void idle_attention_init(idle_attention_t *idle, uint32_t seed) {
    if (!idle) return;
    *idle = (idle_attention_t){ .random = seed ? seed : 0x9e3779b9U };
}

bool idle_attention_step(idle_attention_t *idle, uint32_t now_ms, bool enabled,
                         int *x, int *y) {
    if (!idle || !x || !y) return false;
    if (!enabled) {
        idle->active = false;
        return false;
    }
    bool changed = false;
    if (!idle->active || (int32_t)(now_ms - idle->next_change_ms) >= 0) {
        static const int magnitude[] = {70, 85, 100};
        static const int vertical[] = {-30, -15, 0, 0, 15, 30};
        const int distance = magnitude[next_random(idle) % 3];
        int next_x = (next_random(idle) & 1) ? distance : -distance;
        /* Usually look to the opposite side; sometimes pause at the centre.
         * This gives a readable look around without a metronomic sweep. */
        if (idle->active && idle->x != 0) {
            if (next_random(idle) % 5 == 0) next_x = 0;
            else next_x = idle->x > 0 ? -distance : distance;
        }
        idle->x = next_x;
        idle->y = vertical[next_random(idle) % 6];
        idle->next_change_ms = now_ms + 2600 + next_random(idle) % 3401;
        idle->active = true;
        changed = true;
    }
    *x = idle->x;
    *y = idle->y;
    return changed;
}

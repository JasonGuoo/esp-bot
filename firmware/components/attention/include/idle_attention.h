/* Independent screen-only idle eye targets; no inference or hardware motion. */
#pragma once
#include <stdbool.h>
#include <stdint.h>

typedef struct {
    uint32_t random, next_change_ms;
    int x, y;
    bool active;
} idle_attention_t;

void idle_attention_init(idle_attention_t *idle, uint32_t seed);
/* Returns true when a new target was chosen. Output is meaningful only while
 * enabled; disabling discards the old target so a later idle starts fresh. */
bool idle_attention_step(idle_attention_t *idle, uint32_t now_ms, bool enabled,
                         int *x, int *y);

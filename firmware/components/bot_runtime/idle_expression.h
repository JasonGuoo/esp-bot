#pragma once
#include <stddef.h>
#include <stdbool.h>
#include <stdint.h>

/* A quiet, local-only expression repertoire. Runtime owns when this is allowed;
 * neither a face nor an idle expression grants a conversation permission. */
typedef struct {
    uint32_t random, next_change_ms;
    uint8_t current, previous;
    bool active, presenting;
} bot_idle_expression_t;

static inline uint32_t bot_idle_expression_random(bot_idle_expression_t *idle)
{
    uint32_t x = idle->random;
    x ^= x << 13; x ^= x >> 17; x ^= x << 5;
    return idle->random = x;
}

static inline void bot_idle_expression_init(bot_idle_expression_t *idle, uint32_t seed)
{
    *idle = (bot_idle_expression_t){.random = seed ? seed : 0x9e3779b9U,
                                    .previous = UINT8_MAX};
}

/* NULL means inactive. A normal pause separates the short expressive poses;
 * don't restart the timer every time the owner reapplies the same expression. */
static inline const char *bot_idle_expression_step(bot_idle_expression_t *idle,
                                                    uint32_t now_ms, bool enabled)
{
    static const char *const poses[] = {
        "happy", "laughing", "gentle", "excited", "relieved"
    };
    const uint8_t count = (uint8_t)(sizeof(poses) / sizeof(poses[0]));
    if (!enabled) {
        idle->active = false;
        idle->presenting = false;
        return NULL;
    }
    if (!idle->active) {
        idle->active = true;
        idle->presenting = false;
        idle->next_change_ms = now_ms + 1800 + bot_idle_expression_random(idle) % 1501;
        return "idle";
    }
    if ((int32_t)(now_ms - idle->next_change_ms) >= 0) {
        if (idle->presenting) {
            idle->presenting = false;
            idle->next_change_ms = now_ms + 3500 + bot_idle_expression_random(idle) % 3501;
        } else {
            uint8_t next = (uint8_t)(bot_idle_expression_random(idle) % count);
            if (next == idle->previous) next = (uint8_t)((next + 1) % count);
            idle->current = idle->previous = next;
            idle->presenting = true;
            idle->next_change_ms = now_ms + 1400 + bot_idle_expression_random(idle) % 1001;
        }
    }
    return idle->presenting ? poses[idle->current] : "idle";
}

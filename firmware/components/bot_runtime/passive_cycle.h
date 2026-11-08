#pragma once
#include <stdbool.h>
#include <stdint.h>

/* Monotonic, owner-task-only daytime cycle. Night is supplied separately by
 * the clock policy; an unknown clock never invents a night. */
typedef struct {
    uint32_t phase_started_ms;
    bool drowsy;
    bool night;
} bot_passive_cycle_t;

typedef enum { BOT_PASSIVE_AWAKE, BOT_PASSIVE_DROWSY, BOT_PASSIVE_NIGHT } bot_passive_phase_t;

static inline void bot_passive_cycle_reset(bot_passive_cycle_t *cycle, uint32_t now_ms) {
    cycle->phase_started_ms = now_ms;
    cycle->drowsy = false;
    cycle->night = false;
}

static inline bot_passive_phase_t bot_passive_cycle_step(bot_passive_cycle_t *cycle,
        uint32_t now_ms, bool night, uint32_t awake_ms, uint32_t drowsy_ms) {
    if (night) {
        cycle->night = true;
        cycle->drowsy = false;
        return BOT_PASSIVE_NIGHT;
    }
    if (cycle->night) bot_passive_cycle_reset(cycle, now_ms);
    const uint32_t duration = cycle->drowsy ? drowsy_ms : awake_ms;
    if ((uint32_t)(now_ms - cycle->phase_started_ms) >= duration) {
        cycle->drowsy = !cycle->drowsy;
        cycle->phase_started_ms = now_ms;
    }
    return cycle->drowsy ? BOT_PASSIVE_DROWSY : BOT_PASSIVE_AWAKE;
}

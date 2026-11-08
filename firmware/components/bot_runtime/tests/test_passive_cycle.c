#include "passive_cycle.h"
#include "night_schedule.h"
#include <assert.h>
#include <stdint.h>

int main(void) {
    bot_passive_cycle_t cycle;
    bot_passive_cycle_reset(&cycle, 100);
    assert(bot_passive_cycle_step(&cycle, 1099, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(bot_passive_cycle_step(&cycle, 1100, false, 1000, 200) == BOT_PASSIVE_DROWSY);
    assert(bot_passive_cycle_step(&cycle, 1299, false, 1000, 200) == BOT_PASSIVE_DROWSY);
    assert(bot_passive_cycle_step(&cycle, 1300, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(bot_passive_cycle_step(&cycle, 1400, true, 1000, 200) == BOT_PASSIVE_NIGHT);
    assert(bot_passive_cycle_step(&cycle, 9000, true, 1000, 200) == BOT_PASSIVE_NIGHT);
    assert(bot_passive_cycle_step(&cycle, 9100, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(bot_passive_cycle_step(&cycle, 10099, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(bot_passive_cycle_step(&cycle, 10100, false, 1000, 200) == BOT_PASSIVE_DROWSY);
    bot_passive_cycle_reset(&cycle, UINT32_MAX - 499);
    assert(bot_passive_cycle_step(&cycle, 499, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(bot_passive_cycle_step(&cycle, 500, false, 1000, 200) == BOT_PASSIVE_DROWSY);
    assert(bot_passive_cycle_step(&cycle, 700, false, 1000, 200) == BOT_PASSIVE_AWAKE);
    assert(!bot_night_contains(1349, 1350, 420));
    assert(bot_night_contains(1350, 1350, 420));
    assert(bot_night_contains(0, 1350, 420));
    assert(bot_night_contains(419, 1350, 420));
    assert(!bot_night_contains(420, 1350, 420));
    assert(!bot_night_contains(600, 600, 600));
    assert(bot_night_contains(600, 500, 700));
    assert(!bot_night_contains(700, 500, 700));
    return 0;
}

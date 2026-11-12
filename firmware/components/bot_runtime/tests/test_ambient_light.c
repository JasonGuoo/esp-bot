#include "ambient_light.h"
#include <assert.h>
#include <stdint.h>

int main(void)
{
    bot_ambient_light_t light = {0};
    /* Time wraps while continuously dark: elapsed time remains monotonic. */
    uint32_t start = UINT32_MAX - 15000;
    for (uint32_t elapsed = 0; elapsed <= 30000; elapsed += 1000) {
        bot_ambient_observe(&light, 20, start + elapsed);
        bot_ambient_step(&light, start + elapsed, true);
    }
    assert(light.dark && light.probe_scheduled && !light.probing);
    bot_ambient_step(&light, start + 60000, true);
    assert(light.probing && !light.valid);
    bot_ambient_observe(&light, 80, start + 61000);
    bot_ambient_observe(&light, 80, start + 62000);
    /* A missing camera must not turn a partial bright observation into wake. */
    bot_ambient_step(&light, start + 67001, true);
    assert(light.dark && !light.valid);
    bot_ambient_step(&light, start + 70000, true);
    assert(light.dark && !light.probing);
    bot_ambient_step(&light, start + 100000, true);
    assert(light.probing);
    for (uint32_t elapsed = 101000; elapsed <= 104000; elapsed += 1000)
        bot_ambient_observe(&light, 80, start + elapsed);
    assert(!light.dark);
    bot_ambient_step(&light, start + 104000, true);
    assert(!light.probing && !light.probe_scheduled);
    return 0;
}

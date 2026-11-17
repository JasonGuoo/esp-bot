#include "idle_expression.h"
#include "../../face/include/face_expressions_generated.h"
#include <assert.h>
#include <string.h>

int main(void)
{
    bot_idle_expression_t idle;
    bot_idle_expression_init(&idle, 1);
    assert(bot_idle_expression_step(&idle, 0, false) == NULL);
    assert(strcmp(bot_idle_expression_step(&idle, 100, true), "idle") == 0);
    assert(idle.next_change_ms >= 1900 && idle.next_change_ms <= 3400);
    uint32_t next = idle.next_change_ms;
    assert(strcmp(bot_idle_expression_step(&idle, next - 1, true), "idle") == 0);
    const char *pose = bot_idle_expression_step(&idle, next, true);
    assert(strcmp(pose, "idle") != 0 && idle.presenting);
    assert(idle.next_change_ms - next >= 1400 && idle.next_change_ms - next <= 2400);
    assert(strcmp(bot_idle_expression_step(&idle, idle.next_change_ms - 1, true), pose) == 0);
    const uint8_t last = idle.current;
    next = idle.next_change_ms;
    assert(strcmp(bot_idle_expression_step(&idle, next, true), "idle") == 0);
    next = idle.next_change_ms;
    assert(strcmp(bot_idle_expression_step(&idle, next, true), "idle") != 0);
    assert(idle.current != last);
    assert(bot_idle_expression_step(&idle, next + 1, false) == NULL);
    assert(strcmp(bot_idle_expression_step(&idle, next + 2, true), "idle") == 0);
    bot_idle_expression_init(&idle, 2);
    assert(strcmp(bot_idle_expression_step(&idle, UINT32_MAX - 100, true), "idle") == 0);
    assert(strcmp(bot_idle_expression_step(&idle, 0, true), "idle") == 0);
    assert(strcmp(bot_idle_expression_step(&idle, idle.next_change_ms, true), "idle") != 0);
    /* Repeated cycles have varied poses separated by a quiet neutral phase. */
    unsigned seen = 0;
    for (unsigned i = 0; i < 80; ++i) {
        if (idle.presenting) {
            const char *current = bot_idle_expression_step(&idle, idle.next_change_ms - 1, true);
            size_t expression = 0;
            while (expression < FACE_EX_COUNT && strcmp(current, k_face_expression_ids[expression]) != 0)
                ++expression;
            assert(expression < FACE_EX_COUNT);
            assert(k_face_eve_eyes[expression][0] == k_face_eve_eyes[expression][1]);
            seen |= 1U << idle.current;
        }
        bot_idle_expression_step(&idle, idle.next_change_ms, true);
    }
    assert((seen & (seen - 1)) != 0 && seen != 0); /* at least two expressions */
    return 0;
}

#include "idle_attention.h"
#include <assert.h>
#include <stdint.h>

int main(void) {
    idle_attention_t idle;
    int x = 0, y = 0;
    idle_attention_init(&idle, 1);
    assert(idle_attention_step(&idle, 100, true, &x, &y));
    assert(x >= -100 && x <= 100 && (x <= -70 || x >= 70) && y >= -30 && y <= 30);
    const int first_x = x;
    const uint32_t next = idle.next_change_ms;
    assert(next >= 2700 && next <= 6100);
    assert(!idle_attention_step(&idle, next - 1, true, &x, &y));
    assert(x == first_x);
    assert(idle_attention_step(&idle, next, true, &x, &y));
    assert(x != first_x);
    assert(x == 0 || (first_x < 0 && x > 0) || (first_x > 0 && x < 0));
    assert(!idle_attention_step(&idle, next + 1, false, &x, &y));
    assert(!idle.active);
    assert(idle_attention_step(&idle, UINT32_MAX - 100, true, &x, &y));
    assert(!idle_attention_step(&idle, 0, true, &x, &y));
    assert(idle_attention_step(&idle, idle.next_change_ms, true, &x, &y));
    return 0;
}

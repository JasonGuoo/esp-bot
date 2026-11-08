#include "rgb565_frame.h"
#include <assert.h>
#include <string.h>

int main(void)
{
    /* Padded rows ensure the scaler cannot silently use width as stride. */
    uint8_t src[4 * 12];
    memset(src, 0xee, sizeof(src));
    for (size_t y = 0; y < 4; ++y) {
        for (size_t x = 0; x < 4; ++x) {
            src[y * 12 + x * 2] = (uint8_t)(y * 10 + x);
            src[y * 12 + x * 2 + 1] = 0xa5;
        }
    }
    uint8_t out[10]; memset(out, 0xcd, sizeof(out));
    assert(bot_platform_scale_rgb565(src, sizeof(src), 4, 4, 12, out, 8, 2, 2));
    const uint8_t expected[] = {0,0xa5,2,0xa5,20,0xa5,22,0xa5};
    assert(memcmp(out, expected, sizeof(expected)) == 0);
    assert(out[8] == 0xcd && out[9] == 0xcd);

    uint8_t tall[480 * 2], half[240 * 2];
    for (size_t y = 0; y < 480; ++y) { tall[y * 2] = y & 255; tall[y * 2 + 1] = y >> 8; }
    assert(bot_platform_scale_rgb565(tall, sizeof(tall), 1, 480, 2, half, sizeof(half), 1, 240));
    assert(half[0] == 0 && half[1] == 0);
    assert(half[478] == (478 & 255) && half[479] == (478 >> 8));
    memset(out, 0xcd, sizeof(out));
    assert(!bot_platform_scale_rgb565(src, sizeof(src) - 1, 4, 4, 12, out, 8, 2, 2));
    assert(!bot_platform_scale_rgb565(src, sizeof(src), 4, 4, 12, out, 7, 2, 2));
    for (size_t i = 0; i < sizeof(out); ++i) assert(out[i] == 0xcd);
}

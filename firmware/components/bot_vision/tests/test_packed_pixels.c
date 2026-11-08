#include "packed_pixels.h"
#include <assert.h>
#include <stdio.h>
#include <string.h>

int main(void)
{
    const uint8_t uyvy[] = {30, 10, 40, 20, 90, 50, 100, 60};
    const uint8_t expected_uyvy[] = {90, 60, 100, 50, 30, 20, 40, 10};
    const uint8_t yuyv[] = {10, 30, 20, 40, 50, 90, 60, 100};
    const uint8_t expected_yuyv[] = {60, 90, 50, 100, 20, 30, 10, 40};
    uint8_t result[8], restored[8];
    bot_vision_reverse_yuv422_row(uyvy, result, 4, true);
    assert(memcmp(result, expected_uyvy, sizeof(result)) == 0);
    bot_vision_reverse_yuv422_row(result, restored, 4, true);
    assert(memcmp(restored, uyvy, sizeof(restored)) == 0);
    bot_vision_reverse_yuv422_row(yuyv, result, 4, false);
    assert(memcmp(result, expected_yuyv, sizeof(result)) == 0);
    bot_vision_reverse_yuv422_row(result, restored, 4, false);
    assert(memcmp(restored, yuyv, sizeof(restored)) == 0);
    bot_vision_reverse_yuv422_row(uyvy, result, 2, true);
    assert(memcmp(result, (uint8_t[]){30, 20, 40, 10}, 4) == 0);
    puts("packed YUV422: chroma preservation and reversal tests passed");
}

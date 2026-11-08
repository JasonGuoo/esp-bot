#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Packed 4:2:2 shares U/V between two pixels. Reverse pairs and their luma,
 * preserving chroma order. Source/destination must not overlap; width is even. */
static inline void bot_vision_reverse_yuv422_row(const uint8_t *source, uint8_t *destination,
                                               size_t width, bool uyvy)
{
    for (size_t x = 0; x < width; x += 2) {
        const uint8_t *s = source + x * 2;
        uint8_t *d = destination + (width - 2 - x) * 2;
        if (uyvy) {
            d[0] = s[0]; d[1] = s[3]; d[2] = s[2]; d[3] = s[1];
        } else {
            d[0] = s[2]; d[1] = s[1]; d[2] = s[0]; d[3] = s[3];
        }
    }
}

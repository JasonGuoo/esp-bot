#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Copy nearest-neighbor RGB565 pixels from a full frame. Source and output
 * must not overlap; padded source strides are supported. */
static inline bool bot_platform_scale_rgb565(const uint8_t *src, size_t src_size,
                                              size_t src_w, size_t src_h, size_t src_stride,
                                              uint8_t *dst, size_t dst_size,
                                              size_t dst_w, size_t dst_h)
{
    if (!src || !dst || !src_w || !src_h || !dst_w || !dst_h ||
        src_w > SIZE_MAX / 2 || src_stride < src_w * 2 ||
        src_h > SIZE_MAX / src_stride || src_size < src_stride * src_h ||
        dst_w > SIZE_MAX / 2 || dst_h > SIZE_MAX / (dst_w * 2) ||
        dst_size < dst_w * dst_h * 2 ||
        dst_h > SIZE_MAX / src_h || dst_w > SIZE_MAX / src_w) return false;
    for (size_t y = 0; y < dst_h; ++y) {
        const uint8_t *row = src + (y * src_h / dst_h) * src_stride;
        for (size_t x = 0; x < dst_w; ++x) {
            const size_t offset = (x * src_w / dst_w) * 2;
            dst[(y * dst_w + x) * 2] = row[offset];
            dst[(y * dst_w + x) * 2 + 1] = row[offset + 1];
        }
    }
    return true;
}

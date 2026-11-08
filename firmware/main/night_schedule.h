#pragma once
#include <stdbool.h>

/* Half-open local minute interval, including a window across midnight. Equal
 * endpoints mean disabled rather than silently choosing all day or no day. */
static inline bool bot_night_contains(int minute, int start, int end) {
    if (minute < 0 || minute >= 1440 || start < 0 || start >= 1440 || end < 0 || end >= 1440 || start == end)
        return false;
    return start < end ? minute >= start && minute < end : minute >= start || minute < end;
}

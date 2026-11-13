/* SPDX-License-Identifier: MIT */
#include "bot_audio_start_buffer.h"
#include <stdlib.h>
#include <string.h>
#include "esp_heap_caps.h"

static void wipe(void *data, size_t bytes)
{
    volatile unsigned char *p = data;
    while (bytes--) *p++ = 0;
}

esp_err_t bot_audio_start_buffer_open(bot_audio_start_buffer_t *b, size_t frame_samples,
                                      uint32_t maximum_ms)
{
    if (!b || !frame_samples || frame_samples > 4096 || maximum_ms < 300 || maximum_ms > 1500)
        return ESP_ERR_INVALID_ARG;
    memset(b, 0, sizeof(*b));
    b->frame_samples = frame_samples;
    b->capacity = ((size_t)BOT_AUDIO_SAMPLE_RATE * maximum_ms / 1000) / frame_samples;
    if (!b->capacity) return ESP_ERR_INVALID_SIZE;
    b->samples = heap_caps_calloc(b->capacity * frame_samples, sizeof(int16_t),
                                  MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    b->frames = heap_caps_calloc(b->capacity, sizeof(bot_audio_pcm_t), MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!b->samples || !b->frames) {
        bot_audio_start_buffer_close(b);
        return ESP_ERR_NO_MEM;
    }
    return ESP_OK;
}

const bot_audio_pcm_t *bot_audio_start_buffer_peek(const bot_audio_start_buffer_t *b)
{
    return b && b->count ? &b->frames[b->head] : NULL;
}

void bot_audio_start_buffer_pop(bot_audio_start_buffer_t *b)
{
    if (!b || !b->count) return;
    wipe(b->samples + b->head * b->frame_samples, b->frame_samples * sizeof(int16_t));
    wipe(&b->frames[b->head], sizeof(bot_audio_pcm_t));
    b->head = (b->head + 1) % b->capacity;
    b->count--;
}

void bot_audio_start_buffer_clear(bot_audio_start_buffer_t *b)
{
    if (!b) return;
    while (b->count) bot_audio_start_buffer_pop(b);
    b->head = 0;
}

void bot_audio_start_buffer_close(bot_audio_start_buffer_t *b)
{
    if (!b) return;
    if (b->samples) wipe(b->samples, b->capacity * b->frame_samples * sizeof(int16_t));
    if (b->frames) wipe(b->frames, b->capacity * sizeof(bot_audio_pcm_t));
    free(b->samples);
    free(b->frames);
    memset(b, 0, sizeof(*b));
}

size_t bot_audio_start_buffer_push(bot_audio_start_buffer_t *b, const bot_audio_pcm_t *frame,
                                    int64_t protect_from_us, bool *protected_drop)
{
    if (protected_drop) *protected_drop = false;
    if (!b || !b->capacity || !frame || !frame->samples || frame->sample_count != b->frame_samples)
        return 0;
    size_t evicted = 0;
    if (b->count == b->capacity) {
        const bot_audio_pcm_t *old = bot_audio_start_buffer_peek(b);
        evicted = old->sample_count;
        if (protected_drop && protect_from_us && old->observed_us >= protect_from_us)
            *protected_drop = true;
        bot_audio_start_buffer_pop(b);
    }
    size_t index = (b->head + b->count) % b->capacity;
    int16_t *samples = b->samples + index * b->frame_samples;
    memcpy(samples, frame->samples, frame->sample_count * sizeof(int16_t));
    b->frames[index] = *frame;
    b->frames[index].samples = samples;
    b->count++;
    return evicted;
}

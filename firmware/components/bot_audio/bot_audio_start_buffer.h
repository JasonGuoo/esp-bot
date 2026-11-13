/* SPDX-License-Identifier: MIT */
#pragma once
#include "bot_audio.h"

/* Capture-worker-only ring. A caller must first establish an explicit runtime
 * lease; this helper never decides authorization or changes a frame's token. */
typedef struct {
    int16_t *samples;
    bot_audio_pcm_t *frames;
    size_t frame_samples, capacity, head, count;
} bot_audio_start_buffer_t;

esp_err_t bot_audio_start_buffer_open(bot_audio_start_buffer_t *buffer, size_t frame_samples,
                                      uint32_t maximum_ms);
void bot_audio_start_buffer_clear(bot_audio_start_buffer_t *buffer);
void bot_audio_start_buffer_close(bot_audio_start_buffer_t *buffer);
/* Returns the evicted sample count. protected_drop marks possible utterance
 * prefix loss, using the caller's local VAD onset observation, never hardware
 * capture timestamps. The original token and read timestamps are copied. */
size_t bot_audio_start_buffer_push(bot_audio_start_buffer_t *buffer, const bot_audio_pcm_t *frame,
                                    int64_t protect_from_us, bool *protected_drop);
const bot_audio_pcm_t *bot_audio_start_buffer_peek(const bot_audio_start_buffer_t *buffer);
void bot_audio_start_buffer_pop(bot_audio_start_buffer_t *buffer);

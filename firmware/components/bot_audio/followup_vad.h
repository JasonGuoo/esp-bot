/* SPDX-License-Identifier: MIT */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

static inline bool bot_audio_pcm_speech_energy(const int16_t *pcm, size_t count)
{
    if (!pcm || !count || count > 4096) return false;
    uint64_t squares = 0;
    for (size_t i = 0; i < count; ++i) {
        const int32_t sample = pcm[i];
        squares += (uint64_t)((int64_t)sample * sample);
    }
    /* ceil((32768 * 10^(-42/20))^2). Widen before multiplying, including
     * INT16_MIN. Measure the actual raw ADC input, independently of AGC. */
    return squares >= UINT64_C(67749) * count;
}

static inline bool bot_audio_speech_frame(bool vad_speech, bool onset_energy,
                                          bool waiting_for_followup, bool already_speaking)
{
    /* Only a follow-up onset needs raw energy at the Mac ASR's -42 dBFS
     * gate. Once started, retain soft syllables and normal VAD end timing. */
    return vad_speech && (!waiting_for_followup || already_speaking || onset_energy);
}

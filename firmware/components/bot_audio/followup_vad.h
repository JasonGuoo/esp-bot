/* SPDX-License-Identifier: MIT */
#pragma once
#include <stdbool.h>

static inline bool bot_audio_speech_frame(bool vad_speech, float dbfs,
                                          bool waiting_for_followup, bool already_speaking)
{
    /* ESP-SR reports pre-AGC frame energy. The Mac ASR gate treats frames
     * at -42 dBFS or above as active. Check onset only, preserving quiet
     * syllables and the VAD's normal end-of-speech timing. */
    return vad_speech && (!waiting_for_followup || already_speaking || dbfs >= -42.0f);
}

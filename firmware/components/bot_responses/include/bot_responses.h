/* SPDX-License-Identifier: MIT
 * Pure local response selection and streaming decoding. No wake/UI authority.
 */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define BOT_RESPONSES_SAMPLE_RATE 16000U
#define BOT_RESPONSES_ADPCM_BLOCK_BYTES 256U
#define BOT_RESPONSES_RECENT_COUNT 8U

typedef struct {
    uint64_t now_ms;                 /* monotonic; never a wall-clock substitute */
    bool time_valid;
    uint8_t local_hour;              /* 0..23 only when time_valid */
    bool night_dark;                 /* caller's established quiet/night policy */
    bool was_sleeping;               /* state BEFORE the accepted explicit wake */
    bool ambient_sample_valid, ambient_dark;
    bool face_observation_valid, face_visible; /* fresh usable vision only */
    bool brain_status_valid, brain_online;    /* service state, NOT Wi-Fi state */
    bool repeated_wake;
} bot_response_context_t;

typedef struct {
    const char *id;
    const char *text;
    const char *expression;          /* recommendation; runtime applies it */
    const char *scene;
    uint8_t gain_percent;            /* 0..100 of the user's current volume */
    uint16_t caption_ms;             /* suggested maximum caption lifetime */
    uint32_t audio_offset;
    uint32_t audio_bytes;
    uint32_t pcm_samples;            /* exact unpadded 16 kHz mono sample count */
} bot_response_entry_t;

typedef struct {
    uint32_t random_state;
    uint16_t recent[BOT_RESPONSES_RECENT_COUNT];
    uint8_t recent_count, recent_next;
    uint64_t last_selected_ms;
} bot_responses_state_t;

void bot_responses_init(bot_responses_state_t *state, uint32_t seed);
/** Only call AFTER runtime accepts an explicit acoustic wake. Perception, VAD,
 * darkness and clock changes must NEVER invoke this function on their own.
 * Single-owner state; no allocation, IO, locks, state transitions or callbacks.
 * Returns a pointer to immutable flash metadata; NULL for invalid arguments.
 * Unknown clock/vision/service observations cannot select factual replies.
 */
const bot_response_entry_t *bot_responses_select(bot_responses_state_t *state,
                                                const bot_response_context_t *context);
size_t bot_responses_count(void);
const bot_response_entry_t *bot_responses_get(size_t index);

typedef struct {
    const uint8_t *data;
    uint32_t bytes, pcm_samples, emitted;
    uint32_t block_offset;
    uint16_t nibble_offset;
    int32_t predictor;
    int32_t step_index;
    bool block_started, failed;
} bot_response_audio_stream_t;

/** Validates embedded asset bounds and starts a streaming decoder. No heap.
 * It does NOT start playback or acquire the codec. */
bool bot_responses_audio_open(bot_response_audio_stream_t *stream,
                              const bot_response_entry_t *entry);
/** Decode up to capacity samples; 320 matches bot_audio's 20 ms write API.
 * Returns zero at end or after malformed data; check stream->failed to distinguish.
 * Gain is NOT baked in or applied here: playback owner applies entry.gain_percent
 * once, relative to the current user volume, and respects mute/cancellation.
 */
size_t bot_responses_audio_read(bot_response_audio_stream_t *stream,
                                int16_t *samples, size_t capacity);

#ifdef __cplusplus
}
#endif

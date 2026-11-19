/* SPDX-License-Identifier: MIT */
#include "bot_responses.h"
#include <string.h>

#include "generated_catalog.inc"

extern const uint8_t bot_reply_audio_start[] asm("_binary_wake_replies_adpcm_start");
extern const uint8_t bot_reply_audio_end[] asm("_binary_wake_replies_adpcm_end");

static uint32_t next_random(bot_responses_state_t *state) {
    uint32_t value = state->random_state;
    value ^= value << 13;
    value ^= value >> 17;
    value ^= value << 5;
    state->random_state = value;
    return value;
}

void bot_responses_init(bot_responses_state_t *state, uint32_t seed) {
    if (!state) return;
    memset(state, 0, sizeof(*state));
    state->random_state = seed ? seed : UINT32_C(0x6d2b79f5);
}

size_t bot_responses_count(void) {
    return sizeof(k_bot_responses) / sizeof(k_bot_responses[0]);
}

const bot_response_entry_t *bot_responses_get(size_t index) {
    return index < bot_responses_count() ? &k_bot_responses[index] : NULL;
}

static const char *scene_for(const bot_response_context_t *context) {
    const bool clock_valid = context->time_valid && context->local_hour < 24;
    if (context->night_dark ||
        (clock_valid && (context->local_hour >= 22 || context->local_hour < 5))) return "night";
    if (context->repeated_wake) return "repeat";
    if (context->was_sleeping) return "waking";
    if (context->ambient_sample_valid && context->ambient_dark) return "dark";
    if (context->brain_status_valid && !context->brain_online) return "offline";
    if (context->face_observation_valid && !context->face_visible) return "no_face";
    if (clock_valid && context->local_hour < 11) return "morning";
    if (clock_valid && context->local_hour < 17) return "day";
    if (clock_valid) return "evening";
    return "general";
}

static bool recently_used(const bot_responses_state_t *state, uint16_t index, unsigned avoid) {
    const unsigned count = state->recent_count < avoid ? state->recent_count : avoid;
    for (unsigned i = 0; i < count; ++i) {
        const unsigned position = (state->recent_next + BOT_RESPONSES_RECENT_COUNT - 1U - i) %
                                  BOT_RESPONSES_RECENT_COUNT;
        const uint16_t previous = state->recent[position];
        if (previous == index ||
            (previous < bot_responses_count() &&
             strcmp(k_bot_responses[previous].text, k_bot_responses[index].text) == 0)) return true;
    }
    return false;
}

/* These older recorded lines tell the user to speak quietly or claim that the
 * robot will do so. Retire them without changing the matching audio catalogue;
 * night-time gain is still applied to the remaining night/dark responses. */
static bool retired_response(const bot_response_entry_t *entry) {
    return strcmp(entry->id, "night_03") == 0 || strcmp(entry->id, "night_04") == 0 ||
           strcmp(entry->id, "night_10") == 0 || strcmp(entry->id, "night_12") == 0 ||
           strcmp(entry->id, "dark_07") == 0;
}

const bot_response_entry_t *bot_responses_select(bot_responses_state_t *state,
                                                const bot_response_context_t *context) {
    if (!state || !context) return NULL;
    if (state->random_state == 0) state->random_state = UINT32_C(0x6d2b79f5);
    const char *scene = scene_for(context);
    /* Mix neutral acknowledgements into daytime contextual replies, so an
     * unavailable service or unseen face is not announced on every wake.
     * Night/dark replies stay in their quiet pools. */
    if (strcmp(scene, "night") != 0 && strcmp(scene, "dark") != 0 &&
        next_random(state) % 4U == 0) scene = "general";
    size_t selected = SIZE_MAX;
    for (unsigned pass = 0; pass < 3 && selected == SIZE_MAX; ++pass) {
        const unsigned avoid = pass == 0 ? BOT_RESPONSES_RECENT_COUNT : (pass == 1 ? 1U : 0U);
        uint32_t candidates = 0;
        for (size_t i = 0; i < bot_responses_count(); ++i) {
            if (retired_response(&k_bot_responses[i]) || strcmp(k_bot_responses[i].scene, scene) != 0 ||
                recently_used(state, (uint16_t)i, avoid)) continue;
            ++candidates;
            if (next_random(state) % candidates == 0) selected = i;
        }
    }
    if (selected == SIZE_MAX) return NULL;
    state->recent[state->recent_next] = (uint16_t)selected;
    state->recent_next = (uint8_t)((state->recent_next + 1U) % BOT_RESPONSES_RECENT_COUNT);
    if (state->recent_count < BOT_RESPONSES_RECENT_COUNT) ++state->recent_count;
    state->last_selected_ms = context->now_ms;
    return &k_bot_responses[selected];
}

/* Standard Microsoft IMA WAV mono blocks: LE predictor, index, reserved,
 * followed by low-nibble-first ADPCM. Tables describe the codec, not a model. */
static const int16_t k_ima_steps[89] = {
    7,8,9,10,11,12,13,14,16,17,19,21,23,25,28,31,
    34,37,41,45,50,55,60,66,73,80,88,97,107,118,130,143,
    157,173,190,209,230,253,279,307,337,371,408,449,494,544,
    598,658,724,796,876,963,1060,1166,1282,1411,1552,1707,
    1878,2066,2272,2499,2749,3024,3327,3660,4026,4428,
    4871,5358,5894,6484,7132,7845,8630,9493,10442,11487,
    12635,13899,15289,16818,18500,20350,22385,24623,27086,
    29794,32767
};
static const int8_t k_ima_index_delta[16] = {
    -1,-1,-1,-1,2,4,6,8,-1,-1,-1,-1,2,4,6,8
};

bool bot_responses_audio_open(bot_response_audio_stream_t *stream,
                              const bot_response_entry_t *entry) {
    if (!stream) return false;
    memset(stream, 0, sizeof(*stream));
    const size_t total = (size_t)(bot_reply_audio_end - bot_reply_audio_start);
    if (!entry || entry->audio_bytes == 0 || entry->pcm_samples == 0 ||
        entry->audio_offset > total || entry->audio_bytes > total - entry->audio_offset ||
        entry->audio_bytes % BOT_RESPONSES_ADPCM_BLOCK_BYTES != 0 ||
        (uint64_t)entry->pcm_samples >
            ((uint64_t)entry->audio_bytes / BOT_RESPONSES_ADPCM_BLOCK_BYTES) * 505U) {
        stream->failed = true;
        return false;
    }
    stream->data = bot_reply_audio_start + entry->audio_offset;
    stream->bytes = entry->audio_bytes;
    stream->pcm_samples = entry->pcm_samples;
    return true;
}

size_t bot_responses_audio_read(bot_response_audio_stream_t *stream,
                                int16_t *samples, size_t capacity) {
    if (!stream || !samples || !stream->data || stream->failed) return 0;
    size_t count = 0;
    while (count < capacity && stream->emitted < stream->pcm_samples) {
        if (!stream->block_started) {
            if (stream->block_offset > stream->bytes ||
                stream->bytes - stream->block_offset < BOT_RESPONSES_ADPCM_BLOCK_BYTES) {
                stream->failed = true;
                break;
            }
            const uint8_t *block = stream->data + stream->block_offset;
            const uint16_t raw = (uint16_t)block[0] | ((uint16_t)block[1] << 8);
            stream->predictor = raw < 32768 ? (int32_t)raw : (int32_t)raw - 65536;
            stream->step_index = block[2];
            if (stream->step_index > 88 || block[3] != 0) {
                stream->failed = true;
                break;
            }
            stream->nibble_offset = 0;
            stream->block_started = true;
            samples[count++] = (int16_t)stream->predictor;
            ++stream->emitted;
            continue;
        }
        if (stream->nibble_offset >= (BOT_RESPONSES_ADPCM_BLOCK_BYTES - 4U) * 2U) {
            stream->block_offset += BOT_RESPONSES_ADPCM_BLOCK_BYTES;
            stream->block_started = false;
            continue;
        }
        const uint8_t packed = stream->data[stream->block_offset + 4U + stream->nibble_offset / 2U];
        const uint8_t code = (stream->nibble_offset & 1U) ? packed >> 4 : packed & 15U;
        ++stream->nibble_offset;
        const int32_t step = k_ima_steps[stream->step_index];
        /* WAV's IMA expansion rounds once after multiplication. Splitting
         * this into separately shifted bit terms introduces predictor drift
         * and does not match the FFmpeg encoder/reference WAV decoder. */
        const int32_t difference = (((int32_t)(code & 7U) * 2 + 1) * step) >> 3;
        stream->predictor += (code & 8U) ? -difference : difference;
        if (stream->predictor < -32768) stream->predictor = -32768;
        if (stream->predictor > 32767) stream->predictor = 32767;
        stream->step_index += k_ima_index_delta[code];
        if (stream->step_index < 0) stream->step_index = 0;
        if (stream->step_index > 88) stream->step_index = 88;
        samples[count++] = (int16_t)stream->predictor;
        ++stream->emitted;
    }
    return count;
}

#pragma once
#include <stdbool.h>
#include <stdint.h>

/* Provisional camera-luma thresholds, not calibrated lux. Require fresh,
 * continuous observations so a failed camera cannot be interpreted as dark. */
#define BOT_LIGHT_DARK_LUMA 25U
#define BOT_LIGHT_BRIGHT_LUMA 45U
#define BOT_LIGHT_DARK_HOLD_MS 30000U
#define BOT_LIGHT_BRIGHT_HOLD_MS 3000U
#define BOT_LIGHT_SAMPLE_GAP_MS 4000U
#define BOT_LIGHT_PROBE_INTERVAL_MS 30000U
#define BOT_LIGHT_PROBE_DURATION_MS 10000U

typedef struct {
    bool dark, valid, candidate, probing, probe_scheduled;
    uint8_t luma;
    uint32_t last_sample_ms, candidate_ms, probe_started_ms, next_probe_ms;
} bot_ambient_light_t;

static inline void bot_ambient_observe(bot_ambient_light_t *light, uint8_t luma, uint32_t stamp) {
    if (light->valid) {
        const int32_t gap = (int32_t)(stamp - light->last_sample_ms);
        if (gap <= 0) return;
        if ((uint32_t)gap > BOT_LIGHT_SAMPLE_GAP_MS) light->candidate = false;
    } else {
        light->candidate = false;
    }
    light->valid = true;
    light->last_sample_ms = stamp;
    light->luma = luma;
    const bool qualifies = light->dark ? luma >= BOT_LIGHT_BRIGHT_LUMA : luma <= BOT_LIGHT_DARK_LUMA;
    if (!qualifies) {
        light->candidate = false;
        return;
    }
    if (!light->candidate) {
        light->candidate = true;
        light->candidate_ms = stamp;
        return;
    }
    const uint32_t hold = light->dark ? BOT_LIGHT_BRIGHT_HOLD_MS : BOT_LIGHT_DARK_HOLD_MS;
    if ((uint32_t)(stamp - light->candidate_ms) >= hold) {
        light->dark = !light->dark;
        light->candidate = false;
        light->probing = false;
        light->probe_scheduled = false;
    }
}

static inline void bot_ambient_step(bot_ambient_light_t *light, uint32_t now, bool allow_probe) {
    if (light->valid && (int32_t)(now - light->last_sample_ms) >= 0 &&
        (uint32_t)(now - light->last_sample_ms) > BOT_LIGHT_SAMPLE_GAP_MS) {
        light->valid = false;
        light->candidate = false;
    }
    if (!light->dark || !allow_probe) {
        light->probing = false;
        light->probe_scheduled = false;
        return;
    }
    if (!light->probe_scheduled) {
        light->next_probe_ms = now + BOT_LIGHT_PROBE_INTERVAL_MS;
        light->probe_scheduled = true;
    }
    if (light->probing && (uint32_t)(now - light->probe_started_ms) >= BOT_LIGHT_PROBE_DURATION_MS) {
        light->probing = false;
        light->candidate = false;
        light->next_probe_ms = now + BOT_LIGHT_PROBE_INTERVAL_MS;
    } else if (!light->probing && (int32_t)(now - light->next_probe_ms) >= 0) {
        light->probing = true;
        light->probe_started_ms = now;
    }
}

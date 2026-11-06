/** Single official human focus. Track handles are NOT identity recognition. */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#include "vision_attention.h"
#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    BOT_FOCUS_NONE = 0, BOT_FOCUS_ACQUIRING, BOT_FOCUS_TRACKING,
    BOT_FOCUS_TEMPORARILY_LOST, BOT_FOCUS_REACQUIRING
} bot_focus_state_t;

typedef struct {
    bot_focus_state_t state;
    uint32_t generation, track_id;
    uint32_t observed_ms; /* last REAL camera observation; never refreshed during hold */
    bool localization_valid;
    bool gaze_evidence_valid;
    bool looking_at_bot;
    float confidence;
    vatt_point_t eye_midpoint;
    vatt_observation_t observation;
} bot_focus_snapshot_t;

typedef struct {
    uint32_t observation_max_age_ms;
    uint32_t acquire_dwell_ms;
    uint32_t switch_dwell_ms;
    uint32_t lost_hold_ms;
    float min_face_confidence;
    float switch_margin;
} attention_manager_config_t;

typedef struct {
    attention_manager_config_t config;
    bot_focus_snapshot_t focus;
    uint32_t candidate_id, candidate_since_ms;
    uint32_t last_capture_ms, last_candidate_capture_ms;
    uint32_t lost_since_ms;
    bool has_capture, candidate_has_capture, enabled, engaged, lost_since_valid;
} attention_manager_t;

attention_manager_config_t attention_manager_default_config(void);
bool attention_manager_init(attention_manager_t *manager, const attention_manager_config_t *config);
/** Disabling invalidates/releases visual focus. Session relationship is runtime-owned. */
void attention_manager_set_policy(attention_manager_t *manager, bool enabled, bool engaged);
/** Call from one owner task; NULL snapshot still expires stale positions.
 * Uses all faces, deliberately ignores vatt_snapshot_t.primary_track_id.
 * Returns true when focus fields change; copy output for other tasks. */
bool attention_manager_step(attention_manager_t *manager, const vatt_snapshot_t *snapshot,
                            uint32_t now_ms, bot_focus_snapshot_t *out);
const char *attention_manager_state_name(bot_focus_state_t state);
#ifdef __cplusplus
}
#endif

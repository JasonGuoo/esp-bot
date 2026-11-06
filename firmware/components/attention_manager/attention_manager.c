#include "attention_manager.h"
#include <math.h>
#include <string.h>

static bool fresh(uint32_t now, uint32_t observed, uint32_t max_age)
{
    int32_t age = (int32_t)(now - observed);
    return age >= 0 && (uint32_t)age <= max_age;
}

static uint32_t next_generation(uint32_t value)
{
    value++;
    return value ? value : 1; /* generation zero is reserved for no binding */
}

static void clear_candidate(attention_manager_t *manager)
{
    manager->candidate_id = 0;
    manager->candidate_since_ms = 0;
    manager->last_candidate_capture_ms = 0;
    manager->candidate_has_capture = false;
}

static void release_focus(attention_manager_t *manager)
{
    uint32_t generation = manager->focus.generation;
    if (manager->focus.track_id) generation = next_generation(generation);
    memset(&manager->focus, 0, sizeof(manager->focus));
    manager->focus.generation = generation;
    manager->focus.state = manager->enabled ? BOT_FOCUS_ACQUIRING : BOT_FOCUS_NONE;
    manager->lost_since_valid = false;
    clear_candidate(manager);
}

attention_manager_config_t attention_manager_default_config(void)
{
    return (attention_manager_config_t){
        .observation_max_age_ms = 350, .acquire_dwell_ms = 160,
        .switch_dwell_ms = 900, .lost_hold_ms = 1800,
        .min_face_confidence = 0.55f, .switch_margin = 0.18f
    };
}

bool attention_manager_init(attention_manager_t *manager, const attention_manager_config_t *config)
{
    if (!manager) return false;
    attention_manager_config_t cfg = config ? *config : attention_manager_default_config();
    if (!cfg.observation_max_age_ms || cfg.observation_max_age_ms >= INT32_MAX ||
        cfg.acquire_dwell_ms >= INT32_MAX || cfg.switch_dwell_ms >= INT32_MAX ||
        cfg.lost_hold_ms >= INT32_MAX || cfg.lost_hold_ms < cfg.observation_max_age_ms ||
        !isfinite(cfg.min_face_confidence) || cfg.min_face_confidence < 0 || cfg.min_face_confidence > 1 ||
        !isfinite(cfg.switch_margin) || cfg.switch_margin < 0 || cfg.switch_margin > 1)
        return false;
    memset(manager, 0, sizeof(*manager));
    manager->config = cfg;
    return true;
}

void attention_manager_set_policy(attention_manager_t *manager, bool enabled, bool engaged)
{
    if (!manager) return;
    bool changed = manager->enabled != enabled;
    manager->enabled = enabled;
    manager->engaged = engaged;
    if (!enabled) {
        manager->has_capture = false;
        if (changed || manager->focus.track_id) release_focus(manager);
    } else if (changed) {
        manager->focus.state = BOT_FOCUS_ACQUIRING;
        clear_candidate(manager);
    }
}

static bool usable_face(const attention_manager_t *manager, const vatt_face_t *face,
                         uint32_t capture_ms, uint32_t now_ms)
{
    const vatt_observation_t *observation = &face->observation;
    return face->track_id && face->visible && !face->association_ambiguous &&
        fresh(now_ms, face->last_seen_ms, manager->config.observation_max_age_ms) &&
        (int32_t)(capture_ms - face->last_seen_ms) >= 0 &&
        isfinite(observation->face_confidence) &&
        observation->face_confidence >= manager->config.min_face_confidence &&
        observation->face_confidence <= 1 &&
        isfinite(face->eye_midpoint.x) && isfinite(face->eye_midpoint.y) &&
        face->eye_midpoint.x >= 0 && face->eye_midpoint.x <= 1 &&
        face->eye_midpoint.y >= 0 && face->eye_midpoint.y <= 1 &&
        isfinite(observation->bbox.left) && isfinite(observation->bbox.right) &&
        isfinite(observation->bbox.top) && isfinite(observation->bbox.bottom) &&
        observation->bbox.left >= 0 && observation->bbox.top >= 0 &&
        observation->bbox.right <= 1 && observation->bbox.bottom <= 1 &&
        observation->bbox.right > observation->bbox.left &&
        observation->bbox.bottom > observation->bbox.top;
}

static float candidate_score(const vatt_face_t *face)
{
    float area = (face->observation.bbox.right - face->observation.bbox.left) *
                 (face->observation.bbox.bottom - face->observation.bbox.top);
    if (area > 1) area = 1;
    float center = 1 - (fabsf(face->eye_midpoint.x - 0.5f) + fabsf(face->eye_midpoint.y - 0.5f));
    /* Location and detection quality only. Looking at the bot is not consent,
     * and no inference about emotion, age, identity or who is speaking is made. */
    return face->observation.face_confidence * 0.70f + area * 0.20f + center * 0.10f;
}

static void assign_focus(attention_manager_t *manager, const vatt_face_t *face)
{
    if (manager->focus.track_id != face->track_id) {
        manager->focus.generation = next_generation(manager->focus.generation);
    }
    manager->focus.track_id = face->track_id;
    manager->focus.state = BOT_FOCUS_TRACKING;
    manager->focus.observed_ms = face->last_seen_ms;
    manager->focus.localization_valid = true;
    manager->focus.confidence = face->observation.face_confidence;
    manager->focus.eye_midpoint = face->eye_midpoint;
    manager->focus.observation = face->observation;
    manager->focus.gaze_evidence_valid = face->observation.eye_contact_valid &&
        face->gaze_evidence != VATT_GAZE_UNKNOWN &&
        isfinite(face->observation.gaze_quality) && face->observation.gaze_quality > 0;
    manager->focus.looking_at_bot = manager->focus.gaze_evidence_valid &&
                                   face->gaze_evidence == VATT_GAZE_AT_BOT;
    manager->lost_since_valid = false;
}

static bool candidate_dwelled(attention_manager_t *manager, const vatt_face_t *candidate,
                               uint32_t capture_ms, uint32_t dwell_ms)
{
    if (manager->candidate_id != candidate->track_id ||
        !manager->candidate_has_capture ||
        !fresh(capture_ms, manager->last_candidate_capture_ms, manager->config.observation_max_age_ms)) {
        manager->candidate_id = candidate->track_id;
        manager->candidate_since_ms = capture_ms;
        manager->candidate_has_capture = true;
    }
    manager->last_candidate_capture_ms = capture_ms;
    return (uint32_t)(capture_ms - manager->candidate_since_ms) >= dwell_ms;
}

bool attention_manager_step(attention_manager_t *manager, const vatt_snapshot_t *snapshot,
                            uint32_t now_ms, bot_focus_snapshot_t *out)
{
    if (!manager || !out) return false;
    bot_focus_snapshot_t before = manager->focus;
    if (!manager->enabled) {
        *out = manager->focus;
        return false;
    }
    bool current_frame = snapshot && snapshot->face_count <= VATT_MAX_FACES &&
        snapshot->state != VATT_UNAVAILABLE &&
        fresh(now_ms, snapshot->timestamp_ms, manager->config.observation_max_age_ms) &&
        (!manager->has_capture || (int32_t)(snapshot->timestamp_ms - manager->last_capture_ms) >= 0);
    bool new_frame = current_frame && (!manager->has_capture ||
                                    snapshot->timestamp_ms != manager->last_capture_ms);
    const vatt_face_t *current = NULL, *best = NULL;
    if (current_frame) {
        for (uint8_t i = 0; i < snapshot->face_count; i++) {
            const vatt_face_t *face = &snapshot->faces[i];
            if (!usable_face(manager, face, snapshot->timestamp_ms, now_ms)) continue;
            if (face->track_id == manager->focus.track_id) current = face;
            if (!best || candidate_score(face) > candidate_score(best) ||
                (candidate_score(face) == candidate_score(best) && face->track_id < best->track_id))
                best = face;
        }
    }
    if (current) {
        assign_focus(manager, current);
        /* During an engaged conversation retain the same visible partner.
         * Temporary visual loss also cannot silently select somebody else. */
        if (!manager->engaged && new_frame && best && best->track_id != current->track_id &&
            candidate_score(best) >= candidate_score(current) + manager->config.switch_margin) {
            if (candidate_dwelled(manager, best, snapshot->timestamp_ms, manager->config.switch_dwell_ms)) {
                assign_focus(manager, best);
                clear_candidate(manager);
            }
        } else if (new_frame) {
            clear_candidate(manager);
        }
    } else if (manager->focus.track_id) {
        manager->focus.localization_valid = false;
        manager->focus.gaze_evidence_valid = false;
        manager->focus.looking_at_bot = false;
        manager->focus.state = BOT_FOCUS_TEMPORARILY_LOST;
        clear_candidate(manager);
        if (!manager->lost_since_valid) {
            manager->lost_since_ms = now_ms;
            manager->lost_since_valid = true;
        }
        if (!fresh(now_ms, manager->focus.observed_ms, manager->config.lost_hold_ms)) {
            release_focus(manager);
            manager->focus.state = BOT_FOCUS_REACQUIRING;
        }
    }
    if (!manager->focus.track_id) {
        manager->focus.localization_valid = false;
        manager->focus.gaze_evidence_valid = false;
        manager->focus.looking_at_bot = false;
        if (new_frame && best) {
            if (candidate_dwelled(manager, best, snapshot->timestamp_ms, manager->config.acquire_dwell_ms)) {
                assign_focus(manager, best);
                clear_candidate(manager);
            }
        } else if (new_frame || !current_frame) {
            clear_candidate(manager);
        }
    }
    if (new_frame) {
        manager->last_capture_ms = snapshot->timestamp_ms;
        manager->has_capture = true;
    }
    *out = manager->focus;
    return memcmp(&before, &manager->focus, sizeof(before)) != 0;
}

const char *attention_manager_state_name(bot_focus_state_t state)
{
    switch (state) {
    case BOT_FOCUS_NONE: return "none";
    case BOT_FOCUS_ACQUIRING: return "acquiring";
    case BOT_FOCUS_TRACKING: return "tracking";
    case BOT_FOCUS_TEMPORARILY_LOST: return "temporarily_lost";
    case BOT_FOCUS_REACQUIRING: return "reacquiring";
    default: return "unknown";
    }
}

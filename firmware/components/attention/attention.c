#include "attention.h"
#include <math.h>
#include <stddef.h>
#include <string.h>

static float absf(float x) { return x < 0 ? -x : x; }
static float clampf(float x, float lo, float hi) {
    return x < lo ? lo : (x > hi ? hi : x);
}
static float move_to(float current, float goal, float max_delta) {
    if (goal > current + max_delta) return current + max_delta;
    if (goal < current - max_delta) return current - max_delta;
    return goal;
}
static int visual_offset(float residual, float limit, bool invert) {
    float scaled = 75.0f * clampf(residual / limit, -1.0f, 1.0f);
    if (invert) scaled = -scaled;
    return (int)(scaled + (scaled < 0 ? -0.5f : 0.5f));
}
static bool valid_pose(const attention_pose_t *p) {
    return p && isfinite(p->yaw_deg) && isfinite(p->pitch_deg);
}

attention_config_t attention_default_config(void) {
    return (attention_config_t){
        .yaw_min_deg=-45, .yaw_max_deg=45,
        .pitch_min_deg=-18, .pitch_max_deg=25,
        .head_speed_deg_per_s=55,
        .eye_comfort_yaw_deg=5, .eye_comfort_pitch_deg=4,
        .eye_max_yaw_deg=12, .eye_max_pitch_deg=9,
        .camera_yaw_offset_deg=0, .camera_pitch_offset_deg=0,
        .observation_hold_ms=800, .contact_fresh_ms=180,
        .contact_dwell_ms=320,
        .person_min_confidence=0.65f, .looking_min_confidence=0.80f
    };
}

bool attention_controller_init(attention_controller_t *ctl,
                               const attention_config_t *cfg) {
    if (!ctl || !cfg) return false;
    if (!(cfg->yaw_min_deg < 0 && cfg->yaw_max_deg > 0 &&
          cfg->pitch_min_deg < 0 && cfg->pitch_max_deg > 0 &&
          cfg->eye_comfort_yaw_deg > 0 && cfg->eye_comfort_pitch_deg > 0 &&
          cfg->eye_max_yaw_deg >= cfg->eye_comfort_yaw_deg &&
          cfg->eye_max_pitch_deg >= cfg->eye_comfort_pitch_deg &&
          cfg->head_speed_deg_per_s > 0 &&
          cfg->observation_hold_ms > cfg->contact_fresh_ms &&
          cfg->contact_fresh_ms > 0 && cfg->contact_dwell_ms > 0 &&
          cfg->person_min_confidence > 0 && cfg->person_min_confidence <= 1 &&
          cfg->looking_min_confidence > 0 && cfg->looking_min_confidence <= 1))
        return false;
    float v[] = {cfg->yaw_min_deg, cfg->yaw_max_deg, cfg->pitch_min_deg,
                 cfg->pitch_max_deg, cfg->head_speed_deg_per_s,
                 cfg->eye_comfort_yaw_deg, cfg->eye_comfort_pitch_deg,
                 cfg->eye_max_yaw_deg, cfg->eye_max_pitch_deg,
                 cfg->camera_yaw_offset_deg, cfg->camera_pitch_offset_deg};
    for (size_t i=0; i<sizeof(v)/sizeof(v[0]); i++) if (!isfinite(v[i])) return false;
    memset(ctl, 0, sizeof(*ctl));
    ctl->config = *cfg;
    return true;
}

bool attention_controller_observe(attention_controller_t *ctl,
                                  const attention_observation_t *obs,
                                  const attention_pose_t *head) {
    if (!ctl || !obs || !valid_pose(head)) return false;
    const attention_config_t *cfg=&ctl->config;
    if (!isfinite(obs->person_yaw_from_camera_deg) ||
        !isfinite(obs->person_pitch_from_camera_deg) ||
        !isfinite(obs->person_confidence) ||
        !isfinite(obs->looking_confidence) ||
        obs->person_confidence < cfg->person_min_confidence ||
        obs->person_confidence > 1 ||
        obs->looking_confidence < 0 || obs->looking_confidence > 1)
        return false;
    if (ctl->has_target &&
        (int32_t)(obs->timestamp_ms - ctl->last_observation_ms) < 0)
        return false;
    if (!ctl->has_target || ctl->last_track_id != obs->track_id)
        ctl->candidate_contact = false;
    ctl->has_target = true;
    ctl->last_track_id = obs->track_id;
    ctl->last_observation_ms = obs->timestamp_ms;
    /* Camera is on rotating head: reconstruct direction in fixed base frame. */
    ctl->target_base_yaw_deg = head->yaw_deg + cfg->camera_yaw_offset_deg +
                                obs->person_yaw_from_camera_deg;
    ctl->target_base_pitch_deg = head->pitch_deg + cfg->camera_pitch_offset_deg +
                                  obs->person_pitch_from_camera_deg;
    ctl->user_looking = obs->user_looking_at_robot;
    ctl->looking_confidence = obs->looking_confidence;
    return true;
}

attention_output_t attention_controller_step(attention_controller_t *ctl,
                                             const attention_pose_t *head,
                                             uint32_t now_ms,
                                             bool has_head_motors) {
    attention_output_t out = {0};
    if (!ctl || !valid_pose(head)) return out;
    const attention_config_t *cfg = &ctl->config;
    bool valid = ctl->has_target &&
        (uint32_t)(now_ms - ctl->last_observation_ms) <= cfg->observation_hold_ms;
    if (!valid) {
        ctl->has_target = false;
        ctl->candidate_contact = false;
    }
    out.person_tracked = valid;
    float ry = valid ? ctl->target_base_yaw_deg - head->yaw_deg : 0;
    float rp = valid ? ctl->target_base_pitch_deg - head->pitch_deg : 0;
    out.residual_yaw_deg = ry;
    out.residual_pitch_deg = rp;
    out.eye_x = valid ? visual_offset(ry, cfg->eye_max_yaw_deg, false) : 0;
    out.eye_y = valid ? visual_offset(rp, cfg->eye_max_pitch_deg, true) : 0;

    /* Head handles large angles, eyes supply the modest remaining correction.
       Use a comfort dead-zone to prevent constant unnecessary servo jitter. */
    float target_yaw = 0, target_pitch = 0;
    if (valid && has_head_motors) {
        float yaw = ctl->target_base_yaw_deg;
        float pitch = ctl->target_base_pitch_deg;
        target_yaw = yaw > 0 ? fmaxf(0, yaw - cfg->eye_comfort_yaw_deg)
                             : fminf(0, yaw + cfg->eye_comfort_yaw_deg);
        target_pitch = pitch > 0 ? fmaxf(0, pitch - cfg->eye_comfort_pitch_deg)
                                 : fminf(0, pitch + cfg->eye_comfort_pitch_deg);
    }
    target_yaw = clampf(target_yaw, cfg->yaw_min_deg, cfg->yaw_max_deg);
    target_pitch = clampf(target_pitch, cfg->pitch_min_deg, cfg->pitch_max_deg);
    if (!ctl->has_stepped) {
        ctl->has_stepped = true;
        ctl->head_cmd_yaw_deg = head->yaw_deg;
        ctl->head_cmd_pitch_deg = head->pitch_deg;
        ctl->last_step_ms = now_ms;
    }
    uint32_t elapsed = (uint32_t)(now_ms - ctl->last_step_ms);
    if (elapsed > 100) elapsed = 100;
    ctl->last_step_ms = now_ms;
    if (has_head_motors) {
        float delta = cfg->head_speed_deg_per_s * (float)elapsed / 1000;
        ctl->head_cmd_yaw_deg = move_to(ctl->head_cmd_yaw_deg, target_yaw, delta);
        ctl->head_cmd_pitch_deg = move_to(ctl->head_cmd_pitch_deg, target_pitch, delta);
        out.head_motion_requested = true;
    } else {
        ctl->head_cmd_yaw_deg = head->yaw_deg;
        ctl->head_cmd_pitch_deg = head->pitch_deg;
    }
    out.head_target_yaw_deg = ctl->head_cmd_yaw_deg;
    out.head_target_pitch_deg = ctl->head_cmd_pitch_deg;

    /* Presence in view is not mutual gaze. Require independent gaze estimate,
       actual reachable direction, current observation, and stable dwell. */
    bool fresh = valid && (uint32_t)(now_ms - ctl->last_observation_ms) <=
                         cfg->contact_fresh_ms;
    out.person_looking_at_robot = fresh && ctl->user_looking &&
        ctl->looking_confidence >= cfg->looking_min_confidence;
    bool aligned = absf(ry) <= cfg->eye_max_yaw_deg &&
                   absf(rp) <= cfg->eye_max_pitch_deg;
    bool contact_candidate = out.person_looking_at_robot && aligned;
    if (!contact_candidate) ctl->candidate_contact = false;
    else if (!ctl->candidate_contact) {
        ctl->candidate_contact = true;
        ctl->contact_since_ms = now_ms;
    }
    out.mutual_eye_contact = contact_candidate && ctl->candidate_contact &&
        (uint32_t)(now_ms - ctl->contact_since_ms) >= cfg->contact_dwell_ms;
    return out;
}

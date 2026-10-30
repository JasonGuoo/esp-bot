/* Spatial attention: one target -> coordinated head setpoints and both eyes.
 * Hardware-agnostic: no LVGL, camera driver, motor driver, networking or LLM.
 * Positive yaw = robot-right; positive pitch = up.
 * Display eye y is positive DOWN, so positive pitch maps to negative eye_y.
 * Presence does not imply the user is looking at the robot.
 */
#pragma once
#include <stdbool.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    float yaw_deg, pitch_deg;
} attention_pose_t;

typedef struct {
    uint32_t timestamp_ms, track_id;
    float person_yaw_from_camera_deg, person_pitch_from_camera_deg;
    float person_confidence;              /* 0..1 */
    bool user_looking_at_robot;           /* requires separate calibrated detector */
    float looking_confidence;             /* 0..1 */
} attention_observation_t;

typedef struct {
    float yaw_min_deg, yaw_max_deg, pitch_min_deg, pitch_max_deg;
    float head_speed_deg_per_s, eye_comfort_yaw_deg, eye_comfort_pitch_deg;
    float eye_max_yaw_deg, eye_max_pitch_deg;
    float camera_yaw_offset_deg, camera_pitch_offset_deg;
    uint32_t observation_hold_ms, contact_fresh_ms, contact_dwell_ms;
    float person_min_confidence, looking_min_confidence;
} attention_config_t;

typedef struct {
    bool person_tracked, person_looking_at_robot, mutual_eye_contact;
    bool head_motion_requested;
    float head_target_yaw_deg, head_target_pitch_deg;
    int eye_x, eye_y;                     /* SAME for both eyes, -75..75 */
    float residual_yaw_deg, residual_pitch_deg;
} attention_output_t;

typedef struct {
    attention_config_t config;
    bool has_target, has_stepped, candidate_contact;
    uint32_t last_observation_ms, last_step_ms, contact_since_ms, last_track_id;
    bool user_looking;
    float looking_confidence;
    float target_base_yaw_deg, target_base_pitch_deg;
    float head_cmd_yaw_deg, head_cmd_pitch_deg;
} attention_controller_t;

attention_config_t attention_default_config(void);
bool attention_controller_init(attention_controller_t *controller,
                               const attention_config_t *config);
/* Observations use angles RELATIVE TO MOVING CAMERA.
 * measured_head is measured/estimated servo pose, never just commanded pose. */
bool attention_controller_observe(attention_controller_t *controller,
                                  const attention_observation_t *observation,
                                  const attention_pose_t *measured_head);
/* Call at 20-50 Hz; no motor I/O is performed by this function. */
attention_output_t attention_controller_step(attention_controller_t *controller,
                                             const attention_pose_t *measured_head,
                                             uint32_t now_ms,
                                             bool has_head_motors);
#ifdef __cplusplus
}
#endif

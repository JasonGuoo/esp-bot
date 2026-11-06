/** Stationary hardware adapter. Zero pose is not a measurement. */
#pragma once
#include <stdbool.h>
#include "esp_err.h"
#ifdef __cplusplus
extern "C" {
#endif
typedef struct {
    bool stationary;
    bool motors_installed;
    bool imu_installed;
    bool measured_pose_valid;
    float measured_yaw_deg, measured_pitch_deg;
} head_motion_status_t;
head_motion_status_t head_motion_get_status(void);
esp_err_t head_motion_request(float yaw_deg, float pitch_deg);
/** Idempotent, no hardware side effects on the stationary adapter. */
void head_motion_emergency_stop(void);
#ifdef __cplusplus
}
#endif

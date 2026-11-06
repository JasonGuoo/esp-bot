#include "head_motion.h"
#include <math.h>
head_motion_status_t head_motion_get_status(void) {
    return (head_motion_status_t){
        .stationary = true, .motors_installed = false, .imu_installed = false,
        .measured_pose_valid = false,
        .measured_yaw_deg = NAN, .measured_pitch_deg = NAN,
    };
}
esp_err_t head_motion_request(float yaw_deg, float pitch_deg) {
    (void)yaw_deg; (void)pitch_deg;
    return ESP_ERR_NOT_SUPPORTED;
}
void head_motion_emergency_stop(void) { /* No actuator is installed. */ }

# Pure-C, host-testable perception core. Camera ESP-DL/esp_video adapter
# remains separate so the component is portable and never owns LVGL or motors.
idf_component_register(
    SRCS "vision_attention.c" "pupil_candidate.c"
    INCLUDE_DIRS "include"
)

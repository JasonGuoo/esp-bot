idf_component_register(
    SRCS "bot_platform.c"
    INCLUDE_DIRS "include"
    REQUIRES espressif__esp32_s31_korvo_1 lvgl__lvgl esp_timer nvs_flash
)

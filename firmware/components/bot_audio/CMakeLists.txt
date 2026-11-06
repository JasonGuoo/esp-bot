idf_component_register(
    SRCS "bot_audio.c" "bot_audio_afe.c"
    INCLUDE_DIRS "include"
    PRIV_INCLUDE_DIRS "."
    REQUIRES esp_common
    PRIV_REQUIRES freertos esp_timer esp_partition esp_driver_gpio esp_driver_i2s
                  esp32_s31_korvo_1 esp_codec_dev esp-sr
)

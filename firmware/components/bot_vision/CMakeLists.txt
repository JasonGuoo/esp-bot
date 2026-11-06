idf_component_register(
    SRCS "bot_vision.cpp"
    INCLUDE_DIRS "include"
    REQUIRES vision_attention esp_common
    PRIV_REQUIRES esp_timer freertos heap vfs esp_video human_face_detect esp-dl
)
target_compile_features(${COMPONENT_LIB} PRIVATE cxx_std_17)

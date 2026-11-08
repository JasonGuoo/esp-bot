idf_component_register(SRCS "bot_debug.cpp" INCLUDE_DIRS "include"
    REQUIRES bot_platform bot_bus vision_attention esp_common
    PRIV_REQUIRES esp_timer freertos heap log esp_rom esp-dl espressif__esp_new_jpeg)
target_compile_features(${COMPONENT_LIB} PRIVATE cxx_std_17)

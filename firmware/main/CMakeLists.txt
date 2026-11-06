idf_component_register(
    SRCS "app_main.c"
    INCLUDE_DIRS "."
    REQUIRES bot_platform bot_bus bot_runtime bot_audio bot_vision
             attention attention_manager head_motion face brain_link
)

idf_component_register(
    SRCS "app_main.c" "wake_response_player.c"
    INCLUDE_DIRS "."
    REQUIRES bot_platform bot_bus bot_runtime bot_audio bot_responses bot_vision
             attention attention_manager head_motion face brain_link bot_debug
)

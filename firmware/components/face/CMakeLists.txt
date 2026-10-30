idf_component_register(
    SRCS
        "face.c"
        "face_settings.c"
        "face_renderer.c"
        "renderer_outline.c"
        "renderer_pill.c"
    INCLUDE_DIRS "include"
    PRIV_INCLUDE_DIRS "."
    REQUIRES lvgl__lvgl
)

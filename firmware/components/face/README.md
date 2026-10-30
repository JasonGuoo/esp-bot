# Face component (standalone LVGL 9 module)

The reusable ESP32-side visual expression module lives here, not inside the Brain client. It has **no Wi-Fi, audio, camera or LLM dependency**.

Public header: include/face.h

Example (call from LVGL task or with bsp_display_lock):

    face_view_t *face = face_create(lv_screen_active());
    face_set_theme(face, "sky");
    face_set_expression(face, "listening");
    face_set_gaze(face, 45, -20);
    face_set_subtitle(face, "I'm listening...");
    face_reveal_settings(face);

Theme names: mint, sakura, lavender, butter, sky. The theme changes the background, **both** eyes, captions and settings as a unit. Colors originate in shared/themes.json; regenerate the C header from repository root with:

    python3 tools/generate_face_themes.py

Expressions: idle, listening, happy, curious, thinking, surprised, sleepy, shy, blink. Happy has no mouth; only surprised/shy may display a dot or tiny curve. Idle and listening blink automatically. The eyes themselves are simple LVGL objects.

Future modules should call this API to set expression and gaze, not edit the eye LVGL children themselves. For a FreeRTOS worker task, acquire/release the BSP LVGL display lock around calls.

The current board target is ESP32-S31-Korvo-1 (800x480, GT1151 touch). Boot and display are initialized in firmware/main/app_main.c; the face component never configures GPIO pins. Chinese captions require a CJK-capable LVGL font in the on-device build. The current code has not been hardware-validated.

/* 小呆 composition root. One task owns policy; adapters own their workers.
 * No HTTP poller, second microphone reader, UI decision maker, or fake head pose.
 * First-board execution is recorded in docs/board-bringup-2026-11-06.md. */
#include <math.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "sdkconfig.h"
#include "esp_log.h"
#include "esp_heap_caps.h"
#include "esp_random.h"
#include "esp_rom_sys.h"
#include "esp_system.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "attention.h"
#include "idle_attention.h"
#include "bot_audio.h"
#include "bot_debug.h"
#include "bot_bus.h"
#include "bot_platform.h"
#include "bot_runtime.h"
#include "bot_vision.h"
#include "brain_link.h"
#include "face.h"
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
#include "face_expressions_generated.h"
#endif
#include "head_motion.h"
#include "night_schedule.h"
#include "preferences_worker.h"
#include "wake_response_player.h"

static const char *TAG = "xiaodai";
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
typedef struct { uint16_t x, y; } debug_touch_point_t;
#endif
typedef struct {
    bot_platform_t *platform;
    bot_bus_t *bus;
    bot_runtime_t *runtime;
    bot_audio_t *audio;
    bot_response_player_t *response_player;
    bot_vision_t *vision;
    face_view_t *face;
    atomic_uint text_requested, text_started, text_flushing, text_ready;
    atomic_uint image_requested, image_started, image_flushing, image_ready;
    bool display_refresh_active; /* LVGL lock only; excludes diagnostic snapshots */
    unsigned text_generation, image_generation;
    uint32_t result_sequence, image_sequence;
    bot_session_token_t result_token, image_token;
    char image_asset_id[BOT_DISPLAY_ASSET_ID_BYTES];
    attention_controller_t attention;
    idle_attention_t idle_attention;
    bot_runtime_snapshot_t desired;
    bot_voice_settings_t voice_persist_attempt;
    /* Written only from the LVGL UI owner. Not the runtime session identity. */
    uint32_t settings_lease, settings_revision;
    bool settings_has_value;
    bot_voice_settings_t settings_last_value;
    bot_preferences_request_t preferences;
    bot_preferences_result_t persisted;
    bot_voice_diagnostics_t voice_diagnostics;
    uint32_t last_voice_diagnostic_ms;
    bool voice_preview_reported;
    char voice_notice[96];
    uint8_t camera_orientation;
    QueueHandle_t theme_queue;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    QueueHandle_t debug_eye_queue;
    QueueHandle_t debug_camera_queue;
    QueueHandle_t debug_face_queue;
    QueueHandle_t debug_orientation_queue;
    QueueHandle_t debug_light_queue;
    QueueHandle_t debug_touch_queue;
    debug_touch_point_t debug_touch_point;
    uint32_t debug_touch_release_ms;
    bool debug_touch_pressed;
    bool debug_camera_wanted;
    uint32_t debug_eye_until_ms;
    int debug_eye_x;
    uint32_t debug_face_until_ms;
    char debug_face[BOT_EXPRESSION_MAX_BYTES + 1];
    bool simulated_light;
    int simulated_luma;
    uint32_t simulated_light_until_ms;
#endif
    bool vision_health_sent, vision_available; /* only written by vision callback */
    esp_err_t vision_error;
    bool display_health_sent, last_display_awake;
    bool camera_request_sent, last_camera_requested;
    bool light_mode_sent, last_light_mode;
    bool ambient_reported, last_ambient_dark, last_ambient_probe;
    bool near_face_reported, last_near_face;
    bool night_sent, last_night, last_clock_valid;
    uint8_t last_local_hour;
    uint32_t last_clock_poll_ms, last_diagnostic_ms, last_light_poll_ms, last_audio_retry_ms;
    char rendered_caption[BOT_CAPTION_MAX_BYTES + 1];
    char rendered_expression[BOT_EXPRESSION_MAX_BYTES + 1];
} app_t;

static void post(app_t *app, const bot_event_t *event) {
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    bot_debug_event(event->kind, event->source, event->observed_ms);
#endif
    esp_err_t err = bot_bus_post(app->bus, event, 0);
    if (err != ESP_OK) ESP_LOGW(TAG, "Event %d queue: %s", event->kind, esp_err_to_name(err));
}
static bot_session_token_t runtime_token(bot_audio_token_t t) {
    return (bot_session_token_t){t.epoch, t.session_id, t.turn_id, t.focus_generation};
}
static bool same_runtime_token(bot_session_token_t a, bot_session_token_t b) {
    return a.epoch==b.epoch && a.session_id==b.session_id && a.turn_id==b.turn_id &&
           a.focus_generation==b.focus_generation;
}
static void display_refresh(lv_event_t *event) {
    app_t *app=lv_event_get_user_data(event);
    if (lv_event_get_code(event)==LV_EVENT_REFR_START) {
        app->display_refresh_active=true;
        atomic_store(&app->text_started,0);
        atomic_store(&app->image_started,0);
    } else if (lv_event_get_code(event)==LV_EVENT_REFR_READY) {
        app->display_refresh_active=false;
        unsigned text=atomic_exchange(&app->text_started,0);
        unsigned image=atomic_exchange(&app->image_started,0);
        unsigned expected=text;
        bool await_flush=false;
        if (text && atomic_compare_exchange_strong(&app->text_requested,&expected,0))
            { atomic_store(&app->text_flushing,text); await_flush=true; }
        expected=image;
        if (image && atomic_compare_exchange_strong(&app->image_requested,&expected,0))
            { atomic_store(&app->image_flushing,image); await_flush=true; }
        /* REFR_READY and FLUSH_FINISH can precede asynchronous DMA completion.
         * One further invalidation makes LVGL wait for this refresh's final
         * flush even when the result is otherwise completely static. */
        if (await_flush) lv_obj_invalidate(face_root(app->face));
    } else if (lv_event_get_code(event)==LV_EVENT_FLUSH_WAIT_FINISH) {
        unsigned text=atomic_exchange(&app->text_flushing,0);
        unsigned image=atomic_exchange(&app->image_flushing,0);
        if (text) atomic_store(&app->text_ready,text);
        if (image) atomic_store(&app->image_ready,image);
    }
}
static void result_drawn(face_result_part_t part, void *context) {
    app_t *app=context;
    if (!app->display_refresh_active) return;
    /* DRAW_MAIN_END is delivered only for a widget intersecting LVGL's clip.
     * Keep the request armed if the image is below the scroll viewport. */
    if (part==FACE_RESULT_TEXT)
        atomic_store(&app->text_started,atomic_load(&app->text_requested));
    else if (part==FACE_RESULT_IMAGE)
        atomic_store(&app->image_started,atomic_load(&app->image_requested));
}
static void clear_display_receipts(app_t *app) {
    atomic_store(&app->text_requested,0);
    atomic_store(&app->text_started,0);
    atomic_store(&app->text_flushing,0);
    atomic_store(&app->text_ready,0);
    atomic_store(&app->image_requested,0);
    atomic_store(&app->image_started,0);
    atomic_store(&app->image_flushing,0);
    atomic_store(&app->image_ready,0);
}
static bot_audio_token_t audio_token(bot_session_token_t t) {
    return (bot_audio_token_t){t.epoch, t.session_id, t.turn_id, t.focus_generation};
}
static void audio_pcm(void *context, const bot_audio_pcm_t *frame) {
    (void)context;
    brain_secure_pcm(frame);
}

static void audio_event(void *context, const bot_audio_event_t *source) {
    app_t *app = context;
    /* The transport only accepts SESSION-source hardware receipts. */
    brain_secure_audio_event(source);
    bot_event_t event = { .source = BOT_SOURCE_AUDIO,
        .observed_ms = (uint32_t)(source->observed_us / 1000),
        .token = runtime_token(source->token) };
    switch (source->kind) {
    case BOT_AUDIO_EVENT_HEALTH:
        event.kind = BOT_EVENT_AUDIO_HEALTH;
        event.data.health.available = source->available;
        event.data.health.error = source->error;
        break;
    case BOT_AUDIO_EVENT_WAKE_STATUS:
        event.kind = BOT_EVENT_WAKE_STATUS;
        event.data.wake_status.status = source->wake_status == BOT_AUDIO_WAKE_READY ? BOT_WAKE_READY :
            source->wake_status == BOT_AUDIO_WAKE_MODEL_MISSING ? BOT_WAKE_MODEL_MISSING : BOT_WAKE_UNAVAILABLE;
        ESP_LOGI(TAG, "Wake status=%d: %s", event.data.wake_status.status,
                 source->reason ? source->reason : "");
        break;
    case BOT_AUDIO_EVENT_WAKE_DETECTED:
        if (!source->wake_phrase || strcmp(source->wake_phrase, BOT_AUDIO_WAKE_PHRASE) != 0) return;
        event.kind = BOT_EVENT_WAKE_DETECTED;
        event.data.wake.phrase = BOT_WAKE_PHRASE_XIAODAI_XIAODAI;
        event.data.wake.confidence_available = source->confidence_available;
        event.data.wake.confidence = source->confidence;
        event.data.wake.echo_safe = source->echo_safe;
        ESP_LOGI(TAG, "Full wake phrase detected: backend=%d score_available=%d score=%.3f",
                 source->wake_backend, source->confidence_available, (double)source->confidence);
        break;
    case BOT_AUDIO_EVENT_BARGE_IN:
        event.kind = BOT_EVENT_BARGE_IN;
        event.data.playback.id = source->playback_id;
        break;
    case BOT_AUDIO_EVENT_VAD_STARTED:
        event.kind = BOT_EVENT_UTTERANCE_STARTED;
        event.data.utterance.echo_safe = source->echo_safe;
        break;
    case BOT_AUDIO_EVENT_VAD_ENDED:
        event.kind = BOT_EVENT_UTTERANCE_ENDED;
        event.data.utterance.echo_safe = source->echo_safe;
        break;
    case BOT_AUDIO_EVENT_CAPTURE_READY:
        event.kind = BOT_EVENT_CAPTURE_READY;
        event.data.capture.ready = source->available;
        break;
    case BOT_AUDIO_EVENT_BUFFER_OVERFLOW:
        event.kind = BOT_EVENT_CAPTURE_OVERFLOW;
        break;
    case BOT_AUDIO_EVENT_LOCAL_COMMAND:
        event.kind = BOT_EVENT_LOCAL_COMMAND;
        event.data.local_command.command = source->command;
        event.data.local_command.score = source->confidence;
        ESP_LOGI(TAG, "Local voice command=%s score=%.3f epoch=%lu turn=%lu",
                 bot_voice_command_name(source->command), (double)source->confidence,
                 (unsigned long)source->token.epoch, (unsigned long)source->token.turn_id);
        break;
    case BOT_AUDIO_EVENT_CONTROLS_APPLIED:
        ESP_LOGI(TAG, "Voice controls revision=%lu result=%s", (unsigned long)source->controls_revision,
                 esp_err_to_name(source->error));
        return;
    case BOT_AUDIO_EVENT_AMBIENT_SOUND:
        event.kind = BOT_EVENT_SOUND_ACTIVITY;
        event.data.sound.direction_valid = source->direction_valid;
        event.data.sound.azimuth_deg = source->azimuth_deg;
        event.data.sound.quality = source->direction_quality;
        ESP_LOGI(TAG, "sound activity speech=%d direction_valid=%d az=%.1f quality=%.1f eye_if_awake=%d (screen-x)",
                 source->speech, source->direction_valid, source->azimuth_deg, source->direction_quality,
                 source->direction_valid ? attention_sound_eye_x(source->azimuth_deg) : 0);
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_STARTED:
        event.kind = BOT_EVENT_PLAYBACK_STARTED;
        event.data.playback.id = source->playback_id;
        event.data.playback.purpose = source->playback_purpose;
        event.data.playback.presentation_managed = source->presentation_managed;
        event.data.playback.local_response = source->playback_source == BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE;
        event.data.playback.preview = source->playback_source == BOT_AUDIO_PLAYBACK_PREVIEW;
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_PROGRESS:
        event.kind = BOT_EVENT_PLAYBACK_PROGRESS;
        event.data.playback.id = source->playback_id;
        event.data.playback.purpose = source->playback_purpose;
        event.data.playback.presentation_managed = source->presentation_managed;
        event.data.playback.local_response = source->playback_source == BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE;
        event.data.playback.preview = source->playback_source == BOT_AUDIO_PLAYBACK_PREVIEW;
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_DONE:
        event.kind = BOT_EVENT_PLAYBACK_DONE;
        event.data.playback.id = source->playback_id;
        event.data.playback.purpose = source->playback_purpose;
        event.data.playback.presentation_managed = source->presentation_managed;
        event.data.playback.local_response = source->playback_source == BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE;
        event.data.playback.preview = source->playback_source == BOT_AUDIO_PLAYBACK_PREVIEW;
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_CANCELLED:
    case BOT_AUDIO_EVENT_PLAYBACK_FAILED:
        event.kind = BOT_EVENT_PLAYBACK_FAILED;
        event.data.playback.id = source->playback_id;
        event.data.playback.purpose = source->playback_purpose;
        event.data.playback.presentation_managed = source->presentation_managed;
        event.data.playback.local_response = source->playback_source == BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE;
        event.data.playback.preview = source->playback_source == BOT_AUDIO_PLAYBACK_PREVIEW;
        break;
    default: return;
    }
    post(app, &event);
}

static esp_err_t prepare_camera(void *context) {
    return bot_platform_prepare_camera(((app_t *)context)->platform);
}
static void vision_snapshot(void *context, const vatt_snapshot_t *snapshot) {
    app_t *app = context;
    esp_err_t err = bot_bus_publish_vision(app->bus, snapshot);
    if (err != ESP_OK && err != ESP_ERR_INVALID_STATE)
        ESP_LOGW(TAG, "Vision snapshot: %s", esp_err_to_name(err));
}
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
static void vision_preview(void *context, const void *pixels, uint16_t width, uint16_t height,
                           bot_vision_pixels_t format, uint32_t stamp,
                           const vatt_observation_t *observations, size_t count) {
    (void)context;
    static const bot_debug_pixels_t formats[] = {
        BOT_DEBUG_RGB565_LE, BOT_DEBUG_RGB565_BE, BOT_DEBUG_RGB888, BOT_DEBUG_UYVY, BOT_DEBUG_YUYV,
    };
    if ((unsigned)format < sizeof(formats) / sizeof(formats[0]))
        bot_debug_camera(pixels, width, height, formats[format], stamp, observations, count);
}
#endif
static void vision_status(void *context, const bot_vision_status_t *status) {
    app_t *app = context;
    bool available = status->stream_active && status->observation_valid;
    if (app->vision_health_sent && app->vision_available == available &&
        app->vision_error == status->error) return;
    app->vision_health_sent = true;
    app->vision_available = available;
    app->vision_error = status->error;
    bot_event_t event = { .kind = BOT_EVENT_CAMERA_HEALTH, .source = BOT_SOURCE_VISION,
        .observed_ms = status->observed_ms,
        .data.health = { .available = available, .error = status->error } };
    post(app, &event);
}

static void touch_wake(void *context) {
    app_t *app = context;
    ESP_LOGI(TAG, "Intentional screen touch received");
    bot_event_t event = { .kind = BOT_EVENT_TOUCH_WAKE, .source = BOT_SOURCE_LOCAL_UI,
        .observed_ms = bot_platform_now_ms(), .data.touch.intentional = true };
    post(app, &event);
}
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
/* Official LVGL pointer input path, restricted to an explicit physical USB
 * preview lease. It exercises the actual widgets and their normal runtime
 * callbacks; no settings/session authority is granted to the adapter. */
static void debug_touch_read(lv_indev_t *indev, lv_indev_data_t *data) {
    app_t *app = lv_indev_get_user_data(indev);
    const uint32_t now = bot_platform_now_ms();
    if (!bot_debug_preview_enabled()) {
        app->debug_touch_pressed = false;
        xQueueReset(app->debug_touch_queue);
    } else if (app->debug_touch_pressed) {
        if ((int32_t)(now - app->debug_touch_release_ms) >= 0)
            app->debug_touch_pressed = false;
    } else if (xQueueReceive(app->debug_touch_queue, &app->debug_touch_point, 0) == pdTRUE) {
        app->debug_touch_pressed = true;
        app->debug_touch_release_ms = now + 120;
    }
    data->point.x = app->debug_touch_point.x;
    data->point.y = app->debug_touch_point.y;
    data->state = app->debug_touch_pressed ? LV_INDEV_STATE_PRESSED : LV_INDEV_STATE_RELEASED;
}
#endif
#if CONFIG_ESPBOT_USB_MANUAL_CONTROL
/* Physical USB input is an explicit manual action. Keep it on the same bus
 * and policy path as touch; it cannot bypass runtime permissions or stop. */
static void usb_manual_task(void *context) {
    app_t *app = context;
    enum { COMMAND_BYTES = 4096 };
    char *command = heap_caps_malloc(COMMAND_BYTES, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!command) { ESP_LOGW(TAG, "No PSRAM for USB console"); vTaskDelete(NULL); return; }
    size_t length = 0;
    bool discard = false;
    ESP_LOGI(TAG, "Local USB bring-up control enabled: wake / stop");
    for (;;) {
        int byte = getchar();
        if (byte == EOF) {
            clearerr(stdin);
            vTaskDelay(pdMS_TO_TICKS(20));
            continue;
        }
        if (byte == '\r' || byte == '\n') {
            if (!discard && length) {
                command[length] = '\0';
                if (strncmp(command, "secure config ", 14) == 0) {
                    esp_err_t saved = brain_secure_provision(command + 14);
                    memset(command, 0, COMMAND_BYTES);
                    if (saved == ESP_OK) {
                        ESP_LOGI(TAG, "USB secure provisioning saved; restarting");
                        /* The normal logger is queued and can lag behind UART
                         * preview frames. A bounded direct receipt lets the
                         * physical provisioner commit its matching token hash. */
                        esp_rom_printf("\nXIAODAI_PROVISION_OK\n");
                        vTaskDelay(pdMS_TO_TICKS(1000));
                        esp_restart();
                    } else ESP_LOGW(TAG, "USB secure provisioning failed: %s", esp_err_to_name(saved));
                } else if (strncmp(command, "wifi config ", 12) == 0) {
                    esp_err_t saved = brain_link_wifi_provision(command + 12);
                    memset(command, 0, COMMAND_BYTES);
                    if (saved == ESP_OK) {
                        esp_rom_printf("\nXIAODAI_WIFI_OK\n");
                        ESP_LOGI(TAG, "USB Wi-Fi configuration saved; restarting");
                        vTaskDelay(pdMS_TO_TICKS(1000));
                        esp_restart();
                    } else {
                        esp_rom_printf("\nXIAODAI_WIFI_ERROR\n");
                        ESP_LOGW(TAG, "USB Wi-Fi configuration rejected: %s", esp_err_to_name(saved));
                    }
                } else if (strcmp(command, "wake") == 0) {
                    ESP_LOGI(TAG, "Intentional local USB wake received");
                    const bot_event_t event = {
                        .kind = BOT_EVENT_TOUCH_WAKE, .source = BOT_SOURCE_LOCAL_UI,
                        .observed_ms = bot_platform_now_ms(), .data.touch.intentional = true,
                    };
                    post(app, &event);
                } else if (strcmp(command, "stop") == 0) {
                    bot_bus_request_stop(app->bus, bot_platform_now_ms());
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
                } else if (strcmp(command, "preview on") == 0) {
                    bot_debug_set_preview(true);
                } else if (strcmp(command, "preview off") == 0) {
                    bot_debug_set_preview(false);
                } else if (strcmp(command, "preview ping") == 0) {
                    bot_debug_keepalive();
                } else if (strncmp(command, "touch ", 6) == 0) {
                    char *end = NULL;
                    const char *arg = command + 6;
                    const long x = strtol(arg, &end, 10);
                    const bool have_x = end != arg && *end == ' ';
                    const char *second = end;
                    const long y = strtol(second, &end, 10);
                    if (have_x && end != second && *end == '\0' &&
                        x >= 0 && x < 800 && y >= 0 && y < 480 &&
                        bot_debug_preview_enabled() && app->debug_touch_queue) {
                        const debug_touch_point_t point = { (uint16_t)x, (uint16_t)y };
                        xQueueOverwrite(app->debug_touch_queue, &point);
                    } else ESP_LOGW(TAG, "Touch test rejected: invalid point or preview inactive");
                } else if (strcmp(command, "screen frame") == 0 || strcmp(command, "screen snap") == 0) {
                    if (bot_debug_preview_enabled()) bot_debug_set_screen_snapshot(strcmp(command, "screen snap") == 0);
                    else ESP_LOGW(TAG, "Screen test rejected: USB preview is inactive");
                } else if (strcmp(command, "camera on") == 0 || strcmp(command, "camera off") == 0) {
                    int requested = strcmp(command, "camera on") == 0;
                    if (app->debug_camera_queue && (!requested || bot_debug_preview_enabled()))
                        xQueueOverwrite(app->debug_camera_queue, &requested);
                    else ESP_LOGW(TAG, "USB camera request rejected: preview is inactive");
                } else if (strncmp(command, "light test ", 11) == 0) {
                    char *end = NULL;
                    long value = -1;
                    const char *arg = command + 11;
                    if (strcmp(arg, "off") != 0) value = strtol(arg, &end, 10);
                    const bool valid = strcmp(arg, "off") == 0 ||
                        (end != arg && end && *end == '\0' && value >= 0 && value <= 255);
                    if (valid && bot_debug_preview_enabled() && app->debug_light_queue) {
                        const int requested = (int)value;
                        xQueueOverwrite(app->debug_light_queue, &requested);
                    } else ESP_LOGW(TAG, "Light simulation rejected: invalid value or preview inactive");
                } else if (strcmp(command, "camera orient?") == 0 ||
                           (strncmp(command, "camera orient ", 14) == 0 &&
                            command[14] >= '0' && command[14] <= '3' && command[15] == '\0')) {
                    int orientation = command[13] == '?' ? 4 : command[14] - '0';
                    if (app->debug_orientation_queue && (orientation == 4 || bot_debug_preview_enabled()))
                        xQueueOverwrite(app->debug_orientation_queue, &orientation);
                    else ESP_LOGW(TAG, "Camera orientation request rejected: USB preview is inactive");
                } else if (strncmp(command, "face ", 5) == 0) {
                    char name[BOT_EXPRESSION_MAX_BYTES + 1] = {0};
                    bool known = false;
                    for (int i = 0; i < FACE_EX_COUNT; i++) {
                        if (strcmp(command + 5, k_face_expression_ids[i]) == 0) { known = true; break; }
                    }
                    if (known && bot_debug_preview_enabled() && app->debug_face_queue) {
                        snprintf(name, sizeof(name), "%.*s", BOT_EXPRESSION_MAX_BYTES, command + 5);
                        xQueueOverwrite(app->debug_face_queue, name);
                    } else ESP_LOGW(TAG, "Face test rejected: unknown expression or preview inactive");
                } else if (strcmp(command, "eye left") == 0 || strcmp(command, "eye center") == 0 ||
                           strcmp(command, "eye right") == 0) {
                    int x = strcmp(command, "eye left") == 0 ? -100 :
                            strcmp(command, "eye right") == 0 ? 100 : 0;
                    if (bot_debug_preview_enabled() && app->debug_eye_queue) {
                        xQueueOverwrite(app->debug_eye_queue, &x);
                    } else ESP_LOGW(TAG, "Eye test rejected: USB preview is inactive");
#endif
                } else {
                    ESP_LOGW(TAG, "Unknown USB bring-up command");
                }
            }
            length = 0;
            discard = false;
        } else if (byte >= 0x20 && byte <= 0x7e && !discard && length < COMMAND_BYTES - 1) {
            command[length++] = (char)byte;
        } else {
            discard = true;
        }
    }
}
#endif
static void face_event(face_event_t event, void *context) {
    app_t *app = context;
    if (event == FACE_EVENT_TOUCH) { touch_wake(app); return; }
    if (event == FACE_EVENT_STOP) { bot_bus_request_stop(app->bus, bot_platform_now_ms()); return; }
    bot_event_t message = { .source = BOT_SOURCE_LOCAL_UI, .observed_ms = bot_platform_now_ms(),
        .kind = event == FACE_EVENT_RESULT_DISMISSED ? BOT_EVENT_RESULT_DISMISSED :
                event == FACE_EVENT_SETTINGS_OPENED ? BOT_EVENT_SETTINGS_OPEN : BOT_EVENT_SETTINGS_CLOSE };
    if (event == FACE_EVENT_SETTINGS_OPENED) {
        if (++app->settings_lease == 0) ++app->settings_lease;
        app->settings_revision = 0;
        app->settings_has_value = false;
    }
    message.settings_lease = app->settings_lease;
    if (event == FACE_EVENT_SETTINGS_CLOSED && app->settings_has_value) {
        message.settings_revision = app->settings_revision;
        message.settings_has_value = true;
        message.data.voice_settings = app->settings_last_value;
    }
    post(app, &message);
}
static void theme_applied(const char *theme, void *context) {
    app_t *app = context;
    char value[32] = {0};
    snprintf(value, sizeof(value), "%s", theme);
    if (app->theme_queue) xQueueOverwrite(app->theme_queue, value);
}
static bool sound_settings_request(const bot_voice_settings_t *settings, void *context) {
    app_t *app = context;
    if (!bot_voice_settings_valid(settings)) return false;
    const bot_event_t event = { .kind = BOT_EVENT_VOICE_SETTINGS, .source = BOT_SOURCE_LOCAL_UI,
        .observed_ms = bot_platform_now_ms(), .data.voice_settings = *settings,
        .settings_lease = app->settings_lease, .settings_revision = app->settings_revision + 1 };
    if (!event.settings_lease || !event.settings_revision ||
        bot_bus_post(app->bus, &event, 0) != ESP_OK) return false;
    app->settings_revision = event.settings_revision;
    app->settings_last_value = *settings;
    app->settings_has_value = true;
    return true;
}
static bool sound_preview_request(void *context) {
    app_t *app = context;
    const bot_event_t event = { .kind = BOT_EVENT_SOUND_PREVIEW, .source = BOT_SOURCE_LOCAL_UI,
        .observed_ms = bot_platform_now_ms(), .settings_lease = app->settings_lease };
    return bot_bus_post(app->bus, &event, 0) == ESP_OK;
}
static bool set_voice_theme(void *context, const char *theme) {
    app_t *app = context;
    if (!app->face || !bot_platform_display_lock(5)) return false;
    bool applied = face_set_theme(app->face, theme);
    bot_platform_display_unlock();
    if (applied) theme_applied(theme, app);
    return applied;
}
static void apply_audio_session(app_t *app, const bot_runtime_snapshot_t *state) {
    if (!app->audio) return;
    bot_audio_token_t token = audio_token(state->token);
    const bool listening = state->conversation == BOT_CONVERSATION_LISTENING ||
                           state->conversation == BOT_CONVERSATION_WAITING;
    const bool mac_session_possible = state->link_connected && state->link_authenticated && state->link_encrypted;
    const bot_audio_session_options_t options = {
        .deliver_pcm = listening && state->audio_uplink_permitted && brain_secure_uplink(&state->token),
        /* No pending network utterance exists when the Mac is offline. Keep
         * local commands enabled without an irrelevant 800 ms overflow limit. */
        .retain_start = listening && state->audio_session_requested && mac_session_possible,
        .waiting_for_followup = state->followup_armed,
        .local_commands = listening && state->audio_session_requested,
    };
    (void)bot_audio_set_session_ex(app->audio, state->audio_session_requested ? &token : NULL,
                                   state->audio_session_requested ? &options : NULL);
}
static void apply_state(void *context, const bot_runtime_snapshot_t *state) {
    app_t *app = context;
    app->desired = *state;
    if (app->audio) (void)bot_audio_set_controls(app->audio, &state->voice_settings);
    brain_secure_update(state);
    /* Also close the audio token gate at the intermediate revocation barrier. */
    apply_audio_session(app, state);
}
static void stop_output(void *context) {
    app_t *app = context;
    bot_response_player_cancel(app->response_player);
    if (app->audio) bot_audio_playback_cancel(app->audio, 0);
    head_motion_emergency_stop();
}
static bool play_wake_response(void *context, const bot_session_token_t *token,
                                const bot_response_entry_t *entry, uint8_t gain_percent) {
    app_t *app = context;
    return bot_response_player_queue(app->response_player, token, entry, gain_percent);
}
static bool play_sound_preview(void *context, const bot_session_token_t *token,
                                const bot_response_entry_t *entry, uint8_t gain_percent) {
    app_t *app = context;
    return bot_response_player_preview(app->response_player, token, entry, gain_percent);
}
static bool wake_response_tail_clear(void *context) {
    app_t *app = context;
    bot_audio_status_t status;
    bot_audio_get_status(app->audio, &status);
    return !status.self_playback_blocked;
}
static void cancel_remote(void *context, const bot_session_token_t *token) {
    (void)context; (void)token;
    /* The secure transport observes the invalidated snapshot and token; it
     * cancels the prior remote turn on its own nonblocking worker. */
}
static void command_result(void *context, uint32_t id, bot_command_result_t result) {
    (void)context;
    ESP_LOGI(TAG, "Command %lu: %s", (unsigned long)id, bot_command_result_name(result));
}
static uint32_t runtime_now_ms(void *context) {
    (void)context;
    return bot_platform_now_ms();
}

static void poll_night(app_t *app, uint32_t now_ms) {
    if (app->night_sent && (uint32_t)(now_ms - app->last_clock_poll_ms) < 30000) return;
    app->last_clock_poll_ms = now_ms;
    bool valid = false, night = false;
    time_t wall = time(NULL);
    struct tm local;
    valid = wall >= (time_t)1704067200 && localtime_r(&wall, &local) != NULL;
    uint8_t hour = valid ? (uint8_t)local.tm_hour : 0;
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
    if (valid) {
        int minute = local.tm_hour * 60 + local.tm_min;
        int start = CONFIG_ESPBOT_NIGHT_START_MINUTE, end = CONFIG_ESPBOT_NIGHT_END_MINUTE;
        night = bot_night_contains(minute, start, end);
    }
#endif
    if (!app->night_sent || app->last_night != night || app->last_clock_valid != valid ||
        app->last_local_hour != hour) {
        ESP_LOGI(TAG, "Night policy clock_valid=%d night=%d", valid, night);
        bot_event_t event = { .kind = BOT_EVENT_NIGHT_POLICY, .source = BOT_SOURCE_PLATFORM,
            .observed_ms = now_ms, .data.night = { .clock_valid = valid, .night = night, .local_hour = hour } };
        post(app, &event);
        app->night_sent = true; app->last_night = night; app->last_clock_valid = valid;
        app->last_local_hour = hour;
    }
}

static attention_output_t update_gaze(app_t *app, uint32_t now_ms) {
    const bot_focus_snapshot_t *focus = &app->desired.focus;
    if (app->desired.visual_tracking_permitted && focus->localization_valid) {
        float x = focus->eye_midpoint.x, y = focus->eye_midpoint.y;
#if CONFIG_ESPBOT_GAZE_MIRROR_X
        x = 1 - x;
#endif
#if CONFIG_ESPBOT_GAZE_MIRROR_Y
        y = 1 - y;
#endif
        const float radians = 0.01745329252f;
        attention_observation_t observation = {
            .timestamp_ms = focus->observed_ms, .track_id = focus->track_id,
            .person_yaw_from_camera_deg = atanf((2*x-1) * tanf(CONFIG_ESPBOT_CAMERA_FOV_X_DEG * radians / 2)) / radians,
            .person_pitch_from_camera_deg = atanf((1-2*y) * tanf(CONFIG_ESPBOT_CAMERA_FOV_Y_DEG * radians / 2)) / radians,
            .person_confidence = focus->confidence,
            .user_looking_at_robot = focus->gaze_evidence_valid && focus->looking_at_bot,
            .looking_confidence = focus->gaze_evidence_valid ? focus->observation.gaze_quality : 0,
        };
        bool calibrated = false;
#if CONFIG_ESPBOT_GAZE_CALIBRATED
        calibrated = true;
#endif
        attention_controller_observe_stationary(&app->attention, &observation, calibrated);
    } else {
        attention_controller_clear_target(&app->attention);
    }
    return attention_controller_step(&app->attention, NULL, now_ms, false);
}

static uint32_t bounded_counter(uint64_t value) {
    return value > UINT32_MAX ? UINT32_MAX : (uint32_t)value;
}
static void queue_preferences(app_t *app) {
    if (!bot_preferences_submit(&app->preferences))
        ESP_LOGW(TAG, "Preferences worker unavailable; change remains unsaved");
}

static uint32_t next_preference_revision(uint32_t revision) {
    return revision == UINT32_MAX ? 1 : revision + 1;
}

static void poll_preferences(app_t *app) {
    bot_preferences_result_t result;
    if (!bot_preferences_take_result(&result)) return;
    if (result.voice_revision != app->persisted.voice_revision &&
        result.voice_revision == app->preferences.voice_revision) {
        snprintf(app->voice_notice, sizeof(app->voice_notice), "%s", result.voice_error == ESP_OK ?
                 "声音设置已保存" : "本次已应用，未保存；下次修改时重试");
        if (result.voice_error != ESP_OK)
            ESP_LOGW(TAG, "Voice settings persistence: %s", esp_err_to_name(result.voice_error));
    }
    if (result.theme_revision != app->persisted.theme_revision && result.theme_error != ESP_OK)
        ESP_LOGW(TAG, "Appearance persistence: %s", esp_err_to_name(result.theme_error));
    if (result.orientation_revision != app->persisted.orientation_revision &&
        result.orientation_revision == app->preferences.orientation_revision) {
        if (result.orientation_error == ESP_OK) {
            app->camera_orientation = app->preferences.orientation;
            esp_err_t changed = app->vision ? bot_vision_set_sensor_orientation(app->vision, app->camera_orientation) : ESP_ERR_INVALID_STATE;
            ESP_LOGI(TAG, "Camera orientation current=%u saved=1", (unsigned)app->camera_orientation);
            if (changed != ESP_OK) ESP_LOGW(TAG, "Camera orientation saved; application pending: %s", esp_err_to_name(changed));
        } else ESP_LOGW(TAG, "Camera orientation not saved or changed: %s", esp_err_to_name(result.orientation_error));
    }
    app->persisted = result;
}

static void reconcile_voice(app_t *app, uint32_t now_ms) {
    const bot_runtime_snapshot_t *s = &app->desired;
    if (!bot_voice_settings_equal(&s->voice_settings, &app->voice_persist_attempt)) {
        app->preferences.voice = s->voice_settings;
        app->preferences.voice_revision = next_preference_revision(app->preferences.voice_revision);
        app->voice_persist_attempt = s->voice_settings; /* retry on next deliberate edit */
        snprintf(app->voice_notice, sizeof(app->voice_notice), "%s", bot_preferences_submit(&app->preferences) ?
                 "正在保存声音设置" : "本次已应用，未保存；下次修改时重试");
    }
    if (app->voice_preview_reported != s->sound_preview_active) {
        app->voice_preview_reported = s->sound_preview_active;
        snprintf(app->voice_notice, sizeof(app->voice_notice), "%s", s->sound_preview_active ?
                 "正在试听内置应答" : "试听已结束");
    }
    if ((uint32_t)(now_ms - app->last_voice_diagnostic_ms) < 500) return;
    app->last_voice_diagnostic_ms = now_ms;
    bot_audio_status_t audio = {0};
    if (app->audio) bot_audio_get_status(app->audio, &audio);
    const bool mic_available = audio.capture_running && audio.capture_healthy;
    if (mic_available != s->audio_available) {
        const bot_event_t health = {.kind=BOT_EVENT_AUDIO_HEALTH,.source=BOT_SOURCE_AUDIO,
            .observed_ms=now_ms,.data.health={.available=mic_available,.error=audio.last_error}};
        post(app,&health);
    }
    const bot_wake_status_t wake = !mic_available ? BOT_WAKE_UNAVAILABLE :
        audio.wake_status == BOT_AUDIO_WAKE_READY ? BOT_WAKE_READY :
        audio.wake_status == BOT_AUDIO_WAKE_MODEL_MISSING ? BOT_WAKE_MODEL_MISSING : BOT_WAKE_UNAVAILABLE;
    if (wake != s->wake_status) {
        const bot_event_t status = {.kind=BOT_EVENT_WAKE_STATUS,.source=BOT_SOURCE_AUDIO,
            .observed_ms=now_ms,.data.wake_status={.status=wake}};
        post(app,&status);
    }
    /* A queue overflow can lose a change-only health event. Re-read the owner
     * after recovery rather than restoring a session or guessing audio health. */
    if (app->vision) {
        bot_vision_status_t vision;
        bot_vision_get_status(app->vision,&vision);
        const bool available = vision.stream_active && vision.observation_valid;
        if (available != s->camera_available) {
            const bot_event_t health = {.kind=BOT_EVENT_CAMERA_HEALTH,.source=BOT_SOURCE_VISION,
                .observed_ms=now_ms,.data.health={.available=available,.error=vision.error}};
            post(app,&health);
        }
    }
    if (app->audio && audio.capture_running && !audio.afe_running && !s->audio_session_requested &&
        !s->sound_preview_active && (uint32_t)(now_ms - app->last_audio_retry_ms) >= 30000) {
        app->last_audio_retry_ms = now_ms;
        if (bot_audio_retry_afe(app->audio) == ESP_OK) ESP_LOGI(TAG, "Requested slow AFE recovery");
    }
    app->voice_diagnostics = (bot_voice_diagnostics_t){
        .mic_available = s->audio_available && audio.capture_running,
        .model_ready = audio.wake_status == BOT_AUDIO_WAKE_READY,
        .capture_ready = s->capture_ready, .link_ready = s->transport_ready,
        .playback_blocked = audio.self_playback_blocked,
        .settings_applied = audio.controls_error == ESP_OK &&
            audio.controls_revision == audio.controls_applied_revision &&
            bot_voice_settings_equal(&audio.controls_applied, &s->voice_settings),
        .wake_score_available = audio.last_wake_score_available,
        .peak0 = audio.microphone_peak, .peak1 = audio.microphone_peak_2,
        .last_wake_score = audio.last_wake_score,
        .capture_frames = bounded_counter(audio.capture_frames),
        .read_errors = bounded_counter(audio.capture_errors),
        .playback_underruns = bounded_counter(audio.playback_underruns),
        .buffered_samples = audio.buffered_samples, .buffer_high_water = audio.buffer_high_water,
        .buffered_drops = bounded_counter(audio.buffered_drops),
        .wake_count = bounded_counter(audio.wake_detections),
        .local_command_count = bounded_counter(audio.local_commands),
        .last_capture_ready_ms = (uint32_t)(audio.last_capture_ready_us / 1000),
        .last_error = audio.controls_error != ESP_OK ? audio.controls_error : audio.last_error,
    };
}

static void reconcile(app_t *app, uint32_t now_ms) {
    bot_runtime_snapshot_t *s = &app->desired;
    poll_preferences(app);
    reconcile_voice(app, now_ms);
    apply_audio_session(app, s);
    bool camera_requested = s->camera_requested;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    int orientation_command;
    if (app->debug_orientation_queue && xQueueReceive(app->debug_orientation_queue, &orientation_command, 0) == pdTRUE) {
        if (orientation_command == 4) {
            ESP_LOGI(TAG, "Camera orientation current=%u", (unsigned)app->camera_orientation);
        } else {
            app->preferences.orientation = (uint8_t)orientation_command;
            app->preferences.orientation_revision = next_preference_revision(app->preferences.orientation_revision);
            queue_preferences(app);
        }
    }
    int debug_camera_command;
    const bool usb_preview = bot_debug_preview_enabled();
    if (app->debug_camera_queue && xQueueReceive(app->debug_camera_queue, &debug_camera_command, 0) == pdTRUE) {
        const bool wanted = debug_camera_command && usb_preview;
        if (wanted != app->debug_camera_wanted)
            ESP_LOGI(TAG, "Supervised USB camera preview requested=%d", wanted);
        app->debug_camera_wanted = wanted;
    }
    if (!usb_preview) app->debug_camera_wanted = false;
    /* An explicit supervised local request can keep perception running while
     * idle, without creating a conversation or enabling network media. Night
     * dark, settings and critical faults still suppress debug capture. */
    const bool debug_camera_active = app->debug_camera_wanted && usb_preview &&
        s->behavior != BOT_BEHAVIOR_NIGHT_DARK && s->behavior != BOT_BEHAVIOR_SLEEPING &&
        !s->settings_open && !s->critical_fault;
    camera_requested = camera_requested || debug_camera_active;
#endif
    const bool light_only = s->behavior == BOT_BEHAVIOR_SLEEPING && s->ambient_probe;
    if (app->vision && (!app->light_mode_sent || app->last_light_mode != light_only)) {
        if (bot_vision_set_light_only(app->vision, light_only) == ESP_OK) {
            app->last_light_mode = light_only;
            app->light_mode_sent = true;
            ESP_LOGI(TAG, "Ambient probe light_only=%d", light_only);
        }
    }
    if (app->vision && (!app->camera_request_sent || app->last_camera_requested != camera_requested)) {
        if (bot_vision_set_enabled(app->vision, camera_requested) == ESP_OK) {
            ESP_LOGI(TAG, "Camera requested=%d behavior=%s", camera_requested,
                     bot_behavior_name(s->behavior));
            app->last_camera_requested = camera_requested;
            app->camera_request_sent = true;
        }
    }
    if (!app->ambient_reported || app->last_ambient_dark != s->ambient_dark ||
        app->last_ambient_probe != s->ambient_probe) {
        ESP_LOGI(TAG, "Ambient dark=%d probe=%d luma=%u valid=%d behavior=%s",
                 s->ambient_dark, s->ambient_probe, s->ambient_luma,
                 s->ambient_sample_valid, bot_behavior_name(s->behavior));
        app->ambient_reported = true;
        app->last_ambient_dark = s->ambient_dark;
        app->last_ambient_probe = s->ambient_probe;
    }
    bot_platform_status_t platform;
    bot_platform_get_status(app->platform, &platform);
    if (platform.display_available && platform.display_awake != s->display_awake_requested) {
        bot_platform_set_display_awake(app->platform, s->display_awake_requested);
        bot_platform_get_status(app->platform, &platform);
    }
    if (!app->display_health_sent || app->last_display_awake != platform.display_awake) {
        bot_event_t event = { .kind = BOT_EVENT_DISPLAY_HEALTH, .source = BOT_SOURCE_PLATFORM,
            .observed_ms = now_ms, .data.display = { .awake = platform.display_available && platform.display_awake,
                .sleep_supported = platform.backlight_control_supported,
                .full_blackout_verified = platform.full_blackout_verified, .error = platform.display_error } };
        post(app, &event);
        app->last_display_awake = platform.display_awake; app->display_health_sent = true;
    }
    attention_output_t gaze = update_gaze(app, now_ms);
    const char *gaze_source = gaze.person_tracked ? "face" : "center";
    const bool near_face = s->near_face && !s->settings_open && !s->critical_fault;
    if (!app->near_face_reported || app->last_near_face != near_face) {
        ESP_LOGI(TAG, "Near-face reaction=%d face_area=%u.%u%% camera_available=%d", near_face,
                 (unsigned)s->near_face_area_permille / 10,
                 (unsigned)s->near_face_area_permille % 10, s->camera_available);
        app->near_face_reported = true;
        app->last_near_face = near_face;
    }
    const bool idle_look = s->behavior == BOT_BEHAVIOR_IDLE && !near_face && !s->settings_open &&
                           !s->critical_fault && platform.display_awake;
    if (idle_attention_step(&app->idle_attention, now_ms, idle_look, &gaze.eye_x, &gaze.eye_y))
        ESP_LOGI(TAG, "Idle glance target=(%d,%d)", gaze.eye_x, gaze.eye_y);
    if (idle_look) gaze_source = "idle";
    if (s->sound_reaction && !near_face && !s->settings_open && !s->critical_fault &&
        platform.display_awake) {
        gaze.eye_x = s->sound_direction_valid ? attention_sound_eye_x(s->sound_azimuth_deg) : 0;
        gaze.eye_y = 0;
        gaze_source = s->sound_direction_valid ? "sound" : "sound_unlocated";
    }
    if (near_face) { gaze.eye_x = 0; gaze.eye_y = 0; gaze_source = "near_face"; }
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    const bool eye_test_allowed = bot_debug_preview_enabled() && platform.display_awake &&
        !s->critical_fault && !s->settings_open && s->behavior != BOT_BEHAVIOR_NIGHT_DARK;
    int debug_eye_x;
    if (app->debug_eye_queue && xQueueReceive(app->debug_eye_queue, &debug_eye_x, 0) == pdTRUE) {
        if (eye_test_allowed) {
            app->debug_eye_x = debug_eye_x;
            app->debug_eye_until_ms = now_ms + 10000;
            ESP_LOGI(TAG, "USB eye test requested x=%d for 10 s", debug_eye_x);
        } else ESP_LOGW(TAG, "Eye test rejected: display unavailable or settings open");
    }
    if (eye_test_allowed && app->debug_eye_until_ms && (int32_t)(app->debug_eye_until_ms - now_ms) > 0) {
        gaze.eye_x = app->debug_eye_x; gaze.eye_y = 0;
        gaze_source = "manual_eye";
    } else app->debug_eye_until_ms = 0;
#endif
    const char *expression = near_face && !s->local_response_active ? "happy" : s->expression;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    const bool face_test_allowed = bot_debug_preview_enabled() && platform.display_awake &&
        !s->critical_fault && !s->settings_open && s->behavior != BOT_BEHAVIOR_NIGHT_DARK;
    char debug_face[BOT_EXPRESSION_MAX_BYTES + 1];
    if (app->debug_face_queue && xQueueReceive(app->debug_face_queue, debug_face, 0) == pdTRUE) {
        if (face_test_allowed) {
            memcpy(app->debug_face, debug_face, sizeof(app->debug_face));
            app->debug_face_until_ms = now_ms + 8000;
            ESP_LOGI(TAG, "USB face test requested expression=%s for 8 s", app->debug_face);
        } else ESP_LOGW(TAG, "Face test rejected: display unavailable or settings open");
    }
    if (face_test_allowed && app->debug_face_until_ms && (int32_t)(app->debug_face_until_ms - now_ms) > 0) {
        expression = app->debug_face;
        gaze.eye_x = gaze.eye_y = 0;
        gaze_source = "manual_expression";
    } else app->debug_face_until_ms = 0;
#endif
    bool expression_changed = false, rendered_gaze_valid = false;
    int rendered_gaze_x = 0, rendered_gaze_y = 0;
    if (app->face && bot_platform_display_lock(5)) {
        face_set_sound_settings(app->face, &s->voice_settings);
        face_set_sound_diagnostics(app->face, &app->voice_diagnostics);
        face_set_sound_notice(app->face, app->voice_notice);
        face_set_animation_active(app->face, platform.display_awake);
        if (face_set_expression(app->face, expression) &&
            strcmp(app->rendered_expression, expression) != 0) {
            snprintf(app->rendered_expression, sizeof(app->rendered_expression), "%s", expression);
            expression_changed = true;
        }
        face_set_gaze(app->face, gaze.eye_x, gaze.eye_y);
        face_get_gaze(app->face, &rendered_gaze_x, &rendered_gaze_y);
        rendered_gaze_valid = true;
        face_set_stop_visible(app->face, s->behavior == BOT_BEHAVIOR_WAKING ||
                                      s->behavior == BOT_BEHAVIOR_ENGAGED || s->reading_token.session_id || s->critical_fault);
        const char *caption = s->caption;
        if (s->critical_fault) caption = s->fault_recoverable ? "请点击停止，再唤醒小呆" : "小呆需要重新启动";
        if ((!*caption || strcmp(caption, "请说") == 0) && s->audio_session_requested &&
            (s->conversation == BOT_CONVERSATION_LISTENING || s->conversation == BOT_CONVERSATION_WAITING)) {
            if (s->capture_ready && s->transport_ready)
                caption = s->followup_armed ? "可以继续说" : "我在听";
            else if (!s->capture_ready) caption = "正在准备收音";
            else if (!s->link_authenticated) caption = "本地指令可用，Mac 尚未连接";
            else caption = "正在连接 Mac，请稍等";
        }
        if (!*caption && s->wake_status != BOT_WAKE_READY && !s->settings_open &&
            (s->behavior == BOT_BEHAVIOR_IDLE || s->behavior == BOT_BEHAVIOR_BORED))
            caption = "点一下，叫醒小呆";
        if (!*caption && s->behavior == BOT_BEHAVIOR_ENGAGED && !s->link_authenticated)
            caption = "我在这里，语音对话还没接通";
        if (strcmp(app->rendered_caption, caption) != 0) {
            face_set_subtitle(app->face, caption);
            snprintf(app->rendered_caption, sizeof(app->rendered_caption), "%s", caption);
        }
        bool showing_result = s->reading_token.session_id && !s->settings_open && !s->critical_fault;
        if (!showing_result) {
            face_set_result_text(app->face,"");
            app->result_token=(bot_session_token_t){0};
            app->image_token=(bot_session_token_t){0};
            clear_display_receipts(app);
        } else if (!same_runtime_token(app->result_token,s->reading_token) ||
                   app->result_sequence != s->reading_sequence) {
            clear_display_receipts(app);
            face_set_result_text(app->face,"");
            if (s->reading_sequence) face_set_result_segment(app->face,s->reading_text);
            else face_set_result_text(app->face,s->reading_text);
            app->result_token=s->reading_token;
            app->result_sequence=s->reading_sequence;
            app->image_token=(bot_session_token_t){0};
            if (!++app->text_generation) ++app->text_generation;
            atomic_store(&app->text_requested,app->text_generation);
            lv_obj_invalidate(face_root(app->face));
        }
        if (showing_result) {
            const char *status=s->reading_notice;
            if (s->token.session_id && !same_runtime_token(s->reading_token,s->token)) {
                status=caption;
                if (!*status && s->conversation==BOT_CONVERSATION_LISTENING) status="我在听";
                if (!*status && s->conversation==BOT_CONVERSATION_THINKING) status="正在处理";
            }
            face_set_result_status(app->face,status,s->image_notice);
            if (!same_runtime_token(s->reading_token,s->token) && !s->followup_armed)
                clear_display_receipts(app);
        }
        bot_display_image_t image;
        if (brain_secure_take_image(&image)) {
            bool current=showing_result && same_runtime_token(image.token,s->reading_token) &&
                image.presentation_sequence == s->reading_sequence &&
                (s->followup_armed || same_runtime_token(image.token,s->token));
            if (current && face_set_result_image(app->face,image.pixels,image.width,image.height)) {
                app->image_token=image.token;
                app->image_sequence=image.presentation_sequence;
                snprintf(app->image_asset_id,sizeof(app->image_asset_id),"%s",image.asset_id);
                if (!++app->image_generation) ++app->image_generation;
                atomic_store(&app->image_requested,app->image_generation);
                lv_obj_invalidate(face_root(app->face));
            } else {
                free(image.pixels);
                if (current) brain_secure_image_failed(image.token,image.asset_id,"display_unavailable");
            }
        }
        unsigned text_ready=atomic_exchange(&app->text_ready,0);
        unsigned image_ready=atomic_exchange(&app->image_ready,0);
        if (text_ready && text_ready==app->text_generation && showing_result && platform.display_awake &&
            same_runtime_token(app->result_token,s->reading_token) &&
            (s->followup_armed || same_runtime_token(app->result_token,s->token)) && face_result_visible(app->face) &&
            !brain_secure_display_report_sequence(app->result_token,app->result_sequence,"text",NULL,true,NULL)) atomic_store(&app->text_ready,text_ready);
        if (image_ready && image_ready==app->image_generation && showing_result && platform.display_awake &&
            same_runtime_token(app->image_token,s->reading_token) &&
            (s->followup_armed || same_runtime_token(app->image_token,s->token)) && face_result_visible(app->face) &&
            !brain_secure_display_report_sequence(app->image_token,app->image_sequence,"image",app->image_asset_id,true,NULL)) atomic_store(&app->image_ready,image_ready);
        bot_platform_display_unlock();
    }
    if (expression_changed)
        ESP_LOGI(TAG, "Face expression=%s near_face=%d gaze=(%d,%d)", expression, near_face,
                 gaze.eye_x, gaze.eye_y);
    char theme[32];
    if (xQueueReceive(app->theme_queue, theme, 0) == pdTRUE) {
        memcpy(app->preferences.theme, theme, sizeof(theme));
        app->preferences.theme_revision = next_preference_revision(app->preferences.theme_revision);
        queue_preferences(app);
    }
    const uint32_t diagnostic_interval_ms = camera_requested ? 2000 : 15000;
    if ((uint32_t)(now_ms - app->last_diagnostic_ms) >= diagnostic_interval_ms) {
        app->last_diagnostic_ms = now_ms;
        bot_bus_stats_t stats;
        bot_bus_get_stats(app->bus, &stats);
        ESP_LOGI(TAG, "runtime stack_free=%u", (unsigned)uxTaskGetStackHighWaterMark(NULL));
        /* No images, PCM, transcript, person name, or biometric template in logs. */
        ESP_LOGI(TAG, "mode=%s conversation=%s wake=%d audio=%d camera=%d secure_link=%d fault=%d dropped=%lu",
                 bot_behavior_name(s->behavior), bot_conversation_name(s->conversation), s->wake_status,
                 s->audio_available, s->camera_available, s->link_authenticated && s->link_encrypted,
                 s->critical_fault, (unsigned long)stats.normal_events_dropped);
        if (app->vision) {
            bot_vision_status_t vision;
            bot_vision_get_status(app->vision, &vision);
            ESP_LOGI(TAG, "vision stack_free=%lu", (unsigned long)vision.stack_free);
            ESP_LOGI(TAG, "vision=%s quality=%s enabled=%d error=%s frames=%lu published=%lu inference_ms=%lu luma=%u model_faces=%u rejected=%u faces=%u",
                     bot_vision_state_name(vision.state), bot_vision_quality_name(vision.quality),
                     vision.enabled_requested, esp_err_to_name(vision.error),
                     (unsigned long)vision.frames_seen, (unsigned long)vision.frames_published,
                     (unsigned long)vision.inference_ms, vision.mean_luma,
                     vision.model_faces, vision.rejected_faces, vision.faces_detected);
            ESP_LOGI(TAG, "focus=%s valid=%d tracking=%d idle_look=%d gaze=(%d,%d) near_face=%d face_area=%u.%u%% rejected_events=%lu",
                     attention_manager_state_name(s->focus.state), s->focus.localization_valid,
                     s->visual_tracking_permitted, idle_look, gaze.eye_x, gaze.eye_y, near_face,
                     (unsigned)s->near_face_area_permille / 10,
                     (unsigned)s->near_face_area_permille % 10,
                     (unsigned long)s->rejected_events);
            const bool valid = s->focus.localization_valid;
            const int focus_x = valid ? (int)(s->focus.eye_midpoint.x * 1000.0f + 0.5f) : -1;
            const int focus_y = valid ? (int)(s->focus.eye_midpoint.y * 1000.0f + 0.5f) : -1;
            ESP_LOGI(TAG, "gaze_trace source=%s focus_milli=(%d,%d) focus_age_ms=%lu target=(%d,%d) rendered=(%d,%d) rendered_valid=%d",
                     gaze_source, focus_x, focus_y,
                     (unsigned long)(valid ? (uint32_t)(now_ms - s->focus.observed_ms) : 0),
                     gaze.eye_x, gaze.eye_y, rendered_gaze_x, rendered_gaze_y, rendered_gaze_valid);
        }
        if (app->audio) {
            bot_audio_status_t audio;
            bot_audio_get_status(app->audio, &audio);
            ESP_LOGI(TAG, "audio stack_free capture=%lu output=%lu wake_reply=%lu onset_buffer=%d",
                     (unsigned long)audio.capture_stack_free, (unsigned long)audio.playback_stack_free,
                     (unsigned long)bot_response_player_stack_free(app->response_player), audio.start_buffer_available);
            ESP_LOGI(TAG, "audio_capture=%llu afe_frames=%llu afe_errors=%llu vad=%d mic_peaks=(%u,%u) means=(%u,%u) noise=%u active=%d speech=%d doa=%d valid=%d az=%.1f quality=%.1f sound_events=%llu estimates=%llu uplink=%d",
                     (unsigned long long)audio.capture_frames, (unsigned long long)audio.afe_frames,
                     (unsigned long long)audio.afe_errors, audio.vad_available,
                     audio.microphone_peak, audio.microphone_peak_2, audio.microphone_mean,
                     audio.microphone_mean_2, audio.ambient_noise_mean, audio.ambient_activity, audio.ambient_speech,
                     audio.direction_available, audio.direction_valid, audio.azimuth_deg,
                     audio.direction_quality, (unsigned long long)audio.ambient_events,
                     (unsigned long long)audio.direction_estimates, audio.pcm_delivery_enabled);
            ESP_LOGI(TAG, "voice model=%s ready=(mic:%d link:%d) threshold=%u muted=%d volume=%u score=%s%.3f commands=%llu buffer=(%lu/%lu dropped:%llu) controls=(%lu/%lu err:%s)",
                     audio.model_name, s->capture_ready, s->transport_ready,
                     audio.controls_applied.wake_threshold_percent, audio.controls_applied.muted,
                     audio.controls_applied.volume_percent, audio.last_wake_score_available ? "" : "unavailable:",
                     (double)audio.last_wake_score, (unsigned long long)audio.local_commands,
                     (unsigned long)audio.buffered_samples, (unsigned long)audio.buffer_high_water,
                     (unsigned long long)audio.buffered_drops,
                     (unsigned long)audio.controls_applied_revision, (unsigned long)audio.controls_revision,
                     esp_err_to_name(audio.controls_error));
            ESP_LOGI(TAG, "keyword runs=%llu timeouts=%llu rejected=%llu candidate=(%d %.3f) dbfs=%.1f input_peak=%u spoken=%d",
                     (unsigned long long)audio.keyword_runs, (unsigned long long)audio.keyword_timeouts,
                     (unsigned long long)audio.keyword_rejections, audio.last_candidate_command,
                     (double)audio.last_candidate_score, (double)audio.keyword_dbfs, audio.keyword_input_peak,
                     audio.controls_applied.spoken_answers);
            ESP_LOGI(TAG, "audio timing_us read=%lu/%lu afe=%lu/%lu keyword=%lu/%lu processing=%lu/%lu output_queue_high=%lu underruns=%llu",
                     (unsigned long)audio.read_us, (unsigned long)audio.read_max_us,
                     (unsigned long)audio.afe_us, (unsigned long)audio.afe_max_us,
                     (unsigned long)audio.keyword_us, (unsigned long)audio.keyword_max_us,
                     (unsigned long)audio.processing_us, (unsigned long)audio.processing_max_us,
                     (unsigned long)audio.output_queue_high_water, (unsigned long long)audio.playback_underruns);
        }
    }
}

static void poll_ambient_light(app_t *app, uint32_t now_ms) {
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    int test_luma;
    if (app->debug_light_queue && xQueueReceive(app->debug_light_queue, &test_luma, 0) == pdTRUE) {
        app->simulated_light = test_luma >= 0;
        app->simulated_luma = test_luma;
        app->simulated_light_until_ms = now_ms + 120000;
        ESP_LOGI(TAG, "Ambient simulation enabled=%d luma=%d (temporary)",
                 app->simulated_light, test_luma);
    }
    if (app->simulated_light && (!bot_debug_preview_enabled() ||
        (int32_t)(now_ms - app->simulated_light_until_ms) >= 0)) {
        app->simulated_light = false;
        ESP_LOGI(TAG, "Ambient simulation expired");
    }
#endif
    if (!app->vision || !app->desired.camera_requested ||
        (uint32_t)(now_ms - app->last_light_poll_ms) < 500) return;
    app->last_light_poll_ms = now_ms;
    bot_vision_status_t status;
    bot_vision_get_status(app->vision, &status);
    if (!status.stream_active || status.error != ESP_OK ||
        (status.quality != BOT_VISION_FRAME_OK && status.quality != BOT_VISION_FRAME_DARK)) return;
    uint8_t luma = status.mean_luma;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    if (app->simulated_light) luma = (uint8_t)app->simulated_luma;
#endif
    bot_runtime_observe_light(app->runtime, luma, status.capture_ms, bot_platform_now_ms());
}

static void runtime_task(void *argument) {
    app_t *app = argument;
    TickType_t previous = xTaskGetTickCount();
    for (;;) {
        uint32_t now = bot_platform_now_ms();
        poll_night(app, now);
        poll_ambient_light(app, now);
        bot_runtime_step(app->runtime, now);
        reconcile(app, bot_platform_now_ms());
        vTaskDelayUntil(&previous, pdMS_TO_TICKS(20));
    }
}

/* Reserve the local behavior/Stop task's internal stack at link time; its
 * start still waits for initialization to finish. Optional clients cannot take it. */
static StackType_t s_runtime_stack[12288 / sizeof(StackType_t)] __attribute__((aligned(portBYTE_ALIGNMENT)));
static StaticTask_t s_runtime_tcb;

void app_main(void) {
    ESP_LOGI(TAG, "Boot reset_reason=%d; normal operation does not rewrite application flash",
             (int)esp_reset_reason());
    app_t *app = calloc(1, sizeof(*app));
    if (!app) { ESP_LOGE(TAG, "Cannot allocate application"); return; }
    idle_attention_init(&app->idle_attention, esp_random());
    bot_bus_config_t bus_cfg = bot_bus_default_config();
    ESP_ERROR_CHECK(bot_bus_create(&bus_cfg, &app->bus));
    ESP_ERROR_CHECK(bot_platform_create(&app->platform));
    esp_err_t preferences_error = bot_preferences_start(app->platform);
    if (preferences_error != ESP_OK)
        ESP_LOGW(TAG, "Preferences worker unavailable: %s", esp_err_to_name(preferences_error));
    app->theme_queue = xQueueCreate(1, 32);
    if (!app->theme_queue) { ESP_LOGE(TAG, "Cannot allocate preferences queue"); return; }
    bot_runtime_config_t config = bot_runtime_default_config();
    esp_err_t voice_loaded = bot_platform_load_voice_settings(app->platform, &config.voice_settings);
    if (voice_loaded != ESP_OK && voice_loaded != ESP_ERR_NOT_FOUND)
        ESP_LOGW(TAG, "Voice settings invalid/unavailable; defaults used: %s", esp_err_to_name(voice_loaded));
    app->voice_persist_attempt = config.voice_settings;
    attention_config_t attention_cfg = attention_default_config();
    attention_cfg.person_min_confidence = config.focus.min_face_confidence;
    attention_controller_init(&app->attention, &attention_cfg);
    /* Coordinate mode is fixed before the first observation; no pose sensor exists. */
    app->attention.camera_relative = true;
    bot_platform_status_t platform;
    bot_platform_get_status(app->platform, &platform);
    if (platform.display_available && bot_platform_display_lock(100)) {
        app->face = face_create(bot_platform_screen(app->platform));
        if (app->face) {
            face_on_result_draw(app->face,result_drawn,app);
            lv_display_t *display=lv_display_get_default();
            if (display) {
                lv_display_add_event_cb(display,display_refresh,LV_EVENT_REFR_START,app);
                lv_display_add_event_cb(display,display_refresh,LV_EVENT_REFR_READY,app);
                lv_display_add_event_cb(display,display_refresh,LV_EVENT_FLUSH_WAIT_FINISH,app);
            }
            char theme[32];
            if (bot_platform_load_theme(app->platform, theme, sizeof(theme)) == ESP_OK)
                face_set_theme(app->face, theme);
            face_on_theme_applied(app->face, theme_applied, app);
            face_on_event(app->face, face_event, app);
            face_on_sound_settings_request(app->face, sound_settings_request, app);
            face_on_sound_preview_request(app->face, sound_preview_request, app);
            face_set_sound_settings(app->face, &config.voice_settings);
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
            app->debug_touch_queue = xQueueCreate(1, sizeof(debug_touch_point_t));
            lv_indev_t *debug_touch = app->debug_touch_queue ? lv_indev_create() : NULL;
            if (debug_touch) {
                lv_indev_set_type(debug_touch, LV_INDEV_TYPE_POINTER);
                lv_indev_set_display(debug_touch, display);
                lv_indev_set_user_data(debug_touch, app);
                lv_indev_set_read_cb(debug_touch, debug_touch_read);
            } else ESP_LOGW(TAG, "USB touch test unavailable: input allocation failed");
#endif
        }
        bot_platform_on_dark_touch(app->platform, touch_wake, app);
        bot_platform_display_unlock();
    }
    config.allow_audio_uplink = true; /* runtime still requires authenticated WSS and explicit activation */
#if CONFIG_BOT_AUDIO_AEC_BARGE_IN
    config.allow_barge_in = true; /* audio owner still requires live hardware reference gates */
#else
    config.allow_barge_in = false;
#endif
    bot_runtime_ops_t ops = { .apply_state = apply_state, .stop_output = stop_output,
        .cancel_remote = cancel_remote, .command_result = command_result,
        .now_ms = runtime_now_ms, .play_wake_response = play_wake_response,
        .wake_response_tail_clear = wake_response_tail_clear,
        .play_sound_preview = play_sound_preview, .set_theme = set_voice_theme };
    ESP_ERROR_CHECK(bot_runtime_create(app->bus, &config, &ops, app, bot_platform_now_ms(), &app->runtime));
    bot_runtime_get_snapshot(app->runtime, &app->desired);
    setenv("TZ", CONFIG_ESPBOT_TIMEZONE, 1);
    tzset();
    bot_audio_config_t audio_cfg = bot_audio_default_config();
    audio_cfg.output_volume = config.voice_settings.muted ? 0 : config.voice_settings.volume_percent;
    audio_cfg.on_event = audio_event; audio_cfg.on_pcm = audio_pcm; audio_cfg.context = app;
    esp_err_t err = bot_audio_create(&audio_cfg, &app->audio);
    if (err == ESP_OK) err = bot_audio_set_controls(app->audio, &config.voice_settings);
    if (err == ESP_OK) err = bot_audio_start(app->audio);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "Audio degraded: %s; touch remains available", esp_err_to_name(err));
        bot_event_t event = { .kind = BOT_EVENT_AUDIO_HEALTH, .source = BOT_SOURCE_AUDIO,
            .observed_ms = bot_platform_now_ms(), .data.health = { .available = false, .error = err } };
        post(app, &event);
    }
    if (app->audio) {
        esp_err_t reply_error = bot_response_player_create(app->bus, app->audio, &app->response_player);
        if (reply_error != ESP_OK)
            ESP_LOGW(TAG, "Local wake speech unavailable: %s", esp_err_to_name(reply_error));
        else ESP_LOGI(TAG, "Embedded wake responses ready: %u", (unsigned)bot_responses_count());
    }
    /* Reserve the persistent authenticated client task while internal RAM is
     * contiguous. Camera, USB preview and Bonjour must not be able to displace it. */
    err = brain_link_start(app->bus);
    if (err != ESP_OK) ESP_LOGW(TAG, "Network unavailable: %s; local runtime continues", esp_err_to_name(err));
    if (err == ESP_OK) {
        if (app->audio) {
            esp_err_t secure = brain_secure_start(app->bus, app->audio);
            if (secure != ESP_OK) ESP_LOGW(TAG, "Secure gateway unavailable: %s", esp_err_to_name(secure));
        }
        brain_link_start_diagnostics();
    }
    bot_vision_config_t vision_cfg = bot_vision_default_config();
    vision_cfg.prepare_camera = prepare_camera; vision_cfg.on_snapshot = vision_snapshot;
    vision_cfg.on_status = vision_status; vision_cfg.context = app; vision_cfg.start_enabled = false;
    /* Current board orientation was verified upright; the user reported its
     * horizontal axis reversed. NVS overrides this first-boot board default. */
#if CONFIG_ESPBOT_CAMERA_ROTATE_180
    app->camera_orientation = 3;
#else
    app->camera_orientation = 1;
#endif
    (void)bot_platform_load_camera_orientation(app->platform, &app->camera_orientation);
    vision_cfg.rotate_180 = false; /* sensor control corrects both consumers */
    vision_cfg.sensor_orientation = app->camera_orientation;
    ESP_LOGI(TAG, "Camera orientation current=%u", (unsigned)app->camera_orientation);
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    vision_cfg.on_preview = vision_preview;
    app->debug_eye_queue = xQueueCreate(1, sizeof(int));
    app->debug_camera_queue = xQueueCreate(1, sizeof(int));
    app->debug_face_queue = xQueueCreate(1, BOT_EXPRESSION_MAX_BYTES + 1);
    app->debug_orientation_queue = xQueueCreate(1, sizeof(int));
    app->debug_light_queue = xQueueCreate(1, sizeof(int));
    if (!app->debug_eye_queue) ESP_LOGW(TAG, "USB eye test unavailable: queue allocation failed");
    if (!app->debug_camera_queue) ESP_LOGW(TAG, "USB camera test unavailable: queue allocation failed");
    if (!app->debug_face_queue) ESP_LOGW(TAG, "USB face test unavailable: queue allocation failed");
    if (!app->debug_orientation_queue) ESP_LOGW(TAG, "USB camera orientation unavailable: queue allocation failed");
    if (!app->debug_light_queue) ESP_LOGW(TAG, "USB light simulation unavailable: queue allocation failed");
    err = bot_debug_start(app->platform);
    if (err != ESP_OK) ESP_LOGW(TAG, "USB preview unavailable: %s", esp_err_to_name(err));
#endif
#if CONFIG_ESPBOT_GAZE_CALIBRATED
    vision_cfg.calibration_valid = true;
#endif
    err = bot_vision_start(&vision_cfg, &app->vision);
    if (err != ESP_OK) ESP_LOGW(TAG, "Vision unavailable: %s", esp_err_to_name(err));
    if (!xTaskCreateStatic(runtime_task, "bot_runtime", sizeof(s_runtime_stack), app, 6, s_runtime_stack, &s_runtime_tcb)) {
        stop_output(app);
        if (app->audio) bot_audio_set_session(app->audio, NULL, false);
        if (app->vision) bot_vision_set_enabled(app->vision, false);
        ESP_LOGE(TAG, "Runtime task allocation failed; restart required");
        return;
    }
#if CONFIG_ESPBOT_USB_MANUAL_CONTROL
    if (xTaskCreate(usb_manual_task, "bot_usb_manual", 4096, app, 2, NULL) != pdPASS)
        ESP_LOGW(TAG, "USB bring-up control unavailable");
#endif
    head_motion_status_t head = head_motion_get_status();
    ESP_LOGI(TAG, "Local runtime started; stationary=%d motors=%d pose_feedback=%d",
             head.stationary, head.motors_installed, head.measured_pose_valid);
    ESP_LOGI(TAG, "startup stack_free=%lu (main task released on return)",
             (unsigned long)uxTaskGetStackHighWaterMark(NULL));
}

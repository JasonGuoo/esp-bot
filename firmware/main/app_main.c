/* 小呆 composition root. One task owns policy; adapters own their workers.
 * No HTTP poller, second microphone reader, UI decision maker, or fake head pose.
 * First-board execution is recorded in docs/board-bringup-2026-11-06.md. */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "sdkconfig.h"
#include "esp_log.h"
#include "esp_random.h"
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

static const char *TAG = "xiaodai";
typedef struct {
    bot_platform_t *platform;
    bot_bus_t *bus;
    bot_runtime_t *runtime;
    bot_audio_t *audio;
    bot_vision_t *vision;
    face_view_t *face;
    attention_controller_t attention;
    idle_attention_t idle_attention;
    bot_runtime_snapshot_t desired;
    uint8_t camera_orientation;
    QueueHandle_t theme_queue;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    QueueHandle_t debug_eye_queue;
    QueueHandle_t debug_camera_queue;
    QueueHandle_t debug_face_queue;
    QueueHandle_t debug_orientation_queue;
    bool debug_camera_wanted;
    uint32_t debug_eye_until_ms;
    int debug_eye_x;
    uint32_t debug_face_until_ms;
    char debug_face[BOT_EXPRESSION_MAX_BYTES + 1];
#endif
    bool vision_health_sent, vision_available; /* only written by vision callback */
    esp_err_t vision_error;
    bool display_health_sent, last_display_awake;
    bool camera_request_sent, last_camera_requested;
    bool near_face_reported, last_near_face;
    bool night_sent, last_night, last_clock_valid;
    uint32_t last_clock_poll_ms, last_diagnostic_ms;
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
static bot_audio_token_t audio_token(bot_session_token_t t) {
    return (bot_audio_token_t){t.epoch, t.session_id, t.turn_id, t.focus_generation};
}

static void audio_event(void *context, const bot_audio_event_t *source) {
    app_t *app = context;
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
        break;
    case BOT_AUDIO_EVENT_VAD_STARTED:
        event.kind = BOT_EVENT_UTTERANCE_STARTED;
        event.data.utterance.echo_safe = source->echo_safe;
        break;
    case BOT_AUDIO_EVENT_VAD_ENDED:
        event.kind = BOT_EVENT_UTTERANCE_ENDED;
        event.data.utterance.echo_safe = source->echo_safe;
        break;
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
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_DONE:
        event.kind = BOT_EVENT_PLAYBACK_DONE;
        event.data.playback.id = source->playback_id;
        break;
    case BOT_AUDIO_EVENT_PLAYBACK_CANCELLED:
    case BOT_AUDIO_EVENT_PLAYBACK_FAILED:
        event.kind = BOT_EVENT_PLAYBACK_FAILED;
        event.data.playback.id = source->playback_id;
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
#if CONFIG_ESPBOT_USB_MANUAL_CONTROL
/* Physical USB input is an explicit manual action. Keep it on the same bus
 * and policy path as touch; it cannot bypass runtime permissions or stop. */
static void usb_manual_task(void *context) {
    app_t *app = context;
    char command[32];
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
                if (strcmp(command, "wake") == 0) {
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
                } else if (strcmp(command, "screen frame") == 0 || strcmp(command, "screen snap") == 0) {
                    if (bot_debug_preview_enabled()) bot_debug_set_screen_snapshot(strcmp(command, "screen snap") == 0);
                    else ESP_LOGW(TAG, "Screen test rejected: USB preview is inactive");
                } else if (strcmp(command, "camera on") == 0 || strcmp(command, "camera off") == 0) {
                    int requested = strcmp(command, "camera on") == 0;
                    if (app->debug_camera_queue && (!requested || bot_debug_preview_enabled()))
                        xQueueOverwrite(app->debug_camera_queue, &requested);
                    else ESP_LOGW(TAG, "USB camera request rejected: preview is inactive");
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
        } else if (byte >= 0x20 && byte <= 0x7e && !discard && length < sizeof(command) - 1) {
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
        .kind = event == FACE_EVENT_SETTINGS_OPENED ? BOT_EVENT_SETTINGS_OPEN : BOT_EVENT_SETTINGS_CLOSE };
    post(app, &message);
}
static void theme_applied(const char *theme, void *context) {
    app_t *app = context;
    char value[32] = {0};
    snprintf(value, sizeof(value), "%s", theme);
    if (app->theme_queue) xQueueOverwrite(app->theme_queue, value);
}
static void apply_state(void *context, const bot_runtime_snapshot_t *state) {
    app_t *app = context;
    app->desired = *state;
    /* This hook also runs at the intermediate revocation barrier. Close the
     * audio token gate now, before stop_output and before another bus event. */
    if (app->audio) {
        bot_audio_token_t token = audio_token(state->token);
        bot_audio_set_session(app->audio, state->audio_session_requested ? &token : NULL, false);
    }
}
static void stop_output(void *context) {
    app_t *app = context;
    if (app->audio) bot_audio_playback_cancel(app->audio, 0);
    head_motion_emergency_stop();
}
static void cancel_remote(void *context, const bot_session_token_t *token) {
    (void)context; (void)token;
    /* No authenticated dialogue/media transport is installed. The local epoch
     * fence is effective now; a future transport adds remote cancellation here. */
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
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
    time_t wall = time(NULL);
    struct tm local;
    valid = wall >= (time_t)1704067200 && localtime_r(&wall, &local) != NULL;
    if (valid) {
        int minute = local.tm_hour * 60 + local.tm_min;
        int start = CONFIG_ESPBOT_NIGHT_START_MINUTE, end = CONFIG_ESPBOT_NIGHT_END_MINUTE;
        night = bot_night_contains(minute, start, end);
    }
#endif
    if (!app->night_sent || app->last_night != night || app->last_clock_valid != valid) {
        ESP_LOGI(TAG, "Night policy clock_valid=%d night=%d", valid, night);
        bot_event_t event = { .kind = BOT_EVENT_NIGHT_POLICY, .source = BOT_SOURCE_PLATFORM,
            .observed_ms = now_ms, .data.night = { .clock_valid = valid, .night = night } };
        post(app, &event);
        app->night_sent = true; app->last_night = night; app->last_clock_valid = valid;
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

static void reconcile(app_t *app, uint32_t now_ms) {
    bot_runtime_snapshot_t *s = &app->desired;
    if (app->audio) {
        bot_audio_token_t token = audio_token(s->token);
        /* This build never supplies an on_pcm consumer or authenticated transport. */
        bot_audio_set_session(app->audio, s->audio_session_requested ? &token : NULL, false);
    }
    bool camera_requested = s->camera_requested;
#if CONFIG_ESPBOT_USB_DEBUG_PREVIEW
    int orientation_command;
    if (app->debug_orientation_queue && xQueueReceive(app->debug_orientation_queue, &orientation_command, 0) == pdTRUE) {
        if (orientation_command == 4) {
            ESP_LOGI(TAG, "Camera orientation current=%u", (unsigned)app->camera_orientation);
        } else {
            esp_err_t saved = bot_platform_save_camera_orientation(app->platform, (uint8_t)orientation_command);
            if (saved == ESP_OK) {
                app->camera_orientation = (uint8_t)orientation_command;
                esp_err_t changed = app->vision ? bot_vision_set_sensor_orientation(app->vision, app->camera_orientation) : ESP_ERR_INVALID_STATE;
                ESP_LOGI(TAG, "Camera orientation current=%u saved=1", (unsigned)app->camera_orientation);
                if (changed != ESP_OK) ESP_LOGW(TAG, "Camera orientation saved; application pending: %s", esp_err_to_name(changed));
            } else ESP_LOGW(TAG, "Camera orientation not saved or changed: %s", esp_err_to_name(saved));
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
        s->behavior != BOT_BEHAVIOR_NIGHT_DARK && !s->settings_open && !s->critical_fault;
    camera_requested = camera_requested || debug_camera_active;
#endif
    if (app->vision && (!app->camera_request_sent || app->last_camera_requested != camera_requested)) {
        if (bot_vision_set_enabled(app->vision, camera_requested) == ESP_OK) {
            ESP_LOGI(TAG, "Camera requested=%d behavior=%s", camera_requested,
                     bot_behavior_name(s->behavior));
            app->last_camera_requested = camera_requested;
            app->camera_request_sent = true;
        }
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
    const char *expression = near_face ? "happy" : s->expression;
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
                                      s->behavior == BOT_BEHAVIOR_ENGAGED || s->critical_fault);
        const char *caption = s->caption;
        if (s->critical_fault) caption = "小呆需要重新启动";
        if (!*caption && s->wake_status != BOT_WAKE_READY && !s->settings_open &&
            (s->behavior == BOT_BEHAVIOR_IDLE || s->behavior == BOT_BEHAVIOR_BORED))
            caption = "点一下，叫醒小呆";
        if (!*caption && s->behavior == BOT_BEHAVIOR_ENGAGED && !s->link_authenticated)
            caption = "我在这里，语音对话还没接通";
        if (strcmp(app->rendered_caption, caption) != 0) {
            face_set_subtitle(app->face, caption);
            snprintf(app->rendered_caption, sizeof(app->rendered_caption), "%s", caption);
        }
        bot_platform_display_unlock();
    }
    if (expression_changed)
        ESP_LOGI(TAG, "Face expression=%s near_face=%d gaze=(%d,%d)", expression, near_face,
                 gaze.eye_x, gaze.eye_y);
    char theme[32];
    if (xQueueReceive(app->theme_queue, theme, 0) == pdTRUE) {
        esp_err_t err = bot_platform_save_theme(app->platform, theme);
        if (err != ESP_OK) ESP_LOGW(TAG, "Appearance persistence: %s", esp_err_to_name(err));
    }
    const uint32_t diagnostic_interval_ms = camera_requested ? 2000 : 15000;
    if ((uint32_t)(now_ms - app->last_diagnostic_ms) >= diagnostic_interval_ms) {
        app->last_diagnostic_ms = now_ms;
        bot_bus_stats_t stats;
        bot_bus_get_stats(app->bus, &stats);
        /* No images, PCM, transcript, person name, or biometric template in logs. */
        ESP_LOGI(TAG, "mode=%s conversation=%s wake=%d audio=%d camera=%d secure_link=%d fault=%d dropped=%lu",
                 bot_behavior_name(s->behavior), bot_conversation_name(s->conversation), s->wake_status,
                 s->audio_available, s->camera_available, s->link_authenticated && s->link_encrypted,
                 s->critical_fault, (unsigned long)stats.normal_events_dropped);
        if (app->vision) {
            bot_vision_status_t vision;
            bot_vision_get_status(app->vision, &vision);
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
            ESP_LOGI(TAG, "audio_capture=%llu afe_frames=%llu afe_errors=%llu vad=%d mic_peaks=(%u,%u) means=(%u,%u) noise=%u active=%d speech=%d doa=%d valid=%d az=%.1f quality=%.1f sound_events=%llu estimates=%llu uplink=%d",
                     (unsigned long long)audio.capture_frames, (unsigned long long)audio.afe_frames,
                     (unsigned long long)audio.afe_errors, audio.vad_available,
                     audio.microphone_peak, audio.microphone_peak_2, audio.microphone_mean,
                     audio.microphone_mean_2, audio.ambient_noise_mean, audio.ambient_activity, audio.ambient_speech,
                     audio.direction_available, audio.direction_valid, audio.azimuth_deg,
                     audio.direction_quality, (unsigned long long)audio.ambient_events,
                     (unsigned long long)audio.direction_estimates, audio.pcm_delivery_enabled);
        }
    }
}

static void runtime_task(void *argument) {
    app_t *app = argument;
    TickType_t previous = xTaskGetTickCount();
    for (;;) {
        uint32_t now = bot_platform_now_ms();
        poll_night(app, now);
        bot_runtime_step(app->runtime, now);
        reconcile(app, bot_platform_now_ms());
        vTaskDelayUntil(&previous, pdMS_TO_TICKS(20));
    }
}

void app_main(void) {
    ESP_LOGI(TAG, "Boot reset_reason=%d; normal operation does not rewrite application flash",
             (int)esp_reset_reason());
    app_t *app = calloc(1, sizeof(*app));
    if (!app) { ESP_LOGE(TAG, "Cannot allocate application"); return; }
    idle_attention_init(&app->idle_attention, esp_random());
    bot_bus_config_t bus_cfg = bot_bus_default_config();
    ESP_ERROR_CHECK(bot_bus_create(&bus_cfg, &app->bus));
    ESP_ERROR_CHECK(bot_platform_create(&app->platform));
    app->theme_queue = xQueueCreate(1, 32);
    if (!app->theme_queue) { ESP_LOGE(TAG, "Cannot allocate preferences queue"); return; }
    bot_runtime_config_t config = bot_runtime_default_config();
    attention_config_t attention_cfg = attention_default_config();
    attention_cfg.person_min_confidence = config.focus.min_face_confidence;
    attention_controller_init(&app->attention, &attention_cfg);
    /* Coordinate mode is fixed before the first observation; no pose sensor exists. */
    app->attention.camera_relative = true;
    bot_platform_status_t platform;
    bot_platform_get_status(app->platform, &platform);
    if (platform.display_available && bot_platform_display_lock(0)) {
        app->face = face_create(bot_platform_screen(app->platform));
        if (app->face) {
            char theme[32];
            if (bot_platform_load_theme(app->platform, theme, sizeof(theme)) == ESP_OK)
                face_set_theme(app->face, theme);
            face_on_theme_applied(app->face, theme_applied, app);
            face_on_event(app->face, face_event, app);
        }
        bot_platform_on_dark_touch(app->platform, touch_wake, app);
        bot_platform_display_unlock();
    }
    config.allow_audio_uplink = false;
    config.allow_barge_in = false; /* no measured AEC reference */
    bot_runtime_ops_t ops = { .apply_state = apply_state, .stop_output = stop_output,
        .cancel_remote = cancel_remote, .command_result = command_result,
        .now_ms = runtime_now_ms };
    ESP_ERROR_CHECK(bot_runtime_create(app->bus, &config, &ops, app, bot_platform_now_ms(), &app->runtime));
    bot_runtime_get_snapshot(app->runtime, &app->desired);
#if CONFIG_ESPBOT_NIGHT_SCHEDULE
    setenv("TZ", CONFIG_ESPBOT_TIMEZONE, 1);
    tzset();
#endif
    bot_audio_config_t audio_cfg = bot_audio_default_config();
    audio_cfg.on_event = audio_event; audio_cfg.context = app;
    esp_err_t err = bot_audio_create(&audio_cfg, &app->audio);
    if (err == ESP_OK) err = bot_audio_start(app->audio);
    if (err != ESP_OK) {
        ESP_LOGW(TAG, "Audio degraded: %s; touch remains available", esp_err_to_name(err));
        bot_event_t event = { .kind = BOT_EVENT_AUDIO_HEALTH, .source = BOT_SOURCE_AUDIO,
            .observed_ms = bot_platform_now_ms(), .data.health = { .available = false, .error = err } };
        post(app, &event);
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
    if (!app->debug_eye_queue) ESP_LOGW(TAG, "USB eye test unavailable: queue allocation failed");
    if (!app->debug_camera_queue) ESP_LOGW(TAG, "USB camera test unavailable: queue allocation failed");
    if (!app->debug_face_queue) ESP_LOGW(TAG, "USB face test unavailable: queue allocation failed");
    if (!app->debug_orientation_queue) ESP_LOGW(TAG, "USB camera orientation unavailable: queue allocation failed");
    err = bot_debug_start(app->platform);
    if (err != ESP_OK) ESP_LOGW(TAG, "USB preview unavailable: %s", esp_err_to_name(err));
#endif
#if CONFIG_ESPBOT_GAZE_CALIBRATED
    vision_cfg.calibration_valid = true;
#endif
    err = bot_vision_start(&vision_cfg, &app->vision);
    if (err != ESP_OK) ESP_LOGW(TAG, "Vision unavailable: %s", esp_err_to_name(err));
    err = brain_link_start(app->bus);
    if (err != ESP_OK) ESP_LOGW(TAG, "Network unavailable: %s; local runtime continues", esp_err_to_name(err));
    if (xTaskCreate(runtime_task, "bot_runtime", 12288, app, 6, NULL) != pdPASS) {
        stop_output(app);
        if (app->audio) bot_audio_set_session(app->audio, NULL, false);
        if (app->vision) bot_vision_set_enabled(app->vision, false);
        ESP_LOGE(TAG, "Runtime task allocation failed; restart required");
        return;
    }
#if CONFIG_ESPBOT_USB_MANUAL_CONTROL
    if (xTaskCreate(usb_manual_task, "bot_usb_manual", 3072, app, 2, NULL) != pdPASS)
        ESP_LOGW(TAG, "USB bring-up control unavailable");
#endif
    head_motion_status_t head = head_motion_get_status();
    ESP_LOGI(TAG, "Local runtime started; stationary=%d motors=%d pose_feedback=%d",
             head.stationary, head.motors_installed, head.measured_pose_valid);
}

/* 小呆 composition root. One task owns policy; adapters own their workers.
 * No HTTP poller, second microphone reader, UI decision maker, or fake head pose.
 * Written without build/device execution; see docs/local-runtime.md. */
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include "sdkconfig.h"
#include "esp_log.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include "attention.h"
#include "bot_audio.h"
#include "bot_bus.h"
#include "bot_platform.h"
#include "bot_runtime.h"
#include "bot_vision.h"
#include "brain_link.h"
#include "face.h"
#include "head_motion.h"

static const char *TAG = "xiaodai";
typedef struct {
    bot_platform_t *platform;
    bot_bus_t *bus;
    bot_runtime_t *runtime;
    bot_audio_t *audio;
    bot_vision_t *vision;
    face_view_t *face;
    attention_controller_t attention;
    bot_runtime_snapshot_t desired;
    QueueHandle_t theme_queue;
    bool vision_health_sent, vision_available; /* only written by vision callback */
    esp_err_t vision_error;
    bool display_health_sent, last_display_awake;
    bool camera_request_sent, last_camera_requested;
    bool night_sent, last_night, last_clock_valid;
    uint32_t last_clock_poll_ms, last_diagnostic_ms;
    char rendered_caption[BOT_CAPTION_MAX_BYTES + 1];
} app_t;

static void post(app_t *app, const bot_event_t *event) {
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
    bot_event_t event = { .kind = BOT_EVENT_TOUCH_WAKE, .source = BOT_SOURCE_LOCAL_UI,
        .observed_ms = bot_platform_now_ms(), .data.touch.intentional = true };
    post(app, &event);
}
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
        night = start == end ? false : (start < end ? minute >= start && minute < end :
                                                     minute >= start || minute < end);
    }
#endif
    if (!app->night_sent || app->last_night != night || app->last_clock_valid != valid) {
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
    if (app->vision && (!app->camera_request_sent || app->last_camera_requested != s->camera_requested)) {
        if (bot_vision_set_enabled(app->vision, s->camera_requested) == ESP_OK) {
            app->last_camera_requested = s->camera_requested;
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
    if (app->face && bot_platform_display_lock(5)) {
        face_set_animation_active(app->face, platform.display_awake);
        face_set_expression(app->face, s->expression);
        face_set_gaze(app->face, gaze.eye_x, gaze.eye_y);
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
    char theme[32];
    if (xQueueReceive(app->theme_queue, theme, 0) == pdTRUE) {
        esp_err_t err = bot_platform_save_theme(app->platform, theme);
        if (err != ESP_OK) ESP_LOGW(TAG, "Appearance persistence: %s", esp_err_to_name(err));
    }
    if ((uint32_t)(now_ms - app->last_diagnostic_ms) >= 15000) {
        app->last_diagnostic_ms = now_ms;
        bot_bus_stats_t stats;
        bot_bus_get_stats(app->bus, &stats);
        /* No images, PCM, transcript, person name, or biometric template in logs. */
        ESP_LOGI(TAG, "mode=%s conversation=%s wake=%d audio=%d camera=%d secure_link=%d fault=%d dropped=%lu",
                 bot_behavior_name(s->behavior), bot_conversation_name(s->conversation), s->wake_status,
                 s->audio_available, s->camera_available, s->link_authenticated && s->link_encrypted,
                 s->critical_fault, (unsigned long)stats.normal_events_dropped);
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
    app_t *app = calloc(1, sizeof(*app));
    if (!app) { ESP_LOGE(TAG, "Cannot allocate application"); return; }
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
    head_motion_status_t head = head_motion_get_status();
    ESP_LOGI(TAG, "Local runtime started; stationary=%d motors=%d pose_feedback=%d",
             head.stationary, head.motors_installed, head.measured_pose_valid);
}

/* SPDX-License-Identifier: MIT
 * BSP/esp_codec_dev construction follows Espressif's ESP32-S31-Korvo-1
 * bsp_audio.c. AFE feed/fetch follows the official esp-skainet examples.
 * Local policies (permission gates, tokens, cancellation) are project code.
 */
#include "bot_audio.h"
#include "bot_audio_afe.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sdkconfig.h"
#include "bsp/esp32_s31_korvo_1.h"
#include "driver/gpio.h"
#include "driver/i2s_std.h"
#include "esp_codec_dev.h"
#include "esp_codec_dev_defaults.h"
#include "es8389_codec.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/event_groups.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

#if !CONFIG_IDF_TARGET_ESP32S31
#error "bot_audio supports ESP32-S31-Korvo-1 V1.1 only"
#endif
#if !CONFIG_CODEC_ES8389_SUPPORT
#error "bot_audio requires the official ES8389 codec driver"
#endif
#if defined(CONFIG_MODEL_IN_SDCARD) && CONFIG_MODEL_IN_SDCARD
#error "bot_audio requires ESP-SR MODEL_IN_FLASH; SD recording/model paths are not used"
#endif

#define CAPTURE_EXIT BIT0
#define FETCH_EXIT BIT1
#define PLAYBACK_EXIT BIT2
#define ALL_EXIT (CAPTURE_EXIT | FETCH_EXIT | PLAYBACK_EXIT)
#define PLAYBACK_STARVE_US INT64_C(2000000)
#define MAX_OUTPUT_VOLUME 60

typedef struct {
    uint32_t id;
    uint16_t count;
    bool end;
    int16_t samples[BOT_AUDIO_PLAYBACK_SAMPLES];
} playback_frame_t;

struct bot_audio {
    portMUX_TYPE lock;
    bot_audio_config_t config;
    char model_name[BOT_AUDIO_MODEL_NAME_BYTES + 1];
    char partition_label[17]; /* ESP partition labels are at most 16 bytes */
    bot_audio_status_t status;
    bool run, capture_good, output_fault;
    EventGroupHandle_t exits;
    SemaphoreHandle_t feed_lock;
    SemaphoreHandle_t command_lock;
    QueueHandle_t output_queue;
    TaskHandle_t capture_task, fetch_task, output_task;
    bot_audio_afe_t afe;
    esp_codec_dev_handle_t codec;
    const audio_codec_if_t *codec_if;
    const audio_codec_ctrl_if_t *ctrl_if;
    const audio_codec_gpio_if_t *gpio_if;
    bool codec_open;
    int16_t *stereo_capture, *mono_capture;
    size_t capture_samples;
    uint32_t dma_frames;
    uint64_t capture_sequence;
    uint32_t gate_epoch;
    bot_audio_token_t session;
    uint32_t afe_reset_requested, afe_epoch;
    bot_audio_token_t afe_token;
    bool playback_requested, playback_cancelled, playback_end_enqueued;
    uint32_t next_playback_id;
    bot_audio_token_t playback_token;
    int64_t playback_begin_us, last_queued_us, echo_tail_until_us, last_wake_us;
};

/* BSP owns process-lifetime static I2S handles. Lifecycle operations themselves
 * are serialized by the application's owner task, not by competing callers. */
static bot_audio_t *s_owner;
#define LOCK(a) portENTER_CRITICAL(&(a)->lock)
#define UNLOCK(a) portEXIT_CRITICAL(&(a)->lock)

static bool same_token(const bot_audio_token_t *a, const bot_audio_token_t *b)
{
    return a->epoch == b->epoch && a->session_id == b->session_id &&
           a->turn_id == b->turn_id && a->focus_generation == b->focus_generation;
}

static void clear_audio(void *buffer, size_t size)
{
    volatile unsigned char *p = buffer;
    while (size--) *p++ = 0;
}

static void emit(bot_audio_t *a, bot_audio_event_t event)
{
    if (!event.observed_us) event.observed_us = esp_timer_get_time();
    if (a->config.on_event) a->config.on_event(a->config.context, &event);
}

static void set_reason_locked(bot_audio_t *a, esp_err_t error, const char *text)
{
    a->status.last_error = error;
    snprintf(a->status.reason, sizeof(a->status.reason), "%s", text);
}

static bool running(bot_audio_t *a)
{
    LOCK(a);
    bool result = a->run;
    UNLOCK(a);
    return result;
}

static void publish_wake_status(bot_audio_t *a)
{
    bot_audio_status_t status;
    bot_audio_get_status(a, &status);
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_WAKE_STATUS,
        .wake_status = status.wake_status, .available = status.wake_available,
        .error = status.last_error, .reason = status.reason});
}

static void afe_failed(bot_audio_t *a, esp_err_t error, const char *text)
{
    LOCK(a);
    a->status.afe_running = false;
    a->status.vad_available = false;
    a->status.wake_available = false;
    a->status.wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
    set_reason_locked(a, error, text);
    UNLOCK(a);
    publish_wake_status(a);
}

static void codec_cleanup(bot_audio_t *a)
{
    if (a->codec) {
        if (a->codec_open) {
            (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
            (void)esp_codec_dev_set_out_mute(a->codec, true);
            (void)esp_codec_dev_close(a->codec);
        }
        esp_codec_dev_delete(a->codec);
    }
    a->codec = NULL;
    a->codec_open = false;
    if (a->codec_if) audio_codec_delete_codec_if(a->codec_if);
    if (a->ctrl_if) audio_codec_delete_ctrl_if(a->ctrl_if);
    if (a->gpio_if) audio_codec_delete_gpio_if(a->gpio_if);
    a->codec_if = NULL;
    a->ctrl_if = NULL;
    a->gpio_if = NULL;
    /* Never delete bsp_audio_get_codec_itf() or the shared BSP I2C bus. */
}

static esp_err_t codec_open(bot_audio_t *a)
{
    esp_err_t err = bsp_i2c_init();
    if (err != ESP_OK) return err;
    i2s_std_config_t bus = {
        .clk_cfg = I2S_STD_CLK_DEFAULT_CONFIG(BOT_AUDIO_SAMPLE_RATE),
        .slot_cfg = I2S_STD_PHILIP_SLOT_DEFAULT_CONFIG(I2S_DATA_BIT_WIDTH_16BIT, I2S_SLOT_MODE_STEREO),
        .gpio_cfg = {.mclk = BSP_I2S_MCLK, .bclk = BSP_I2S_SCLK,
            .ws = BSP_I2S_LCLK, .dout = BSP_I2S_DOUT, .din = BSP_I2S_DSIN},
    };
    err = bsp_audio_init(&bus);
    if (err != ESP_OK) return err;
    const audio_codec_data_if_t *data = bsp_audio_get_codec_itf();
    if (!data) return ESP_ERR_INVALID_STATE;
    /* This is exactly the DMA geometry used by pinned BSP 1.1.0. It bounds the
     * potential preauthorization FIFO, not physical sample timestamps. */
    i2s_chan_config_t dma = I2S_CHANNEL_DEFAULT_CONFIG(CONFIG_BSP_I2S_NUM, I2S_ROLE_MASTER);
    a->dma_frames = dma.dma_desc_num * dma.dma_frame_num;
    audio_codec_i2c_cfg_t control = {
        .port = BSP_I2C_NUM, .addr = ES8389_CODEC_DEFAULT_ADDR,
        .bus_handle = bsp_i2c_get_handle(),
    };
    a->ctrl_if = audio_codec_new_i2c_ctrl(&control);
    a->gpio_if = audio_codec_new_gpio();
    if (!a->ctrl_if || !a->gpio_if) return ESP_ERR_NO_MEM;
    es8389_codec_cfg_t device = {
        .ctrl_if = a->ctrl_if, .gpio_if = a->gpio_if,
        .codec_mode = ESP_CODEC_DEV_WORK_MODE_BOTH,
        .pa_pin = BSP_POWER_AMP_IO, .pa_reverted = false, .master_mode = false,
        .use_mclk = false, /* same zero/default clock selection as the BSP */
        .hw_gain = {.pa_voltage = 5.0f, .codec_dac_voltage = 3.3f},
    };
    a->codec_if = es8389_codec_new(&device);
    if (!a->codec_if) return ESP_ERR_NO_MEM;
    esp_codec_dev_cfg_t owner = {
        .dev_type = ESP_CODEC_DEV_TYPE_IN_OUT,
        .codec_if = a->codec_if, .data_if = data,
    };
    a->codec = esp_codec_dev_new(&owner);
    if (!a->codec) return ESP_ERR_NO_MEM;
    esp_codec_dev_sample_info_t format = {
        .sample_rate = BOT_AUDIO_SAMPLE_RATE, .bits_per_sample = 16, .channel = 2,
    };
    err = esp_codec_dev_open(a->codec, &format);
    if (err != ESP_OK) return err;
    a->codec_open = true;
    err = esp_codec_dev_set_out_mute(a->codec, true);
    if (err == ESP_OK) err = gpio_set_level(BSP_POWER_AMP_IO, 0);
    if (err == ESP_OK) err = esp_codec_dev_set_out_vol(a->codec, a->config.output_volume);
    if (err == ESP_OK) err = esp_codec_dev_set_in_gain(a->codec, a->config.microphone_gain_db);
    return err;
}

static void capture_worker(void *context)
{
    bot_audio_t *a = context;
    const size_t bytes = a->capture_samples * 2 * sizeof(int16_t);
    LOCK(a);
    a->status.capture_running = true;
    UNLOCK(a);
    while (running(a)) {
        LOCK(a);
        uint32_t read_epoch = a->gate_epoch;
        uint64_t sequence = ++a->capture_sequence;
        UNLOCK(a);
        int64_t started = esp_timer_get_time();
        memset(a->stereo_capture, 0, bytes);
        esp_err_t err = esp_codec_dev_read(a->codec, a->stereo_capture, (int)bytes);
        int64_t observed = esp_timer_get_time();
        if (err != ESP_OK) {
            LOCK(a);
            bool changed = a->capture_good;
            a->capture_good = false;
            a->status.vad_available = false;
            a->status.wake_available = false;
            a->status.capture_errors++;
            set_reason_locked(a, err, "Codec capture failed; no PCM emitted");
            UNLOCK(a);
            if (changed) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH,
                .error = err, .available = false, .reason = "Codec capture failed"});
            vTaskDelay(pdMS_TO_TICKS(20));
            continue;
        }
        /* Only lane zero is used. No inference about lane one's mic/reference
         * routing is required for local wake or authorized primary-mic PCM. */
        for (size_t i = 0; i < a->capture_samples; ++i) a->mono_capture[i] = a->stereo_capture[2 * i];
        LOCK(a);
        bool health_changed = !a->capture_good;
        a->capture_good = true;
        a->status.capture_frames++;
        a->status.last_capture_us = observed;
        a->status.vad_available = a->status.afe_running;
        a->status.wake_available = a->status.afe_running &&
                                  a->status.wake_status == BOT_AUDIO_WAKE_READY;
        bool eligible = a->run && a->status.session_open && read_epoch == a->gate_epoch;
        if (eligible && a->status.rx_discard_frames) {
            uint32_t remaining = a->status.rx_discard_frames;
            a->status.rx_discard_frames = remaining > a->capture_samples ?
                                         remaining - a->capture_samples : 0;
            eligible = false; /* discard this entire block, including partial fence */
        }
        if (eligible && a->afe_epoch != a->gate_epoch) a->afe_reset_requested = a->gate_epoch;
        bool feed_allowed = a->status.afe_running &&
                            (!a->afe_reset_requested || a->afe_epoch == a->afe_reset_requested);
        bool deliver = eligible && a->status.pcm_delivery_enabled && a->config.on_pcm != NULL;
        bot_audio_token_t token = a->session;
        UNLOCK(a);
        if (health_changed) {
            emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH,
                .observed_us = observed, .available = true, .reason = "Primary microphone capture available"});
            publish_wake_status(a);
        }
        if (feed_allowed && xSemaphoreTake(a->feed_lock, pdMS_TO_TICKS(20)) == pdTRUE) {
            LOCK(a);
            feed_allowed = a->status.afe_running &&
                           (!a->afe_reset_requested || a->afe_epoch == a->afe_reset_requested);
            UNLOCK(a);
            int result = 0;
            if (feed_allowed) result = a->afe.iface->feed(a->afe.data, a->mono_capture);
            xSemaphoreGive(a->feed_lock);
            if (result < 0) afe_failed(a, ESP_FAIL, "Official AFE feed failed");
        }
        if (deliver && a->config.on_pcm) {
            LOCK(a);
            deliver = a->run && a->status.session_open && a->status.pcm_delivery_enabled &&
                      a->gate_epoch == read_epoch && same_token(&a->session, &token);
            if (deliver) a->status.delivered_pcm_frames++;
            UNLOCK(a);
            if (deliver) {
                bot_audio_pcm_t frame = {.samples = a->mono_capture,
                    .sample_count = a->capture_samples, .sample_rate = BOT_AUDIO_SAMPLE_RATE,
                    .sequence = sequence, .read_started_us = started, .observed_us = observed,
                    .token = token, .enhanced = false, .echo_cancelled = false};
                a->config.on_pcm(a->config.context, &frame);
            }
        }
        if (!deliver) {
            LOCK(a);
            a->status.gated_pcm_frames++;
            UNLOCK(a);
        }
    }
    clear_audio(a->stereo_capture, bytes);
    clear_audio(a->mono_capture, a->capture_samples * sizeof(int16_t));
    LOCK(a);
    a->status.capture_running = false;
    a->capture_good = false;
    a->status.vad_available = false;
    a->status.wake_available = false;
    UNLOCK(a);
    xEventGroupSetBits(a->exits, CAPTURE_EXIT);
    vTaskDelete(NULL);
}

/* Reset is owned by fetch; a feed mutex prevents concurrent feed/reset. Fetch
 * itself never holds that mutex while blocking, so a full native feed ring can
 * always be drained. First post-fence feed waits for this acknowledgement. */
static void apply_afe_fence(bot_audio_t *a)
{
    LOCK(a);
    uint32_t requested = a->afe_reset_requested;
    bool needed = requested && requested != a->afe_epoch &&
                  requested == a->gate_epoch && a->status.session_open;
    bot_audio_token_t token = a->session;
    UNLOCK(a);
    if (!needed || xSemaphoreTake(a->feed_lock, 0) != pdTRUE) return;
    /* The official reset APIs return 1 on success, -1 on failure. Never
     * acknowledge a new permission epoch if old AFE state could remain. */
    int reset = a->afe.iface->reset_buffer ? a->afe.iface->reset_buffer(a->afe.data) : -1;
    int vad_reset = a->afe.iface->reset_vad ? a->afe.iface->reset_vad(a->afe.data) : -1;
    if (reset != 1 || vad_reset != 1) {
        xSemaphoreGive(a->feed_lock);
        afe_failed(a, ESP_FAIL, "Official AFE session reset failed; local detector disabled");
        return;
    }
    LOCK(a);
    if (requested == a->gate_epoch && a->status.session_open) {
        a->afe_epoch = requested;
        a->afe_token = token;
        a->afe_reset_requested = 0;
    }
    UNLOCK(a);
    xSemaphoreGive(a->feed_lock);
}

static void fetch_worker(void *context)
{
    bot_audio_t *a = context;
    bool speech = false;
    uint32_t speech_epoch = 0;
    unsigned failures = 0;
    while (running(a)) {
        LOCK(a);
        bool active = a->status.afe_running;
        UNLOCK(a);
        if (!active) break;
        apply_afe_fence(a);
        LOCK(a);
        active = a->status.afe_running;
        uint32_t epoch = a->afe_epoch;
        bot_audio_token_t token = a->afe_token;
        UNLOCK(a);
        if (!active) break;
        afe_fetch_result_t *result = a->afe.iface->fetch_with_delay(a->afe.data, pdMS_TO_TICKS(20));
        if (!result) continue; /* bounded wait elapsed; not a hardware fault */
        if (result->ret_value != ESP_OK) {
            LOCK(a);
            a->status.afe_errors++;
            UNLOCK(a);
            if (++failures >= 20) {
                afe_failed(a, ESP_FAIL, "Official AFE fetch repeatedly failed");
                break;
            }
            vTaskDelay(pdMS_TO_TICKS(5));
            continue;
        }
        failures = 0;
        int64_t now = esp_timer_get_time();
        LOCK(a);
        a->status.afe_frames++;
        a->status.last_afe_result_us = now;
        bool echo_block = a->status.playback_running || now < a->echo_tail_until_us;
        bool vad_allowed = a->run && a->capture_good && a->status.session_open && epoch != 0 &&
                           epoch == a->gate_epoch && a->status.rx_discard_frames == 0 && !echo_block;
        bool wake = a->run && a->capture_good && a->status.wake_status == BOT_AUDIO_WAKE_READY &&
                    result->wakeup_state == WAKENET_DETECTED &&
                    result->wakenet_model_index == 1 && result->wake_word_index == 1;
        bool wake_allowed = wake && !echo_block &&
            (!a->last_wake_us || now - a->last_wake_us >= (int64_t)a->config.wake_debounce_ms * 1000);
        if (wake_allowed) {
            a->last_wake_us = now;
            a->status.wake_detections++;
        } else if (wake) a->status.suppressed_wakes++;
        UNLOCK(a);
        if (wake_allowed) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_WAKE_DETECTED,
            .observed_us = now, .wake_phrase = BOT_AUDIO_WAKE_PHRASE,
            .wake_word_index = result->wake_word_index, .echo_safe = true,
            .confidence_available = false, .confidence = 0.0f});
        if (epoch != speech_epoch || !vad_allowed) {
            speech = false;
            speech_epoch = epoch;
        }
        if (!vad_allowed) continue;
        bool next_speech = result->vad_state == VAD_SPEECH;
        if (next_speech != speech) {
            speech = next_speech;
            emit(a, (bot_audio_event_t){.kind = speech ? BOT_AUDIO_EVENT_VAD_STARTED : BOT_AUDIO_EVENT_VAD_ENDED,
                .observed_us = now, .token = token, .echo_safe = false});
        }
        /* result->data/vad_cache/raw_data are never exported. In particular,
         * vad_cache can precede the user's authorization and is discarded. */
    }
    LOCK(a);
    a->status.afe_running = false;
    a->status.vad_available = false;
    a->status.wake_available = false;
    UNLOCK(a);
    xEventGroupSetBits(a->exits, FETCH_EXIT);
    vTaskDelete(NULL);
}

static esp_err_t output_silence(bot_audio_t *a, uint32_t frames)
{
    int16_t zeros[BOT_AUDIO_PLAYBACK_SAMPLES * 2] = {0};
    while (frames) {
        uint32_t count = frames > BOT_AUDIO_PLAYBACK_SAMPLES ? BOT_AUDIO_PLAYBACK_SAMPLES : frames;
        esp_err_t err = esp_codec_dev_write(a->codec, zeros, count * 2 * sizeof(int16_t));
        if (err != ESP_OK) return err;
        frames -= count;
    }
    return ESP_OK;
}

static void finish_playback(bot_audio_t *a, uint32_t id, bot_audio_token_t token,
                            bot_audio_event_kind_t kind, esp_err_t error)
{
    esp_err_t amp_error = gpio_set_level(BSP_POWER_AMP_IO, 0);
    esp_err_t mute_error = esp_codec_dev_set_out_mute(a->codec, true);
    /* Purge old DMA samples while muted. No later stream can begin until this
     * completes, so unmuting never intentionally replays the canceled tail. */
    esp_err_t flush = output_silence(a, a->dma_frames + BOT_AUDIO_PLAYBACK_SAMPLES);
    esp_err_t drain_error = amp_error != ESP_OK ? amp_error : mute_error != ESP_OK ? mute_error : flush;
    if (error == ESP_OK && drain_error != ESP_OK) error = drain_error;
    if (error != ESP_OK) kind = BOT_AUDIO_EVENT_PLAYBACK_FAILED;
    xSemaphoreTake(a->command_lock, portMAX_DELAY);
    xQueueReset(a->output_queue);
    LOCK(a);
    if (a->status.playback_id == id) {
        if (a->playback_cancelled && error == ESP_OK) kind = BOT_AUDIO_EVENT_PLAYBACK_CANCELLED;
        /* A failed purge must never be followed by unmuting another stream. */
        if (drain_error != ESP_OK) a->output_fault = true;
        a->playback_requested = false;
        a->playback_cancelled = false;
        a->playback_end_enqueued = false;
        a->status.playback_running = false;
        a->echo_tail_until_us = esp_timer_get_time() + (int64_t)a->config.playback_tail_ms * 1000;
        if (error != ESP_OK) {
            a->status.playback_errors++;
            set_reason_locked(a, error, "Local playback failed; amplifier gated off");
        }
    }
    UNLOCK(a);
    xSemaphoreGive(a->command_lock);
    emit(a, (bot_audio_event_t){.kind = kind, .token = token, .playback_id = id,
        .error = error, .reason = error == ESP_OK ? NULL : "Local playback or DMA drain failed"});
}

static void playback_worker(void *context)
{
    bot_audio_t *a = context;
    playback_frame_t frame;
    int16_t stereo[BOT_AUDIO_PLAYBACK_SAMPLES * 2];
    while (true) {
        LOCK(a);
        bool run = a->run;
        bool requested = a->playback_requested;
        bool cancelled = a->playback_cancelled || !run;
        bool started = a->status.playback_running;
        uint32_t id = a->status.playback_id;
        bot_audio_token_t token = a->playback_token;
        int64_t queued_at = a->last_queued_us;
        UNLOCK(a);
        if (!run && !requested) break;
        if (!requested) {
            vTaskDelay(pdMS_TO_TICKS(10));
            continue;
        }
        if (cancelled) {
            finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_CANCELLED, ESP_OK);
            continue;
        }
        if (xQueueReceive(a->output_queue, &frame, pdMS_TO_TICKS(20)) != pdTRUE) {
            if (esp_timer_get_time() - queued_at > PLAYBACK_STARVE_US) {
                finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, ESP_ERR_TIMEOUT);
            } else if (started) {
                LOCK(a);
                a->status.playback_underruns++;
                UNLOCK(a);
                esp_err_t err = output_silence(a, BOT_AUDIO_PLAYBACK_SAMPLES);
                if (err != ESP_OK) finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
            }
            continue;
        }
        if (frame.id != id) {
            clear_audio(&frame, sizeof(frame));
            continue;
        }
        LOCK(a);
        bool current = a->run && a->playback_requested && !a->playback_cancelled && a->status.playback_id == id;
        UNLOCK(a);
        if (!current) {
            clear_audio(&frame, sizeof(frame));
            continue;
        }
        if (frame.end) {
            /* Enough zero frames push all preceding audio out of DMA before DONE. */
            esp_err_t err = output_silence(a, a->dma_frames + BOT_AUDIO_PLAYBACK_SAMPLES);
            finish_playback(a, id, token, err == ESP_OK ? BOT_AUDIO_EVENT_PLAYBACK_DONE : BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
            clear_audio(&frame, sizeof(frame));
            continue;
        }
        if (!started) {
            esp_err_t err = esp_codec_dev_set_out_mute(a->codec, false);
            if (err != ESP_OK) {
                finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
                clear_audio(&frame, sizeof(frame));
                continue;
            }
            LOCK(a);
            current = a->run && a->playback_requested && !a->playback_cancelled && a->status.playback_id == id;
            if (current) {
                err = gpio_set_level(BSP_POWER_AMP_IO, 1);
                a->status.playback_running = err == ESP_OK;
            }
            UNLOCK(a);
            if (!current) {
                clear_audio(&frame, sizeof(frame));
                continue;
            }
            if (err != ESP_OK) {
                finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
                clear_audio(&frame, sizeof(frame));
                continue;
            }
            emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_PLAYBACK_STARTED, .token = token, .playback_id = id});
        }
        for (size_t i = 0; i < frame.count; ++i) stereo[2 * i] = stereo[2 * i + 1] = frame.samples[i];
        esp_err_t err = esp_codec_dev_write(a->codec, stereo, frame.count * 2 * sizeof(int16_t));
        clear_audio(&frame, sizeof(frame));
        clear_audio(stereo, sizeof(stereo));
        LOCK(a);
        a->status.last_playback_us = esp_timer_get_time();
        UNLOCK(a);
        if (err != ESP_OK) finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
    }
    clear_audio(&frame, sizeof(frame));
    clear_audio(stereo, sizeof(stereo));
    (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    xEventGroupSetBits(a->exits, PLAYBACK_EXIT);
    vTaskDelete(NULL);
}

bot_audio_config_t bot_audio_default_config(void)
{
    return (bot_audio_config_t){
        .wake_model_name = CONFIG_BOT_AUDIO_WAKE_MODEL_NAME,
        .model_partition = CONFIG_BOT_AUDIO_MODEL_PARTITION,
        .output_volume = CONFIG_BOT_AUDIO_OUTPUT_VOLUME,
        .microphone_gain_db = CONFIG_BOT_AUDIO_MIC_GAIN_DB,
        .wake_debounce_ms = CONFIG_BOT_AUDIO_WAKE_DEBOUNCE_MS,
        .playback_tail_ms = CONFIG_BOT_AUDIO_PLAYBACK_TAIL_MS,
    };
}

esp_err_t bot_audio_create(const bot_audio_config_t *config, bot_audio_t **out)
{
    if (!out) return ESP_ERR_INVALID_ARG;
    *out = NULL;
    if (s_owner) return ESP_ERR_INVALID_STATE;
    bot_audio_config_t cfg = config ? *config : bot_audio_default_config();
    if (!cfg.wake_model_name || !cfg.model_partition || !cfg.model_partition[0] ||
        strlen(cfg.wake_model_name) > BOT_AUDIO_MODEL_NAME_BYTES ||
        strlen(cfg.model_partition) > 16 || cfg.output_volume > MAX_OUTPUT_VOLUME ||
        !isfinite(cfg.microphone_gain_db) || cfg.microphone_gain_db < 0 || cfg.microphone_gain_db > 42 ||
        cfg.wake_debounce_ms < 500 || cfg.wake_debounce_ms > 10000 ||
        cfg.playback_tail_ms < 200 || cfg.playback_tail_ms > 3000) return ESP_ERR_INVALID_ARG;
    bot_audio_t *a = calloc(1, sizeof(*a));
    if (!a) return ESP_ERR_NO_MEM;
    a->lock = (portMUX_TYPE)portMUX_INITIALIZER_UNLOCKED;
    a->config = cfg;
    snprintf(a->model_name, sizeof(a->model_name), "%s", cfg.wake_model_name);
    snprintf(a->partition_label, sizeof(a->partition_label), "%s", cfg.model_partition);
    a->config.wake_model_name = a->model_name;
    a->config.model_partition = a->partition_label;
    a->exits = xEventGroupCreate();
    a->feed_lock = xSemaphoreCreateMutex();
    a->command_lock = xSemaphoreCreateMutex();
    a->output_queue = xQueueCreate(CONFIG_BOT_AUDIO_PLAYBACK_QUEUE_FRAMES, sizeof(playback_frame_t));
    if (!a->exits || !a->feed_lock || !a->command_lock || !a->output_queue) {
        if (a->output_queue) vQueueDelete(a->output_queue);
        if (a->command_lock) vSemaphoreDelete(a->command_lock);
        if (a->feed_lock) vSemaphoreDelete(a->feed_lock);
        if (a->exits) vEventGroupDelete(a->exits);
        free(a);
        return ESP_ERR_NO_MEM;
    }
    a->gate_epoch = 1;
    a->status.wake_status = BOT_AUDIO_WAKE_MODEL_MISSING;
    snprintf(a->status.model_name, sizeof(a->status.model_name), "%s", a->model_name);
    snprintf(a->status.reason, sizeof(a->status.reason), "%s", "Audio owner created; hardware and model not opened");
    xEventGroupSetBits(a->exits, ALL_EXIT);
    s_owner = a;
    *out = a;
    return ESP_OK;
}

static void release_capture_buffers(bot_audio_t *a)
{
    if (a->stereo_capture) {
        clear_audio(a->stereo_capture, a->capture_samples * 2 * sizeof(int16_t));
        free(a->stereo_capture);
    }
    if (a->mono_capture) {
        clear_audio(a->mono_capture, a->capture_samples * sizeof(int16_t));
        free(a->mono_capture);
    }
    a->stereo_capture = a->mono_capture = NULL;
}

esp_err_t bot_audio_start(bot_audio_t *a)
{
    if (!a || a != s_owner) return ESP_ERR_INVALID_ARG;
    LOCK(a);
    bool started = a->status.started;
    UNLOCK(a);
    if (started) return ESP_ERR_INVALID_STATE;
    esp_err_t afe_error = bot_audio_afe_open(&a->afe, a->partition_label, a->model_name);
    if (afe_error != ESP_OK) {
        bot_audio_afe_close(&a->afe);
        a->afe.wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        if (!a->afe.reason[0]) snprintf(a->afe.reason, sizeof(a->afe.reason), "%s", "Official AFE initialization unavailable");
    }
    /* Even a failed AFE leaves the official codec/manual-session path available. */
    a->capture_samples = a->afe.data ? a->afe.feed_samples : BOT_AUDIO_PLAYBACK_SAMPLES;
    esp_err_t err = codec_open(a);
    /* Start/restart also purges the BSP's process-lifetime TX DMA while muted. */
    if (err == ESP_OK) err = output_silence(a, a->dma_frames + BOT_AUDIO_PLAYBACK_SAMPLES);
    if (err == ESP_OK) {
        a->stereo_capture = calloc(a->capture_samples * 2, sizeof(int16_t));
        a->mono_capture = calloc(a->capture_samples, sizeof(int16_t));
        if (!a->stereo_capture || !a->mono_capture) err = ESP_ERR_NO_MEM;
    }
    if (err != ESP_OK) {
        release_capture_buffers(a);
        codec_cleanup(a);
        bot_audio_afe_close(&a->afe);
        LOCK(a);
        a->status.wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        set_reason_locked(a, err, "Codec/capture initialization failed; explicit local UI remains available");
        UNLOCK(a);
        emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH, .available = false,
            .error = err, .reason = "Codec/capture initialization failed"});
        publish_wake_status(a);
        return err;
    }
    xQueueReset(a->output_queue);
    xEventGroupSetBits(a->exits, ALL_EXIT);
    LOCK(a);
    memset(&a->status, 0, sizeof(a->status));
    a->status.started = true;
    a->status.codec_available = true;
    a->status.afe_running = a->afe.data != NULL;
    a->status.vad_available = false; /* a healthy microphone read is still required */
    a->status.night_wake_supported = true;
    a->status.wake_status = a->afe.wake_status;
    a->status.last_error = afe_error;
    snprintf(a->status.model_name, sizeof(a->status.model_name), "%s", a->model_name);
    snprintf(a->status.reason, sizeof(a->status.reason), "%s", a->afe.reason);
    a->run = true;
    a->capture_good = false;
    a->output_fault = false;
    a->capture_sequence = 0;
    a->afe_epoch = a->afe_reset_requested = 0;
    if (++a->gate_epoch == 0) ++a->gate_epoch;
    a->session = (bot_audio_token_t){0};
    a->afe_token = (bot_audio_token_t){0};
    a->playback_requested = a->playback_cancelled = a->playback_end_enqueued = false;
    a->last_wake_us = a->echo_tail_until_us = 0;
    UNLOCK(a);
    /* Emit initial state before capture can publish its first successful read. */
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH, .available = false,
        .reason = "Codec opened; awaiting first microphone read"});
    publish_wake_status(a);
    if (a->afe.data) {
        xEventGroupClearBits(a->exits, FETCH_EXIT);
        if (xTaskCreate(fetch_worker, "bot_afe_fetch", 8192, a,
                        CONFIG_BOT_AUDIO_WORKER_PRIORITY, &a->fetch_task) != pdPASS) {
            xEventGroupSetBits(a->exits, FETCH_EXIT);
            err = ESP_ERR_NO_MEM;
        }
    }
    if (err == ESP_OK) {
        xEventGroupClearBits(a->exits, PLAYBACK_EXIT);
        if (xTaskCreate(playback_worker, "bot_audio_out", 6144, a,
                        CONFIG_BOT_AUDIO_WORKER_PRIORITY, &a->output_task) != pdPASS) {
            xEventGroupSetBits(a->exits, PLAYBACK_EXIT);
            err = ESP_ERR_NO_MEM;
        }
    }
    if (err == ESP_OK) {
        xEventGroupClearBits(a->exits, CAPTURE_EXIT);
        if (xTaskCreate(capture_worker, "bot_audio_in", 8192, a,
                        CONFIG_BOT_AUDIO_WORKER_PRIORITY, &a->capture_task) != pdPASS) {
            xEventGroupSetBits(a->exits, CAPTURE_EXIT);
            err = ESP_ERR_NO_MEM;
        }
    }
    if (err != ESP_OK) {
        esp_err_t stop_error = bot_audio_stop(a, 3000);
        return stop_error == ESP_OK ? err : stop_error;
    }
    return ESP_OK;
}

esp_err_t bot_audio_set_session(bot_audio_t *a, const bot_audio_token_t *token, bool deliver_pcm)
{
    if (!a || (!token && deliver_pcm) || (token && (!token->epoch || !token->session_id))) return ESP_ERR_INVALID_ARG;
    LOCK(a);
    if (!a->run) {
        UNLOCK(a);
        return ESP_ERR_INVALID_STATE;
    }
    bool unchanged = a->status.session_open == (token != NULL) &&
        a->status.pcm_delivery_enabled == deliver_pcm && (!token || same_token(&a->session, token));
    if (unchanged) {
        UNLOCK(a);
        return ESP_OK;
    }
    bool same_playback_session = token && same_token(&a->session, token);
    if (++a->gate_epoch == 0) ++a->gate_epoch;
    a->status.session_open = token != NULL;
    a->status.pcm_delivery_enabled = token && deliver_pcm;
    a->session = token ? *token : (bot_audio_token_t){0};
    a->status.rx_discard_frames = token ? a->dma_frames + a->capture_samples : 0;
    a->afe_reset_requested = 0; /* fetch acknowledges only after fresh RX fence */
    if (!same_playback_session && a->playback_requested) {
        a->playback_cancelled = true;
        /* This GPIO write is bounded and independent of codec I2C/queue locks. */
        (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    }
    UNLOCK(a);
    return ESP_OK;
}

esp_err_t bot_audio_playback_begin(bot_audio_t *a, const bot_audio_token_t *token, uint32_t *out_id)
{
    if (!a || !token || !out_id) return ESP_ERR_INVALID_ARG;
    *out_id = 0;
    if (xSemaphoreTake(a->command_lock, 0) != pdTRUE) return ESP_ERR_TIMEOUT;
    LOCK(a);
    bool allowed = a->run && a->codec_open && !a->output_fault && a->status.session_open &&
        same_token(token, &a->session) && !a->playback_requested;
    if (allowed) {
        if (++a->next_playback_id == 0) ++a->next_playback_id;
        a->status.playback_id = a->next_playback_id;
        a->playback_token = *token;
        a->playback_requested = true;
        a->playback_cancelled = false;
        a->playback_end_enqueued = false;
        a->playback_begin_us = a->last_queued_us = esp_timer_get_time();
        *out_id = a->status.playback_id;
    }
    UNLOCK(a);
    xSemaphoreGive(a->command_lock);
    return allowed ? ESP_OK : ESP_ERR_INVALID_STATE;
}

static esp_err_t enqueue_playback(bot_audio_t *a, const playback_frame_t *frame)
{
    if (xSemaphoreTake(a->command_lock, 0) != pdTRUE) return ESP_ERR_TIMEOUT;
    LOCK(a);
    bool allowed = a->run && a->playback_requested && !a->playback_cancelled &&
        !a->playback_end_enqueued && a->status.playback_id == frame->id;
    UNLOCK(a);
    esp_err_t err = allowed ? ESP_OK : ESP_ERR_INVALID_STATE;
    if (allowed) {
        if (xQueueSend(a->output_queue, frame, 0) != pdTRUE) err = ESP_ERR_TIMEOUT;
        else {
            LOCK(a);
            if (a->status.playback_id == frame->id) {
                a->last_queued_us = esp_timer_get_time();
                if (frame->end) a->playback_end_enqueued = true;
            }
            UNLOCK(a);
        }
    }
    xSemaphoreGive(a->command_lock);
    return err;
}

esp_err_t bot_audio_playback_write(bot_audio_t *a, uint32_t id, const int16_t *samples, size_t count)
{
    if (!a || !id || !samples || !count || count > BOT_AUDIO_PLAYBACK_SAMPLES) return ESP_ERR_INVALID_ARG;
    playback_frame_t frame = {.id = id, .count = (uint16_t)count};
    memcpy(frame.samples, samples, count * sizeof(int16_t));
    esp_err_t err = enqueue_playback(a, &frame);
    clear_audio(&frame, sizeof(frame));
    return err;
}

esp_err_t bot_audio_playback_end(bot_audio_t *a, uint32_t id)
{
    if (!a || !id) return ESP_ERR_INVALID_ARG;
    playback_frame_t frame = {.id = id, .end = true};
    return enqueue_playback(a, &frame);
}

void bot_audio_playback_cancel(bot_audio_t *a, uint32_t id)
{
    if (!a) return;
    LOCK(a);
    if (a->playback_requested && (!id || id == a->status.playback_id)) {
        a->playback_cancelled = true;
        if (a->codec_open) (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    }
    UNLOCK(a);
}

esp_err_t bot_audio_stop(bot_audio_t *a, uint32_t timeout_ms)
{
    if (!a || a != s_owner || timeout_ms > 10000) return ESP_ERR_INVALID_ARG;
    LOCK(a);
    if (!a->status.started) {
        UNLOCK(a);
        return ESP_OK;
    }
    a->run = false;
    a->status.session_open = false;
    a->status.pcm_delivery_enabled = false;
    a->status.rx_discard_frames = 0;
    a->afe_reset_requested = 0;
    if (++a->gate_epoch == 0) ++a->gate_epoch;
    a->session = (bot_audio_token_t){0};
    a->playback_cancelled = a->playback_requested;
    (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    UNLOCK(a);
    EventBits_t bits = xEventGroupWaitBits(a->exits, ALL_EXIT, pdFALSE, pdTRUE, pdMS_TO_TICKS(timeout_ms));
    if ((bits & ALL_EXIT) != ALL_EXIT) return ESP_ERR_TIMEOUT;
    /* No worker can touch the device, model, or sample buffers after its exit bit. */
    codec_cleanup(a);
    bot_audio_afe_close(&a->afe);
    release_capture_buffers(a);
    xQueueReset(a->output_queue);
    a->capture_task = a->fetch_task = a->output_task = NULL;
    LOCK(a);
    a->status.started = false;
    a->status.capture_running = a->status.afe_running = a->status.playback_running = false;
    a->status.codec_available = a->status.vad_available = a->status.wake_available = false;
    a->status.night_wake_supported = false;
    a->status.wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
    a->playback_requested = a->playback_cancelled = a->playback_end_enqueued = false;
    set_reason_locked(a, ESP_OK, "Audio stopped; gates closed and amplifier off");
    UNLOCK(a);
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH, .available = false, .reason = "Audio stopped"});
    publish_wake_status(a);
    return ESP_OK;
}

esp_err_t bot_audio_destroy(bot_audio_t *a, uint32_t timeout_ms)
{
    if (!a || a != s_owner) return ESP_ERR_INVALID_ARG;
    esp_err_t err = bot_audio_stop(a, timeout_ms);
    if (err != ESP_OK) return err;
    vQueueDelete(a->output_queue);
    vSemaphoreDelete(a->command_lock);
    vSemaphoreDelete(a->feed_lock);
    vEventGroupDelete(a->exits);
    clear_audio(a, sizeof(*a));
    free(a);
    s_owner = NULL;
    return ESP_OK;
}

void bot_audio_get_status(bot_audio_t *a, bot_audio_status_t *out)
{
    if (!out) return;
    if (!a) {
        memset(out, 0, sizeof(*out));
        out->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        out->last_error = ESP_ERR_INVALID_ARG;
        return;
    }
    LOCK(a);
    *out = a->status;
    UNLOCK(a);
}

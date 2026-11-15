/* SPDX-License-Identifier: MIT
 * BSP/esp_codec_dev construction follows Espressif's ESP32-S31-Korvo-1
 * bsp_audio.c. AFE feed/fetch follows the official esp-skainet examples.
 * Local policies (permission gates, tokens, cancellation) are project code.
 */
#include "bot_audio.h"
#include "bot_audio_afe.h"
#include "bot_audio_start_buffer.h"
#include "followup_vad.h"
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "sdkconfig.h"
#include "bsp/esp32_s31_korvo_1.h"
#include "driver/gpio.h"
#include "driver/i2s_std.h"
#include "driver/i2s_tdm.h"
#include "esp_apa_doa.h"
#include "esp_codec_dev.h"
#include "esp_codec_dev_defaults.h"
#include "es8389_codec.h"
#include "esp_timer.h"
#include "esp_log.h"
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
#define PLAYBACK_EXIT BIT2
#define ALL_EXIT (CAPTURE_EXIT | PLAYBACK_EXIT)
#define PLAYBACK_STARVE_US INT64_C(2000000)
#define PLAYBACK_PROGRESS_US INT64_C(1000000)
#define MAX_OUTPUT_VOLUME 60
#define SOUND_EVENT_INTERVAL_US INT64_C(1200000)
#define SOUND_DIRECTION_HOLD_US INT64_C(700000)
#define SOUND_MIN_PEAK 200
#define SOUND_MIN_QUALITY 3.0f
#define SOUND_PEAK_HOLD_US INT64_C(300000)
#define SOUND_IMPULSE_HOLD_US INT64_C(400000)
#define AFE_RECOVERY_DELAY_US INT64_C(1500000)
#define START_ONSET_MARGIN_US INT64_C(256000)
#define FOLLOWUP_ENERGY_HOLD_US INT64_C(320000) /* AFE onset/delay plus a fetch frame */
#define START_REPLAY_FRAMES_PER_CAPTURE 4U

static const char *TAG = "bot_audio";

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
    TaskHandle_t capture_task, output_task;
    bot_audio_afe_t afe;
    bot_audio_start_buffer_t start_buffer;
    esp_codec_dev_handle_t codec;
    const audio_codec_if_t *codec_if;
    const audio_codec_ctrl_if_t *ctrl_if;
    const audio_codec_gpio_if_t *gpio_if;
    const audio_codec_data_if_t *data_if;
    i2s_chan_handle_t i2s_tx, i2s_rx;
    bool i2s_tx_enabled, i2s_rx_enabled;
    esp_apa_doa_handle_t doa;
    bool passive_speech, last_sound_direction_valid;
    int64_t last_sound_event_us, last_direction_us, last_loud_us, last_impulse_us, ambient_ready_us;
    float ambient_noise;
    bool codec_open;
    int16_t *stereo_capture, *mono_capture;
    size_t capture_samples;
    uint32_t dma_frames;
    uint64_t capture_sequence;
    uint32_t gate_epoch;
    bot_audio_token_t session;
    bot_audio_session_options_t session_options;
    uint32_t buffer_generation, buffer_owner_generation;
    int64_t buffer_protect_from_us;
    bool buffer_overflow_latched;
    uint32_t afe_reset_requested, afe_epoch;
    uint32_t keyword_epoch; /* capture worker's last recognizer permission fence */
    bot_audio_token_t afe_token;
    bot_audio_token_t afe_speech_token; /* original START lease, independent of later fences */
    bool afe_speech, waiting_for_followup;
    uint32_t afe_speech_epoch;
    size_t afe_pending_samples; /* capture worker only; cleared with native rings */
    int64_t afe_fetch_wait_since_us;
    int64_t raw_speech_until_us; /* raw-energy observation, never retained PCM */
    uint32_t keyword_policy_epoch, controls_attempt_revision;
    bool keyword_policy_commands, retry_afe_requested;
    int64_t afe_recovery_due_us;
    bool playback_requested, playback_cancelled, playback_end_enqueued;
    bool echo_reset_pending;
    uint32_t echo_discard_frames;
    uint32_t next_playback_id;
    bot_audio_token_t playback_token;
    int64_t playback_begin_us, last_queued_us, echo_tail_until_us, last_wake_us;
};

/* Lifecycle operations are serialized by the application's owner task. */
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

/* Capture worker only. Permission loss is an end edge too; never silently
 * clear START, nor label its END with a newly installed session token. */
static void end_speech(bot_audio_t *a)
{
    if (!a->afe_speech) return;
    a->afe_speech = false;
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_VAD_ENDED,
        .token = a->afe_speech_token, .echo_safe = false});
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

/* Called with the owner lock held. The post-playback RX/AFE fence prevents
 * buffered speaker audio from becoming eligible just because wall time passed. */
static bool echo_blocked_locked(const bot_audio_t *a, int64_t now)
{
    return a->playback_requested || now < a->echo_tail_until_us ||
           a->echo_reset_pending || a->echo_discard_frames != 0;
}

static void reconcile_speech_permission(bot_audio_t *a)
{
    LOCK(a);
    bool allowed = a->run && a->capture_good && a->status.afe_running &&
        a->status.session_open && same_token(&a->session, &a->afe_speech_token) &&
        !a->status.rx_discard_frames && !echo_blocked_locked(a, esp_timer_get_time()) &&
        !a->status.command_consumed && !a->buffer_overflow_latched &&
        (a->session_options.retain_start || a->session_options.deliver_pcm || a->session_options.local_commands);
    UNLOCK(a);
    if (!allowed) end_speech(a);
}

static bool output_muted_locked(const bot_audio_t *a)
{
    return a->status.controls_requested.muted || !a->status.controls_requested.volume_percent ||
           a->status.controls_applied.muted || !a->status.controls_applied.volume_percent;
}

static uint32_t elapsed_us(int64_t start)
{
    int64_t elapsed = esp_timer_get_time() - start;
    return elapsed > UINT32_MAX ? UINT32_MAX : elapsed > 0 ? (uint32_t)elapsed : 0;
}

static void set_capture_ready(bot_audio_t *a, bool ready, bot_audio_token_t token)
{
    LOCK(a);
    ready = ready && a->run && a->status.session_open && same_token(&a->session, &token) &&
            !a->status.command_consumed && !a->buffer_overflow_latched &&
            !echo_blocked_locked(a, esp_timer_get_time());
    bool changed = a->status.capture_ready != ready;
    a->status.capture_ready = ready;
    if (changed && ready) a->status.last_capture_ready_us = esp_timer_get_time();
    UNLOCK(a);
    if (changed) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_CAPTURE_READY,
        .token = token, .available = ready});
}

/* Only the capture worker manipulates this ring; callers revoke it by changing
 * buffer_generation. No callback may reassign an old frame to a new token. */
static void clear_start_buffer(bot_audio_t *a)
{
    bot_audio_start_buffer_clear(&a->start_buffer);
    a->buffer_protect_from_us = 0;
    LOCK(a);
    a->status.buffered_samples = 0;
    a->buffer_owner_generation = a->buffer_generation;
    UNLOCK(a);
}

static void publish_wake_status(bot_audio_t *a)
{
    bot_audio_status_t status;
    bot_audio_get_status(a, &status);
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_WAKE_STATUS,
        .wake_status = status.wake_status, .available = status.wake_available,
        .wake_backend = status.wake_backend,
        .error = status.last_error, .reason = status.reason});
}

static void afe_failed(bot_audio_t *a, esp_err_t error, const char *text)
{
    end_speech(a);
    LOCK(a);
    a->status.afe_running = false;
    a->status.vad_available = false;
    a->status.wake_available = false;
    a->status.wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
    if (!a->afe_recovery_due_us) a->afe_recovery_due_us = esp_timer_get_time() + AFE_RECOVERY_DELAY_US;
    set_reason_locked(a, error, text);
    UNLOCK(a);
    publish_wake_status(a);
}

static void apply_controls(bot_audio_t *a)
{
    LOCK(a);
    uint32_t revision = a->status.controls_revision;
    bot_voice_settings_t settings = a->status.controls_requested;
    bool due = revision != a->controls_attempt_revision;
    UNLOCK(a);
    if (!due) return;
    /* This owner is the only runtime caller of codec/model configuration APIs.
     * Playback writes use the same owned device; no UI thread touches codec I2C. */
    esp_err_t volume_error = esp_codec_dev_set_out_vol(a->codec, settings.volume_percent);
    esp_err_t err = volume_error;
    if (err == ESP_OK && a->afe.keyword.data)
        err = bot_audio_keyword_set_policy(&a->afe.keyword, a->afe.keyword.commands_enabled,
                                           settings.wake_threshold_percent);
    if (err == ESP_OK && a->afe.wake_backend == BOT_AUDIO_WAKE_BACKEND_WAKENET) {
        if (!a->afe.iface || !a->afe.iface->set_wakenet_threshold ||
            a->afe.iface->set_wakenet_threshold(a->afe.data, 1,
                settings.wake_threshold_percent / 100.0f) != 1) err = ESP_FAIL;
    }
    LOCK(a);
    a->controls_attempt_revision = revision;
    a->status.controls_error = err;
    /* A recognizer threshold failure must not retain an old output mute or
     * volume after the codec accepted the new level. Full revision still fails. */
    if (volume_error == ESP_OK) {
        a->status.controls_applied.volume_percent = settings.volume_percent;
        a->status.controls_applied.muted = settings.muted;
        a->config.output_volume = settings.volume_percent;
    }
    a->status.local_commands_available = a->afe.keyword.data && a->afe.keyword.available;
    if (err == ESP_OK) {
        a->status.controls_applied = settings;
        a->status.controls_applied_revision = revision;
        a->config.output_volume = settings.volume_percent;
        if (a->status.afe_running && a->afe.wake_backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7)
            a->status.wake_status = a->afe.keyword.available ? BOT_AUDIO_WAKE_READY : BOT_AUDIO_WAKE_UNAVAILABLE;
    }
    /* Do not use esp_codec_dev_set_out_mute: ES8389 1.5.11 mutes the ADC. */
    if (output_muted_locked(a)) (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    UNLOCK(a);
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_CONTROLS_APPLIED,
        .available = err == ESP_OK, .error = err, .controls_revision = revision});
}

static void recover_afe(bot_audio_t *a)
{
    LOCK(a);
    bool requested = a->retry_afe_requested;
    bool due = a->afe.data && !echo_blocked_locked(a, esp_timer_get_time()) &&
        (requested || (!a->status.afe_running && a->afe_recovery_due_us &&
         esp_timer_get_time() >= a->afe_recovery_due_us &&
         a->status.afe_recovery_attempts < CONFIG_BOT_AUDIO_AFE_RECOVERY_LIMIT));
    if (due) {
        a->retry_afe_requested = false;
        a->status.afe_recovery_attempts++;
        a->afe_recovery_due_us = esp_timer_get_time() + AFE_RECOVERY_DELAY_US;
        a->buffer_generation++;
    }
    UNLOCK(a);
    if (!due) return;
    clear_start_buffer(a);
    int reset = a->afe.iface->reset_buffer ? a->afe.iface->reset_buffer(a->afe.data) : -1;
    int vad = a->afe.iface->reset_vad ? a->afe.iface->reset_vad(a->afe.data) : -1;
    bot_audio_keyword_reset(&a->afe.keyword);
    a->keyword_policy_epoch = 0;
    end_speech(a);
    a->afe_pending_samples = 0;
    a->afe_fetch_wait_since_us = 0;
    a->raw_speech_until_us = 0;
    LOCK(a);
    if (reset == 1 && vad == 1) {
        a->status.afe_running = true;
        a->status.wake_status = a->afe.wake_status;
        a->status.afe_recoveries++;
        a->afe_recovery_due_us = 0;
        a->afe_epoch = 0;
        if (a->status.session_open) a->status.rx_discard_frames = a->dma_frames + a->capture_samples;
        set_reason_locked(a, ESP_OK, "AFE reset recovered; awaiting fresh microphone/AFE results");
    } else {
        a->status.afe_running = false;
        set_reason_locked(a, ESP_FAIL, "AFE reset failed; manual capture remains available");
    }
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
    if (a->data_if) audio_codec_delete_data_if(a->data_if);
    a->data_if = NULL;
    if (a->i2s_rx) {
        if (a->i2s_rx_enabled) (void)i2s_channel_disable(a->i2s_rx);
        (void)i2s_del_channel(a->i2s_rx);
    }
    if (a->i2s_tx) {
        if (a->i2s_tx_enabled) (void)i2s_channel_disable(a->i2s_tx);
        (void)i2s_del_channel(a->i2s_tx);
    }
    a->i2s_rx = a->i2s_tx = NULL;
    a->i2s_rx_enabled = a->i2s_tx_enabled = false;
    /* The shared BSP I2C bus is owned by the platform. */
}

static esp_err_t codec_open(bot_audio_t *a)
{
    esp_err_t err = bsp_i2c_init();
    if (err != ESP_OK) return err;
    /* Espressif's S31-Korvo-1 APA example uses four 16-bit TDM slots and
     * selects slots 0 and 2: the two physical microphones. codec_dev packs
     * the selected slots into a two-channel interleaved capture buffer. */
    i2s_tdm_config_t bus = {
        .clk_cfg = I2S_TDM_CLK_DEFAULT_CONFIG(BOT_AUDIO_SAMPLE_RATE),
        .slot_cfg = I2S_TDM_PHILIPS_SLOT_DEFAULT_CONFIG(I2S_DATA_BIT_WIDTH_16BIT,
                    I2S_SLOT_MODE_STEREO, I2S_TDM_SLOT0 | I2S_TDM_SLOT2),
        .gpio_cfg = {.mclk = BSP_I2S_MCLK, .bclk = BSP_I2S_SCLK,
            .ws = BSP_I2S_LCLK, .dout = BSP_I2S_DOUT, .din = BSP_I2S_DSIN},
    };
    bus.slot_cfg.total_slot = 4;
    i2s_chan_config_t dma = I2S_CHANNEL_DEFAULT_CONFIG(CONFIG_BSP_I2S_NUM, I2S_ROLE_MASTER);
    dma.auto_clear = true;
    a->dma_frames = dma.dma_desc_num * dma.dma_frame_num;
    if ((err = i2s_new_channel(&dma, &a->i2s_tx, &a->i2s_rx)) != ESP_OK) return err;
    if ((err = i2s_channel_init_tdm_mode(a->i2s_tx, &bus)) != ESP_OK) return err;
    if ((err = i2s_channel_init_tdm_mode(a->i2s_rx, &bus)) != ESP_OK) return err;
    if ((err = i2s_channel_enable(a->i2s_tx)) != ESP_OK) return err;
    a->i2s_tx_enabled = true;
    if ((err = i2s_channel_enable(a->i2s_rx)) != ESP_OK) return err;
    a->i2s_rx_enabled = true;
    audio_codec_i2s_cfg_t i2s = {.port = CONFIG_BSP_I2S_NUM,
        .rx_handle = a->i2s_rx, .tx_handle = a->i2s_tx};
    a->data_if = audio_codec_new_i2s_data(&i2s);
    if (!a->data_if) return ESP_ERR_NO_MEM;
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
        .no_dac_ref = false, /* 64 BCLKs/frame; slots 0 and 2 select microphones */
        .hw_gain = {.pa_voltage = 5.0f, .codec_dac_voltage = 3.3f},
    };
    a->codec_if = es8389_codec_new(&device);
    if (!a->codec_if) return ESP_ERR_NO_MEM;
    esp_codec_dev_cfg_t owner = {
        .dev_type = ESP_CODEC_DEV_TYPE_IN_OUT,
        .codec_if = a->codec_if, .data_if = a->data_if,
    };
    a->codec = esp_codec_dev_new(&owner);
    if (!a->codec) return ESP_ERR_NO_MEM;
    esp_codec_dev_sample_info_t format = {
        .sample_rate = BOT_AUDIO_SAMPLE_RATE, .bits_per_sample = 16, .channel = 4,
        .channel_mask = ESP_CODEC_DEV_MAKE_CHANNEL_MASK(0) | ESP_CODEC_DEV_MAKE_CHANNEL_MASK(2),
    };
    err = esp_codec_dev_open(a->codec, &format);
    if (err != ESP_OK) return err;
    a->codec_open = true;
    /* In codec_dev 1.5.11, ES8389 set_mute writes ADC register 0x20.
     * Muting the speaker through that API would also silence local wake.
     * Keep ADC enabled; the board's PA gate owns physical speaker silence. */
    err = gpio_set_level(BSP_POWER_AMP_IO, 0);
    if (err == ESP_OK) err = esp_codec_dev_set_out_vol(a->codec, a->config.output_volume);
    if (err == ESP_OK) err = esp_codec_dev_set_in_gain(a->codec, a->config.microphone_gain_db);
    if (err == ESP_OK) ESP_LOGI(TAG, "ES8389 TDM: 16 kHz, four slots, microphone slots 0/2");
    return err;
}

static void apply_afe_fence(bot_audio_t *a);
static bool fetch_frame(bot_audio_t *a);

/* Called only by the capture owner. APA's valid/quality describe the estimate,
 * not physical installation calibration or whether the source is a person. */
static void process_ambient(bot_audio_t *a, int64_t now, uint16_t peak0, uint16_t peak1,
                            uint16_t mean0, uint16_t mean1)
{
    LOCK(a);
    bool usable = a->run && a->capture_good && a->status.afe_running && now >= a->ambient_ready_us &&
                  now - a->status.last_afe_result_us < 500000 &&
                  !echo_blocked_locked(a, now);
    bool vad_speech = a->passive_speech;
    UNLOCK(a);
    const float level = mean0 > mean1 ? mean0 : mean1;
    const uint16_t peak = peak0 > peak1 ? peak0 : peak1;
    if (a->ambient_noise < 1.0f) a->ambient_noise = fmaxf(5.0f, level);
    /* Speech comes from ESP-SR. A short energy increase separately covers
     * knocks and other clear non-speech sounds; it is not a speech classifier. */
    if (usable && peak >= SOUND_MIN_PEAK) a->last_loud_us = now;
    if (usable && peak >= 300 && level >= fmaxf(60.0f, 4.0f * a->ambient_noise))
        a->last_impulse_us = now;
    a->ambient_noise += (level - a->ambient_noise) * (level < a->ambient_noise ? 0.05f : 0.005f);
    bool speech = usable && vad_speech && a->last_loud_us && now - a->last_loud_us <= SOUND_PEAK_HOLD_US;
    bool impulse = usable && a->last_impulse_us && now - a->last_impulse_us <= SOUND_IMPULSE_HOLD_US;
    bool activity = speech || impulse;
    esp_apa_doa_result_t result = {0};
    esp_err_t err = a->doa ? esp_apa_doa_process(a->doa, a->stereo_capture,
                            a->capture_samples * 2, activity ? &result : NULL) : ESP_ERR_NOT_SUPPORTED;
    if (activity && err == ESP_OK && result.valid && isfinite(result.azimuth_deg) &&
        isfinite(result.quality) && result.azimuth_deg >= 0 && result.azimuth_deg <= 180 &&
        result.quality >= SOUND_MIN_QUALITY) {
        LOCK(a);
        a->status.azimuth_deg = result.azimuth_deg;
        a->status.direction_quality = result.quality;
        a->status.direction_estimates++;
        UNLOCK(a);
        a->last_direction_us = now;
    }
    bool direction_valid = activity && a->last_direction_us &&
                           now - a->last_direction_us <= SOUND_DIRECTION_HOLD_US;
    LOCK(a);
    a->status.ambient_activity = activity;
    a->status.ambient_speech = speech;
    a->status.microphone_mean = mean0;
    a->status.microphone_mean_2 = mean1;
    a->status.ambient_noise_mean = (uint16_t)fminf(a->ambient_noise, 32768.0f);
    a->status.direction_valid = direction_valid;
    float angle = a->status.azimuth_deg, quality = a->status.direction_quality;
    UNLOCK(a);
    if (!activity) {
        a->last_direction_us = 0;
        a->last_sound_direction_valid = false;
        return;
    }
    bool due = !a->last_sound_event_us || now - a->last_sound_event_us >= SOUND_EVENT_INTERVAL_US;
    if (!due && !(direction_valid && !a->last_sound_direction_valid)) return;
    a->last_sound_event_us = now;
    a->last_sound_direction_valid = direction_valid;
    LOCK(a);
    a->status.ambient_events++;
    UNLOCK(a);
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_AMBIENT_SOUND,
        .observed_us = now, .direction_valid = direction_valid,
        .speech = speech, .azimuth_deg = angle, .direction_quality = quality});
}

static bool deliver_pcm_frame(bot_audio_t *a, const bot_audio_pcm_t *frame, bool replay)
{
    LOCK(a);
    bool allowed = a->run && a->status.session_open && a->status.capture_ready && a->status.pcm_delivery_enabled &&
        !a->status.command_consumed && !a->buffer_overflow_latched &&
        same_token(&a->session, &frame->token) && !echo_blocked_locked(a, esp_timer_get_time());
    if (allowed) {
        a->status.delivered_pcm_frames++;
        if (replay) a->status.replayed_frames++;
    }
    UNLOCK(a);
    if (allowed && a->config.on_pcm) a->config.on_pcm(a->config.context, frame);
    return allowed;
}

static void process_session_pcm(bot_audio_t *a, const bot_audio_pcm_t *frame,
                                 uint32_t read_epoch, bool eligible)
{
    LOCK(a);
    bool fresh = eligible && a->run && a->status.session_open && a->status.capture_ready && read_epoch == a->gate_epoch &&
        same_token(&a->session, &frame->token) && !a->status.command_consumed &&
        !a->buffer_overflow_latched && !echo_blocked_locked(a, esp_timer_get_time());
    bool retain = fresh && a->session_options.retain_start && a->start_buffer.capacity;
    bool deliver = fresh && a->status.pcm_delivery_enabled && a->config.on_pcm;
    bool invalidated = a->buffer_owner_generation != a->buffer_generation;
    UNLOCK(a);
    if (invalidated || !fresh || !retain) clear_start_buffer(a);
    if (!fresh || (!retain && !deliver)) {
        LOCK(a);
        a->status.gated_pcm_frames++;
        UNLOCK(a);
        return;
    }
    unsigned replay_budget = START_REPLAY_FRAMES_PER_CAPTURE;
    while (deliver && replay_budget && a->start_buffer.count) {
        const bot_audio_pcm_t *buffered = bot_audio_start_buffer_peek(&a->start_buffer);
        if (!deliver_pcm_frame(a, buffered, true)) {
            clear_start_buffer(a);
            return;
        }
        bot_audio_start_buffer_pop(&a->start_buffer);
        replay_budget--;
    }
    if (deliver && !a->start_buffer.count) {
        (void)deliver_pcm_frame(a, frame, false);
    } else if (retain) {
        /* Recheck after callbacks; a local stop/command may have revoked this
         * lease while earlier buffered frames were being handed off. */
        LOCK(a);
        fresh = a->run && read_epoch == a->gate_epoch && a->status.session_open && a->status.capture_ready &&
            same_token(&a->session, &frame->token) && a->session_options.retain_start &&
            !a->status.command_consumed && !a->buffer_overflow_latched &&
            !echo_blocked_locked(a, esp_timer_get_time());
        UNLOCK(a);
        if (!fresh) { clear_start_buffer(a); return; }
        bool protected_drop = false;
        size_t evicted = bot_audio_start_buffer_push(&a->start_buffer, frame,
                            a->buffer_protect_from_us, &protected_drop);
        if (protected_drop) {
            LOCK(a);
            a->status.buffered_drops += evicted;
            a->buffer_overflow_latched = true;
            a->status.pcm_delivery_enabled = false;
            a->status.retain_start_enabled = false;
            a->buffer_generation++;
            UNLOCK(a);
            clear_start_buffer(a);
            set_capture_ready(a, false, frame->token);
            /* Do not submit a known-truncated utterance as if it were complete.
             * Runtime cancels this turn and may offer an explicit retry. */
            emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_BUFFER_OVERFLOW,
                .token = frame->token, .error = ESP_ERR_NO_MEM, .available = false,
                .reason = "Authorized start buffer filled before transport readiness; retry this turn"});
            return;
        }
    }
    LOCK(a);
    a->status.buffered_samples = (uint32_t)(a->start_buffer.count * a->capture_samples);
    if (a->status.buffered_samples > a->status.buffer_high_water)
        a->status.buffer_high_water = a->status.buffered_samples;
    if (!deliver) a->status.gated_pcm_frames++;
    UNLOCK(a);
}

static void capture_worker(void *context)
{
    bot_audio_t *a = context;
    const size_t bytes = a->capture_samples * 2 * sizeof(int16_t);
    int64_t last_stack_sample_us = 0;
    LOCK(a);
    a->status.capture_running = true;
    UNLOCK(a);
    while (running(a)) {
        if (esp_timer_get_time() - last_stack_sample_us >= 5000000) {
            last_stack_sample_us = esp_timer_get_time();
            const uint32_t remaining = uxTaskGetStackHighWaterMark(NULL);
            LOCK(a); a->status.capture_stack_free = remaining; UNLOCK(a);
        }
        reconcile_speech_permission(a);
        apply_controls(a);
        recover_afe(a);
        LOCK(a);
        bool invalidated = a->buffer_owner_generation != a->buffer_generation;
        UNLOCK(a);
        if (invalidated) clear_start_buffer(a);
        apply_afe_fence(a);
        /* A local response can outlive a transport session transition. Both
         * streams use this same microphone/speaker suppression boundary. */
        LOCK(a);
        bool reset_echo = a->echo_reset_pending && !a->playback_requested &&
                          esp_timer_get_time() >= a->echo_tail_until_us && !a->echo_discard_frames;
        bool reset_afe = a->status.afe_running;
        UNLOCK(a);
        if (reset_echo && xSemaphoreTake(a->feed_lock, 0) == pdTRUE) {
            int reset = 1, vad_reset = 1;
            if (reset_afe) {
                reset = a->afe.iface->reset_buffer ? a->afe.iface->reset_buffer(a->afe.data) : -1;
                vad_reset = a->afe.iface->reset_vad ? a->afe.iface->reset_vad(a->afe.data) : -1;
            }
            bot_audio_keyword_reset(&a->afe.keyword);
            a->afe_pending_samples = 0;
            a->afe_fetch_wait_since_us = 0;
            a->raw_speech_until_us = 0;
            end_speech(a);
            if (reset != 1 || vad_reset != 1)
                afe_failed(a, ESP_FAIL, "Official AFE post-playback reset failed; local detector disabled");
            LOCK(a);
            if (!a->playback_requested && esp_timer_get_time() >= a->echo_tail_until_us &&
                !a->echo_discard_frames) a->echo_reset_pending = false;
            UNLOCK(a);
            xSemaphoreGive(a->feed_lock);
        }
        LOCK(a);
        uint32_t read_epoch = a->gate_epoch;
        uint64_t sequence = ++a->capture_sequence;
        UNLOCK(a);
        int64_t started = esp_timer_get_time();
        memset(a->stereo_capture, 0, bytes);
        esp_err_t err = esp_codec_dev_read(a->codec, a->stereo_capture, (int)bytes);
        int64_t observed = esp_timer_get_time();
        uint32_t read_time = elapsed_us(started);
        LOCK(a);
        a->status.read_us = read_time;
        if (read_time > a->status.read_max_us) a->status.read_max_us = read_time;
        UNLOCK(a);
        if (err != ESP_OK) {
            a->raw_speech_until_us = 0;
            end_speech(a);
            bot_audio_keyword_reset(&a->afe.keyword);
            clear_start_buffer(a);
            LOCK(a);
            bool changed = a->capture_good;
            a->capture_good = false;
            a->status.vad_available = false;
            a->status.wake_available = false;
            a->status.capture_errors++;
            set_reason_locked(a, err, "Codec capture failed; no PCM emitted");
            bot_audio_token_t failed_token = a->session;
            UNLOCK(a);
            set_capture_ready(a, false, failed_token);
            if (changed) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH,
                .error = err, .available = false, .reason = "Codec capture failed"});
            vTaskDelay(pdMS_TO_TICKS(20));
            continue;
        }
        /* The primary mic remains the only AFE/authorized PCM input. */
        uint16_t peak = 0, peak2 = 0;
        uint32_t sum = 0, sum2 = 0;
        for (size_t i = 0; i < a->capture_samples; ++i) {
            int32_t sample = a->stereo_capture[2 * i];
            a->mono_capture[i] = (int16_t)sample;
            uint16_t magnitude = (uint16_t)(sample < 0 ? -sample : sample);
            if (magnitude > peak) peak = magnitude;
            sum += magnitude;
            sample = a->stereo_capture[2 * i + 1];
            magnitude = (uint16_t)(sample < 0 ? -sample : sample);
            if (magnitude > peak2) peak2 = magnitude;
            sum2 += magnitude;
        }
        LOCK(a);
        bool health_changed = !a->capture_good;
        a->capture_good = true;
        a->status.capture_frames++;
        a->status.microphone_peak = peak;
        a->status.microphone_peak_2 = peak2;
        a->status.last_capture_us = observed;
        bool afe_fresh = a->status.afe_running && a->status.afe_frames > 0 &&
                         observed - a->status.last_afe_result_us < 500000;
        a->status.vad_available = afe_fresh;
        a->status.wake_available = afe_fresh &&
                                  a->status.wake_status == BOT_AUDIO_WAKE_READY;
        if (a->playback_requested || observed < a->echo_tail_until_us) {
            a->echo_reset_pending = true;
            a->echo_discard_frames = a->dma_frames + a->capture_samples;
        } else if (a->echo_discard_frames) {
            a->echo_discard_frames = a->echo_discard_frames > a->capture_samples ?
                                    a->echo_discard_frames - a->capture_samples : 0;
        }
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
        bot_audio_token_t token = a->session;
        bool capture_ready = eligible && (!a->status.afe_running || a->afe_epoch == a->gate_epoch) &&
            (a->session_options.retain_start || a->session_options.deliver_pcm || a->session_options.local_commands);
        UNLOCK(a);
        set_capture_ready(a, capture_ready, token);
        /* Only authorized, fence-cleared raw input can open a follow-up.
         * Hold energy briefly to align it with AFE's delayed VAD decision;
         * AGC-amplified noise or earlier playback cannot authorize an onset. */
        if (capture_ready && bot_audio_pcm_speech_energy(a->mono_capture, a->capture_samples))
            a->raw_speech_until_us = observed + FOLLOWUP_ENERGY_HOLD_US;
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
            int64_t afe_started = esp_timer_get_time();
            if (feed_allowed) result = a->afe.iface->feed(a->afe.data, a->mono_capture);
            /* WebRTC AGC may use a different native feed/fetch frame size.
             * Keep one capture/reset owner, accumulating only accepted input;
             * never block that owner waiting for input it has not fed yet. */
            /* A full native ring may reject a feed with zero, rather than
             * a negative error. Still fetch to drain queued work; skipping
             * that read would make temporary backpressure permanent. */
            if (feed_allowed && result == 0) {
                bot_audio_keyword_reset(&a->afe.keyword);
                LOCK(a);
                a->status.afe_feed_rejections++;
                UNLOCK(a);
            }
            if (feed_allowed && result > 0) a->afe_pending_samples += a->capture_samples;
            for (unsigned drained = 0; feed_allowed && result >= 0 && drained < 32 &&
                 a->afe_pending_samples >= a->afe.fetch_samples; ++drained) {
                if (!fetch_frame(a)) break;
                a->afe_pending_samples -= a->afe.fetch_samples;
            }
            uint32_t afe_time = elapsed_us(afe_started);
            LOCK(a);
            a->status.afe_us = afe_time;
            if (afe_time > a->status.afe_max_us) a->status.afe_max_us = afe_time;
            UNLOCK(a);
            xSemaphoreGive(a->feed_lock);
            if (result < 0) afe_failed(a, ESP_FAIL, "Official AFE feed failed");
        }
        process_ambient(a, esp_timer_get_time(), peak, peak2,
                        (uint16_t)(sum / a->capture_samples), (uint16_t)(sum2 / a->capture_samples));
        bot_audio_pcm_t frame = {.samples = a->mono_capture,
            .sample_count = a->capture_samples, .sample_rate = BOT_AUDIO_SAMPLE_RATE,
            .sequence = sequence, .read_started_us = started, .observed_us = observed,
            .token = token, .enhanced = false, .echo_cancelled = false};
        process_session_pcm(a, &frame, read_epoch, eligible);
        uint32_t processing = elapsed_us(observed);
        LOCK(a);
        a->status.processing_us = processing;
        if (processing > a->status.processing_max_us) a->status.processing_max_us = processing;
        if (processing > a->capture_samples * 1000000U / BOT_AUDIO_SAMPLE_RATE)
            a->status.capture_overruns++;
        UNLOCK(a);
    }
    end_speech(a);
    clear_audio(a->stereo_capture, bytes);
    clear_audio(a->mono_capture, a->capture_samples * sizeof(int16_t));
    clear_start_buffer(a);
    LOCK(a);
    a->status.capture_running = false;
    a->status.afe_running = false;
    a->capture_good = false;
    a->status.vad_available = false;
    a->status.wake_available = false;
    UNLOCK(a);
    xEventGroupSetBits(a->exits, CAPTURE_EXIT);
    vTaskDelete(NULL);
}

/* The capture/AFE worker owns all reset/feed/fetch calls. A permission fence
 * runs even when capture is withholding feeds until that fence is acknowledged. */
static void apply_afe_fence(bot_audio_t *a)
{
    LOCK(a);
    uint32_t requested = a->afe_reset_requested;
    bool needed = a->status.afe_running && requested && requested != a->afe_epoch &&
                  requested == a->gate_epoch && a->status.session_open;
    bot_audio_token_t token = a->session;
    UNLOCK(a);
    if (!needed || xSemaphoreTake(a->feed_lock, 0) != pdTRUE) return;
    /* The official reset APIs return 1 on success, -1 on failure. Never
     * acknowledge a new permission epoch if old AFE state could remain. */
    int reset = a->afe.iface->reset_buffer ? a->afe.iface->reset_buffer(a->afe.data) : -1;
    int vad_reset = a->afe.iface->reset_vad ? a->afe.iface->reset_vad(a->afe.data) : -1;
    bot_audio_keyword_reset(&a->afe.keyword);
    a->afe_pending_samples = 0;
    a->afe_fetch_wait_since_us = 0;
    a->raw_speech_until_us = 0;
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

static bool fetch_frame(bot_audio_t *a)
{
    LOCK(a);
    bool active = a->status.afe_running;
    UNLOCK(a);
    if (!active) return false;
    LOCK(a);
    uint32_t epoch = a->afe_epoch;
    bot_audio_token_t token = a->afe_token;
    UNLOCK(a);
    /* Missing output is normal while the official VAD history fills or frame
     * sizes differ. Poll without blocking the sole owner of the next feed. */
    afe_fetch_result_t *result = a->afe.iface->fetch_with_delay(a->afe.data, 0);
    if (!result || result->ret_value != ESP_OK) {
        int64_t now = esp_timer_get_time();
        if (!a->afe_fetch_wait_since_us) a->afe_fetch_wait_since_us = now;
        if (now - a->afe_fetch_wait_since_us >= INT64_C(4000000)) {
            LOCK(a);
            a->status.afe_errors++;
            UNLOCK(a);
            afe_failed(a, ESP_FAIL, "Official AFE fetch repeatedly failed or timed out");
        }
        return false;
    }
    a->afe_fetch_wait_since_us = 0;
    int64_t now = esp_timer_get_time();
    LOCK(a);
    a->status.afe_frames++;
    a->status.last_afe_result_us = now;
    uint32_t detection_gate = a->gate_epoch;
    bot_audio_wake_backend_t backend = a->status.wake_backend;
    bool healthy = a->run && a->capture_good && !echo_blocked_locked(a, now);
    bool command_mode = a->status.session_open && a->session_options.local_commands &&
        epoch != 0 && epoch == detection_gate && !a->status.rx_discard_frames &&
        !a->status.command_consumed && !a->buffer_overflow_latched;
    bool wake_cooldown_clear =
        (!a->last_wake_us || now - a->last_wake_us >= (int64_t)a->config.wake_debounce_ms * 1000);
    uint8_t threshold = a->status.controls_applied.wake_threshold_percent;
    UNLOCK(a);
    /* A graph change is serialized here, after the new token's RX/AFE fence.
     * Invalid model updates are not retried on every PCM frame. A later policy
     * revision/session or explicit bounded recovery can retry. */
    if (a->afe.keyword.data && (a->keyword_policy_epoch != detection_gate ||
        a->keyword_policy_commands != command_mode)) {
        esp_err_t policy_error = bot_audio_keyword_set_policy(&a->afe.keyword, command_mode, threshold);
        a->keyword_policy_epoch = detection_gate;
        a->keyword_policy_commands = command_mode;
        LOCK(a);
        a->status.local_commands_available = policy_error == ESP_OK;
        if (backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7) {
            a->status.wake_status = policy_error == ESP_OK ? BOT_AUDIO_WAKE_READY : BOT_AUDIO_WAKE_UNAVAILABLE;
            if (policy_error != ESP_OK) {
                a->status.wake_available = false;
                set_reason_locked(a, policy_error, "MultiNet command policy unavailable; manual session remains available");
            }
        }
        UNLOCK(a);
        if (policy_error != ESP_OK) publish_wake_status(a);
    }
    bool wake = backend == BOT_AUDIO_WAKE_BACKEND_WAKENET &&
                result->wakeup_state == WAKENET_DETECTED &&
                result->wakenet_model_index == 1 && result->wake_word_index == 1;
    int detected = 0;
    if (a->afe.keyword.data) {
        if (a->keyword_epoch != detection_gate) {
            bot_audio_keyword_reset(&a->afe.keyword);
            a->keyword_epoch = detection_gate;
        }
        size_t count = result->data_size > 0 && result->data_size % sizeof(int16_t) == 0 ?
                       (size_t)result->data_size / sizeof(int16_t) : 0;
        /* Inference stays on the single audio owner, outside the spinlock.
         * The permission/playback gate is checked again after native inference. */
        bool infer = healthy && (command_mode ||
                     (backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7 && wake_cooldown_clear));
        detected = bot_audio_keyword_detect(&a->afe.keyword, result->data, count,
                                              result->data_volume, result->vad_state == VAD_SPEECH, infer);
        if (backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7) wake = detected == 1;
    }
    now = esp_timer_get_time();
    LOCK(a);
    bool echo_block = echo_blocked_locked(a, now);
    a->passive_speech = a->run && a->capture_good && !echo_block && result->vad_state == VAD_SPEECH;
    bool vad_allowed = a->run && a->capture_good && a->status.session_open && epoch != 0 &&
        epoch == a->gate_epoch && a->status.rx_discard_frames == 0 && !echo_block &&
        !a->status.command_consumed && !a->buffer_overflow_latched &&
        (a->session_options.retain_start || a->session_options.deliver_pcm || a->session_options.local_commands);
    const bool waiting_for_followup = a->waiting_for_followup;
    bool command_allowed = detected >= BOT_VOICE_COMMAND_VOLUME_UP &&
        detected <= BOT_VOICE_COMMAND_UNMUTE && command_mode && vad_allowed &&
        a->session_options.local_commands && detection_gate == a->gate_epoch && same_token(&a->session, &token);
    bool wake_allowed = wake && a->run && a->capture_good &&
        a->status.wake_status == BOT_AUDIO_WAKE_READY && detection_gate == a->gate_epoch && !echo_block &&
        (!a->last_wake_us || now - a->last_wake_us >= (int64_t)a->config.wake_debounce_ms * 1000);
    if (wake_allowed) {
        a->last_wake_us = now;
        a->status.wake_detections++;
        a->status.last_wake_us = now;
        a->status.last_wake_score_available = backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7;
        a->status.last_wake_score = a->status.last_wake_score_available ? a->afe.keyword.last_score : 0.0f;
    } else if (wake) a->status.suppressed_wakes++;
    a->status.keyword_runs = a->afe.keyword.runs;
    a->status.keyword_dbfs = result->data_volume;
    a->status.keyword_input_peak = a->afe.keyword.input_peak;
    a->status.keyword_timeouts = a->afe.keyword.timeouts;
    a->status.keyword_rejections = a->afe.keyword.rejections;
    a->status.keyword_us = a->afe.keyword.last_us;
    a->status.keyword_max_us = a->afe.keyword.max_us;
    a->status.last_candidate_score = a->afe.keyword.last_candidate_score;
    a->status.last_candidate_command = a->afe.keyword.last_candidate_command;
    if (command_allowed) {
        /* Consume locally before this capture block can reach on_pcm. Earlier
         * transmitted prefixes cannot be recalled; runtime cancels that turn. */
        a->status.command_consumed = true;
        a->status.pcm_delivery_enabled = false;
        a->status.retain_start_enabled = false;
        a->status.local_commands++;
        a->status.last_command_us = now;
        a->buffer_generation++;
    }
    UNLOCK(a);
    if (command_allowed) {
        clear_start_buffer(a);
        set_capture_ready(a, false, token);
        end_speech(a);
        emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_LOCAL_COMMAND,
            .observed_us = now, .token = token, .command = (bot_voice_command_t)detected,
            .confidence_available = true, .confidence = a->afe.keyword.last_score,
            .wake_backend = BOT_AUDIO_WAKE_BACKEND_MULTINET7});
        return true;
    }
    if (wake_allowed) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_WAKE_DETECTED,
        .observed_us = now, .wake_phrase = BOT_AUDIO_WAKE_PHRASE,
        .wake_word_index = 1, .wake_backend = backend, .echo_safe = true,
        .confidence_available = backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7,
        .confidence = backend == BOT_AUDIO_WAKE_BACKEND_MULTINET7 ? a->afe.keyword.last_score : 0.0f});
    if (epoch != a->afe_speech_epoch || !vad_allowed) {
        end_speech(a);
        a->afe_speech_epoch = epoch;
    }
    if (!vad_allowed) return true;
    bool next_speech = bot_audio_speech_frame(result->vad_state == VAD_SPEECH,
        now <= a->raw_speech_until_us, waiting_for_followup, a->afe_speech);
    if (next_speech != a->afe_speech) {
        a->afe_speech = next_speech;
        if (next_speech) a->afe_speech_token = token;
        if (next_speech && !a->buffer_protect_from_us) {
            /* VAD onset and delay are each 128 ms. This is an observation-time
             * bound, not a claimed hardware capture timestamp. */
            a->buffer_protect_from_us = now > START_ONSET_MARGIN_US ? now - START_ONSET_MARGIN_US : 1;
        }
        emit(a, (bot_audio_event_t){.kind = next_speech ? BOT_AUDIO_EVENT_VAD_STARTED : BOT_AUDIO_EVENT_VAD_ENDED,
            .observed_us = now, .token = token, .echo_safe = false});
    }
    /* result->data/vad_cache/raw_data are never exported. In particular,
     * vad_cache can precede the user's authorization and is discarded. */
    return true;
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
    /* Purge old DMA samples while muted. No later stream can begin until this
     * completes, so unmuting never intentionally replays the canceled tail. */
    esp_err_t flush = output_silence(a, a->dma_frames + BOT_AUDIO_PLAYBACK_SAMPLES);
    esp_err_t drain_error = amp_error != ESP_OK ? amp_error : flush;
    if (error == ESP_OK && drain_error != ESP_OK) error = drain_error;
    if (error != ESP_OK) kind = BOT_AUDIO_EVENT_PLAYBACK_FAILED;
    xSemaphoreTake(a->command_lock, portMAX_DELAY);
    xQueueReset(a->output_queue);
    LOCK(a);
    bot_audio_playback_source_t source = a->status.playback_source;
    if (a->status.playback_id == id) {
        if (a->playback_cancelled && error == ESP_OK) kind = BOT_AUDIO_EVENT_PLAYBACK_CANCELLED;
        /* A failed purge must never be followed by unmuting another stream. */
        if (drain_error != ESP_OK) a->output_fault = true;
        a->playback_requested = false;
        a->playback_cancelled = false;
        a->playback_end_enqueued = false;
        a->status.playback_running = false;
        a->status.last_playback_done_us = esp_timer_get_time();
        a->echo_tail_until_us = esp_timer_get_time() + (int64_t)a->config.playback_tail_ms * 1000;
        if (error != ESP_OK) {
            a->status.playback_errors++;
            set_reason_locked(a, error, "Local playback failed; amplifier gated off");
        }
    }
    UNLOCK(a);
    xSemaphoreGive(a->command_lock);
    emit(a, (bot_audio_event_t){.kind = kind, .token = token, .playback_id = id,
        .playback_source = source,
        .error = error, .reason = error == ESP_OK ? NULL : "Local playback or DMA drain failed"});
}

static void playback_worker(void *context)
{
    bot_audio_t *a = context;
    playback_frame_t frame;
    int16_t stereo[BOT_AUDIO_PLAYBACK_SAMPLES * 2];
    int64_t last_progress_us = 0;
    int64_t last_stack_sample_us = 0;
    while (true) {
        if (esp_timer_get_time() - last_stack_sample_us >= 5000000) {
            last_stack_sample_us = esp_timer_get_time();
            const uint32_t remaining = uxTaskGetStackHighWaterMark(NULL);
            LOCK(a); a->status.playback_stack_free = remaining; UNLOCK(a);
        }
        LOCK(a);
        bool run = a->run;
        bool requested = a->playback_requested;
        bool cancelled = a->playback_cancelled || !run;
        bool started = a->status.playback_running;
        uint32_t id = a->status.playback_id;
        bot_audio_playback_source_t source = a->status.playback_source;
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
        /* The pending mute request gates the very next output block even if
         * the capture owner has not yet acknowledged codec volume. PA gating
         * never calls ES8389's ADC-affecting set_out_mute implementation. */
        LOCK(a);
        current = a->run && a->playback_requested && !a->playback_cancelled && a->status.playback_id == id;
        bool muted = output_muted_locked(a);
        esp_err_t amp_error = current ? gpio_set_level(BSP_POWER_AMP_IO, muted ? 0 : 1) : ESP_OK;
        if (current && amp_error == ESP_OK) a->status.playback_running = true;
        UNLOCK(a);
        if (!current || amp_error != ESP_OK) {
            if (amp_error != ESP_OK) finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, amp_error);
            clear_audio(&frame, sizeof(frame));
            continue;
        }
        for (size_t i = 0; i < frame.count; ++i)
            stereo[2 * i] = stereo[2 * i + 1] = muted ? 0 : frame.samples[i];
        esp_err_t err = esp_codec_dev_write(a->codec, stereo, frame.count * 2 * sizeof(int16_t));
        clear_audio(&frame, sizeof(frame));
        clear_audio(stereo, sizeof(stereo));
        int64_t written_us = esp_timer_get_time();
        LOCK(a);
        current = a->run && a->playback_requested && !a->playback_cancelled && a->status.playback_id == id;
        if (err == ESP_OK) {
            a->status.last_playback_us = written_us;
            if (!started) a->status.last_playback_started_us = written_us;
        }
        UNLOCK(a);
        if (err != ESP_OK) {
            finish_playback(a, id, token, BOT_AUDIO_EVENT_PLAYBACK_FAILED, err);
        } else if (current) {
            if (!started) {
                last_progress_us = written_us;
                emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_PLAYBACK_STARTED,
                    .observed_us = written_us, .token = token, .playback_id = id, .playback_source = source});
            } else if (written_us - last_progress_us >= PLAYBACK_PROGRESS_US) {
                last_progress_us = written_us;
                emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_PLAYBACK_PROGRESS,
                    .observed_us = written_us, .token = token, .playback_id = id, .playback_source = source});
            }
        }
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
    a->status.controls_requested = bot_voice_default_settings();
    a->status.controls_requested.volume_percent = cfg.output_volume;
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    a->status.controls_requested.wake_threshold_percent = CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT;
#endif
    a->status.controls_applied = a->status.controls_requested;
    a->status.controls_revision = 1;
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
    bot_audio_start_buffer_close(&a->start_buffer);
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
    /* Start/restart also purges TX DMA while the amplifier is muted. */
    if (err == ESP_OK) err = output_silence(a, a->dma_frames + BOT_AUDIO_PLAYBACK_SAMPLES);
    if (err == ESP_OK) {
        a->stereo_capture = calloc(a->capture_samples * 2, sizeof(int16_t));
        a->mono_capture = calloc(a->capture_samples, sizeof(int16_t));
        if (!a->stereo_capture || !a->mono_capture) err = ESP_ERR_NO_MEM;
    }
    if (err == ESP_OK) {
        const esp_err_t buffered = bot_audio_start_buffer_open(&a->start_buffer, a->capture_samples,
                                                               CONFIG_BOT_AUDIO_START_BUFFER_MS);
        if (buffered != ESP_OK)
            ESP_LOGW(TAG, "Onset buffer unavailable (%s); local wake remains enabled, wait for transport readiness before speaking",
                     esp_err_to_name(buffered));
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
    esp_apa_doa_cfg_t doa_config = ESP_APA_DOA_CFG_DEFAULT();
    ESP_APA_DOA_PRESET_BALANCED(&doa_config);
    doa_config.fmax_hz = 4000.0f;
    doa_config.mic_num = 2;
    const float microphones[2][2] = ESP_APA_DOA_MIC_POS_LINEAR(0.043f);
    memcpy(doa_config.mic_pos, microphones, sizeof(microphones));
    esp_err_t doa_error = esp_apa_doa_create(&doa_config, &a->doa);
    if (doa_error != ESP_OK) {
        a->doa = NULL;
        ESP_LOGW(TAG, "Official DOA unavailable (%s); sound reaction has no direction", esp_err_to_name(doa_error));
    } else {
        ESP_LOGI(TAG, "Official 2-microphone DOA active; orientation requires physical calibration");
    }
    xQueueReset(a->output_queue);
    xEventGroupSetBits(a->exits, ALL_EXIT);
    LOCK(a);
    /* set_controls is valid before start; preserve its complete request across
     * the status reset. Hardware/model acknowledgements start fresh. */
    bot_voice_settings_t initial_controls = a->status.controls_requested;
    uint32_t initial_revision = a->status.controls_revision;
    memset(&a->status, 0, sizeof(a->status));
    a->status.controls_requested = initial_controls;
    a->status.controls_applied = initial_controls;
    a->status.controls_revision = initial_revision;
    a->status.started = true;
    a->status.start_buffer_available = a->start_buffer.capacity != 0;
    a->status.codec_available = true;
    a->status.afe_running = a->afe.data != NULL;
    a->status.vad_available = false; /* a healthy microphone read is still required */
    a->status.direction_available = a->doa != NULL;
    a->status.night_wake_supported = true;
    a->status.wake_status = a->afe.wake_status;
    a->status.wake_backend = a->afe.wake_backend;
    a->status.local_commands_available = a->afe.keyword.data && a->afe.keyword.available;
    a->status.last_error = afe_error;
    snprintf(a->status.model_name, sizeof(a->status.model_name), "%s", a->afe.model_name);
    snprintf(a->status.reason, sizeof(a->status.reason), "%s", a->afe.reason);
    a->run = true;
    a->capture_good = false;
    a->output_fault = false;
    a->capture_sequence = 0;
    a->afe_epoch = a->afe_reset_requested = 0;
    a->keyword_epoch = 0;
    a->keyword_policy_epoch = 0;
    a->keyword_policy_commands = false;
    a->controls_attempt_revision = 0;
    a->retry_afe_requested = false;
    a->afe_recovery_due_us = 0;
    if (++a->gate_epoch == 0) ++a->gate_epoch;
    a->session = (bot_audio_token_t){0};
    a->session_options = (bot_audio_session_options_t){0};
    a->waiting_for_followup = false;
    a->buffer_generation = 1;
    a->buffer_owner_generation = 0;
    a->buffer_protect_from_us = 0;
    a->buffer_overflow_latched = false;
    a->afe_token = (bot_audio_token_t){0};
    a->playback_requested = a->playback_cancelled = a->playback_end_enqueued = false;
    a->echo_reset_pending = false;
    a->echo_discard_frames = 0;
    a->last_wake_us = a->echo_tail_until_us = 0;
    a->last_sound_event_us = a->last_direction_us = a->last_loud_us = a->last_impulse_us = 0;
    a->ambient_ready_us = esp_timer_get_time() + INT64_C(1000000);
    a->ambient_noise = 0;
    a->passive_speech = a->last_sound_direction_valid = false;
    UNLOCK(a);
    /* Emit initial state before capture can publish its first successful read. */
    emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_HEALTH, .available = false,
        .reason = "Codec opened; awaiting first microphone read"});
    publish_wake_status(a);
    a->afe_speech = false;
    a->afe_speech_epoch = 0;
    a->afe_pending_samples = 0;
    a->afe_fetch_wait_since_us = 0;
    a->raw_speech_until_us = 0;
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
        if (xTaskCreate(capture_worker, "bot_audio_in", 12288, a,
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

esp_err_t bot_audio_set_controls(bot_audio_t *a, const bot_voice_settings_t *settings)
{
    if (!a || !settings || !bot_voice_settings_valid(settings)) return ESP_ERR_INVALID_ARG;
    LOCK(a);
    if (!bot_voice_settings_equal(settings, &a->status.controls_requested)) {
        a->status.controls_requested = *settings;
        if (++a->status.controls_revision == 0) ++a->status.controls_revision;
    }
    UNLOCK(a);
    return ESP_OK;
}

esp_err_t bot_audio_retry_afe(bot_audio_t *a)
{
    if (!a) return ESP_ERR_INVALID_ARG;
    LOCK(a);
    bool allowed = a->run && a->afe.data && !a->retry_afe_requested;
    if (allowed) a->retry_afe_requested = true;
    UNLOCK(a);
    return allowed ? ESP_OK : ESP_ERR_INVALID_STATE;
}

esp_err_t bot_audio_set_session_ex(bot_audio_t *a, const bot_audio_token_t *token,
                                   const bot_audio_session_options_t *options)
{
    if (!a || ((token == NULL) != (options == NULL)) ||
        (token && (!token->epoch || !token->session_id))) return ESP_ERR_INVALID_ARG;
    const bot_audio_session_options_t next = options ? *options : (bot_audio_session_options_t){0};
    LOCK(a);
    if (!a->run) {
        UNLOCK(a);
        return ESP_ERR_INVALID_STATE;
    }
    bool same_session = token && a->status.session_open && same_token(&a->session, token);
    bool unchanged = a->status.session_open == (token != NULL) && (!token || same_session) &&
        a->session_options.deliver_pcm == next.deliver_pcm &&
        a->session_options.retain_start == next.retain_start &&
        a->session_options.waiting_for_followup == next.waiting_for_followup &&
        a->session_options.local_commands == next.local_commands;
    if (unchanged) {
        UNLOCK(a);
        return ESP_OK;
    }
    bot_audio_token_t old_token = a->session;
    bool was_ready = a->status.capture_ready;
    bool capture_requested = next.retain_start || next.deliver_pcm || next.local_commands;
    if (!same_session) {
        if (++a->gate_epoch == 0) ++a->gate_epoch;
        a->status.rx_discard_frames = token ? a->dma_frames + a->capture_samples : 0;
        a->afe_reset_requested = 0;
        a->status.command_consumed = false;
        a->buffer_overflow_latched = false;
    }
    if (!same_session || !next.retain_start ||
        (a->session_options.deliver_pcm && !next.deliver_pcm)) a->buffer_generation++;
    a->status.session_open = token != NULL;
    a->status.pcm_delivery_enabled = token && next.deliver_pcm &&
                                     !a->status.command_consumed && !a->buffer_overflow_latched;
    a->status.retain_start_enabled = token && next.retain_start && a->start_buffer.capacity &&
                                     !a->status.command_consumed && !a->buffer_overflow_latched;
    if (!same_session || !capture_requested) a->status.capture_ready = false;
    a->waiting_for_followup = next.waiting_for_followup;
    a->session_options = next;
    a->session = token ? *token : (bot_audio_token_t){0};
    bool lost_ready = was_ready && !a->status.capture_ready;
    if (!same_session && a->playback_requested &&
        a->status.playback_source == BOT_AUDIO_PLAYBACK_SESSION) {
        a->playback_cancelled = true;
        /* This GPIO write is bounded and independent of codec I2C/queue locks. */
        (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    }
    UNLOCK(a);
    if (lost_ready) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_CAPTURE_READY,
        .token = old_token, .available = false});
    return ESP_OK;
}

esp_err_t bot_audio_set_session(bot_audio_t *a, const bot_audio_token_t *token, bool deliver_pcm)
{
    if (!token && deliver_pcm) return ESP_ERR_INVALID_ARG;
    bot_audio_session_options_t options = {.deliver_pcm = deliver_pcm};
    return bot_audio_set_session_ex(a, token, token ? &options : NULL);
}

esp_err_t bot_audio_wait_for_followup(bot_audio_t *a, const bot_audio_token_t *token)
{
    bot_audio_session_options_t options = {.waiting_for_followup = true, .local_commands = true};
    return bot_audio_set_session_ex(a, token, token ? &options : NULL);
}

static esp_err_t playback_begin(bot_audio_t *a, const bot_audio_token_t *token,
                                 bot_audio_playback_source_t source, uint32_t *out_id)
{
    if (!a || !token || !token->epoch || !out_id ||
        (source != BOT_AUDIO_PLAYBACK_PREVIEW && !token->session_id) ||
        (source == BOT_AUDIO_PLAYBACK_PREVIEW && (token->session_id || token->turn_id || token->focus_generation)))
        return ESP_ERR_INVALID_ARG;
    *out_id = 0;
    if (xSemaphoreTake(a->command_lock, 0) != pdTRUE) return ESP_ERR_TIMEOUT;
    LOCK(a);
    bool authorized = source == BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE || source == BOT_AUDIO_PLAYBACK_PREVIEW ||
                      (a->status.session_open && same_token(token, &a->session));
    bool allowed = a->run && a->codec_open && !a->output_fault && authorized && !a->playback_requested;
    bool lost_ready = false;
    bot_audio_token_t capture_token = a->session;
    if (allowed) {
        if (++a->next_playback_id == 0) ++a->next_playback_id;
        a->status.playback_id = a->next_playback_id;
        a->status.playback_source = source;
        a->playback_token = *token;
        a->playback_requested = true;
        a->playback_cancelled = false;
        a->playback_end_enqueued = false;
        a->echo_reset_pending = true;
        a->echo_discard_frames = a->dma_frames + a->capture_samples;
        a->playback_begin_us = a->last_queued_us = esp_timer_get_time();
        a->status.last_playback_begin_us = a->playback_begin_us;
        a->buffer_generation++;
        lost_ready = a->status.capture_ready;
        a->status.capture_ready = false;
        *out_id = a->status.playback_id;
    }
    UNLOCK(a);
    xSemaphoreGive(a->command_lock);
    if (lost_ready) emit(a, (bot_audio_event_t){.kind = BOT_AUDIO_EVENT_CAPTURE_READY,
        .token = capture_token, .available = false});
    return allowed ? ESP_OK : ESP_ERR_INVALID_STATE;
}

esp_err_t bot_audio_playback_begin(bot_audio_t *a, const bot_audio_token_t *token, uint32_t *out_id)
{
    return playback_begin(a, token, BOT_AUDIO_PLAYBACK_SESSION, out_id);
}

esp_err_t bot_audio_playback_begin_local(bot_audio_t *a, const bot_audio_token_t *token, uint32_t *out_id)
{
    return playback_begin(a, token, BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE, out_id);
}

esp_err_t bot_audio_playback_begin_preview(bot_audio_t *a, uint32_t epoch, uint32_t *out_id)
{
    const bot_audio_token_t token = {.epoch = epoch};
    return playback_begin(a, &token, BOT_AUDIO_PLAYBACK_PREVIEW, out_id);
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
            UBaseType_t queued = uxQueueMessagesWaiting(a->output_queue);
            LOCK(a);
            if (queued > a->status.output_queue_high_water) a->status.output_queue_high_water = queued;
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
    a->status.capture_ready = false;
    a->status.retain_start_enabled = false;
    a->buffer_generation++;
    a->status.rx_discard_frames = 0;
    a->afe_reset_requested = 0;
    if (++a->gate_epoch == 0) ++a->gate_epoch;
    a->session = (bot_audio_token_t){0};
    a->session_options = (bot_audio_session_options_t){0};
    a->playback_cancelled = a->playback_requested;
    (void)gpio_set_level(BSP_POWER_AMP_IO, 0);
    UNLOCK(a);
    EventBits_t bits = xEventGroupWaitBits(a->exits, ALL_EXIT, pdFALSE, pdTRUE, pdMS_TO_TICKS(timeout_ms));
    if ((bits & ALL_EXIT) != ALL_EXIT) return ESP_ERR_TIMEOUT;
    /* No worker can touch the device, model, or sample buffers after its exit bit. */
    if (a->doa) (void)esp_apa_doa_destroy(a->doa);
    a->doa = NULL;
    codec_cleanup(a);
    bot_audio_afe_close(&a->afe);
    release_capture_buffers(a);
    xQueueReset(a->output_queue);
    a->capture_task = a->output_task = NULL;
    LOCK(a);
    a->status.started = false;
    a->status.start_buffer_available = false;
    a->status.capture_running = a->status.afe_running = a->status.playback_running = false;
    a->status.codec_available = a->status.vad_available = a->status.wake_available = false;
    a->status.local_commands_available = false;
    a->status.direction_available = a->status.direction_valid = false;
    a->status.ambient_activity = a->status.ambient_speech = false;
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
    out->capture_healthy = a->capture_good;
    out->self_playback_blocked = echo_blocked_locked(a, esp_timer_get_time());
    if (out->self_playback_blocked) out->capture_ready = false;
    UNLOCK(a);
}

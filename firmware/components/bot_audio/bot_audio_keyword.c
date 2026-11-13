/* SPDX-License-Identifier: MIT
 * Official ESP-SR 2.6.1 MultiNet7 integration. Continuous custom-keyword use
 * follows XiaoZhi's CustomWakeWord design; no upstream recording/upload path.
 * Passive graph: ONE whole phrase, never two shorter-name detections. The
 * separate active-session graph adds only fixed local control commands.
 */
#include "bot_audio_keyword.h"
#include <math.h>
#include <stdlib.h>
#include <string.h>
#include "sdkconfig.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_mn_models.h"
#include "esp_mn_speech_commands.h"
#include "esp_timer.h"

#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK && !CONFIG_SR_MN_CN_MULTINET7_QUANT
#error "Enable CONFIG_SR_MN_CN_MULTINET7_QUANT to package the actual mn7_cn wake fallback"
#endif
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK && CONFIG_SR_MN_EN_MULTINET7_QUANT
/* ESP-SR 2.6.1 commands_add uses compile-time English G2P when this is set. */
#error "The Chinese full-phrase wake fallback requires CONFIG_SR_MN_EN_NONE=y"
#endif

#define WAKE_COMMAND_ID 1
#define SILENCE_RESET_SAMPLES (BOT_AUDIO_SAMPLE_RATE * 600U / 1000U)
#define SILENCE_DBFS (-48.0f)

#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
typedef struct { bot_voice_command_t id; const char *phrase; } local_phrase_t;
static const local_phrase_t s_commands[] = {
    {BOT_VOICE_COMMAND_VOLUME_UP, "sheng yin da yi dian"},
    {BOT_VOICE_COMMAND_VOLUME_DOWN, "sheng yin xiao yi dian"},
    {BOT_VOICE_COMMAND_QUIET, "an jing yi dian"},
    {BOT_VOICE_COMMAND_STOP, "ting zhi"},
    {BOT_VOICE_COMMAND_SLEEP, "wan an"},
    {BOT_VOICE_COMMAND_THEME_BLUE, "huan cheng lan se"},
    {BOT_VOICE_COMMAND_HAPPY, "kai xin yi dian"},
    {BOT_VOICE_COMMAND_REPEAT, "zai shuo yi bian"},
    {BOT_VOICE_COMMAND_UNMUTE, "hui fu sheng yin"},
};
#endif

static void clear_frame(bot_audio_keyword_t *keyword)
{
    if (keyword->frame) {
        volatile int16_t *p = keyword->frame;
        for (size_t i = 0; i < keyword->samples; ++i) p[i] = 0;
    }
    keyword->buffered = 0;
}

esp_err_t bot_audio_keyword_open(bot_audio_keyword_t *keyword, char *model_name,
                                  size_t afe_samples)
{
    memset(keyword, 0, sizeof(*keyword));
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    if (!model_name || strcmp(model_name, BOT_AUDIO_MULTINET_MODEL) != 0) return ESP_ERR_NOT_FOUND;
    keyword->iface = esp_mn_handle_from_name(model_name);
    const esp_mn_iface_t *mn = keyword->iface;
    if (!mn || !mn->create || !mn->destroy || !mn->detect || !mn->clean ||
        !mn->get_samp_rate || !mn->get_samp_chunksize || !mn->get_results ||
        !mn->set_det_threshold || !mn->set_speech_commands || !mn->check_speech_command)
        return ESP_ERR_NOT_SUPPORTED;
    size_t internal_before = heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT);
    size_t psram_before = heap_caps_get_free_size(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    keyword->data = mn->create(model_name, CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS);
    if (!keyword->data) return ESP_ERR_NO_MEM;
    /* The official Chinese command API is process-global. bot_audio is the sole
     * owner; replace all built-in commands before any inference is allowed. */
    keyword->commands_owned = true;
    int count = mn->get_samp_chunksize(keyword->data);
    if (count <= 0 || count > 4096 || !afe_samples || afe_samples > 4096 ||
        mn->get_samp_rate(keyword->data) != BOT_AUDIO_SAMPLE_RATE) {
        bot_audio_keyword_close(keyword);
        return ESP_ERR_INVALID_SIZE;
    }
    keyword->samples = (size_t)count;
    /* Both engines remain 16 kHz mono. A single bounded, aligned PSRAM chunk
     * also covers a native frame-size change without truncation or resampling. */
    keyword->frame = heap_caps_aligned_alloc(16, keyword->samples * sizeof(int16_t),
                                            MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT);
    if (!keyword->frame) {
        bot_audio_keyword_close(keyword);
        return ESP_ERR_NO_MEM;
    }
    clear_frame(keyword);
    if (esp_mn_commands_alloc(mn, keyword->data) != ESP_OK ||
        bot_audio_keyword_set_policy(keyword, false, CONFIG_BOT_AUDIO_MULTINET_THRESHOLD_PERCENT) != ESP_OK) {
        bot_audio_keyword_close(keyword);
        return ESP_FAIL;
    }
    mn->clean(keyword->data);
    ESP_LOGI("bot_audio", "MultiNet7: one complete '%s' command; model=%s, threshold=%.2f, window=%d ms",
             BOT_AUDIO_MULTINET_COMMAND, model_name, keyword->threshold_percent / 100.0,
             CONFIG_BOT_AUDIO_MULTINET_WINDOW_MS);
    ESP_LOGI("bot_audio", "MultiNet input frame=%u samples; AFE=%u samples; one bounded PSRAM staging frame",
             (unsigned)keyword->samples, (unsigned)afe_samples);
    ESP_LOGI("bot_audio", "MultiNet initialization free heap: internal %u -> %u, PSRAM %u -> %u bytes",
             (unsigned)internal_before,
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
             (unsigned)psram_before,
             (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT));
    return ESP_OK;
#else
    (void)model_name;
    (void)afe_samples;
    return ESP_ERR_NOT_SUPPORTED;
#endif
}

esp_err_t bot_audio_keyword_set_policy(bot_audio_keyword_t *keyword, bool commands_enabled,
                                       uint8_t wake_threshold_percent)
{
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    if (!keyword->data || !keyword->iface) return ESP_ERR_NOT_SUPPORTED;
    if (wake_threshold_percent < BOT_VOICE_WAKE_THRESHOLD_MIN ||
        wake_threshold_percent > BOT_VOICE_WAKE_THRESHOLD_MAX) return ESP_ERR_INVALID_ARG;
    if (keyword->available && keyword->commands_enabled == commands_enabled &&
        keyword->threshold_percent == wake_threshold_percent) return ESP_OK;
    bot_audio_keyword_reset(keyword);
    const bool rebuild = !keyword->available || keyword->commands_enabled != commands_enabled;
    keyword->available = false;
    unsigned native_percent = wake_threshold_percent;
    if (commands_enabled && native_percent > CONFIG_BOT_AUDIO_LOCAL_COMMAND_THRESHOLD_PERCENT)
        native_percent = CONFIG_BOT_AUDIO_LOCAL_COMMAND_THRESHOLD_PERCENT;
    /* A separate top-result gate retains the wake/command-specific threshold. */
    if (keyword->iface->set_det_threshold(keyword->data, native_percent / 100.0f) < 0) return ESP_FAIL;
    if (rebuild) {
        if (esp_mn_commands_clear() != ESP_OK ||
            esp_mn_commands_add(WAKE_COMMAND_ID, BOT_AUDIO_MULTINET_COMMAND) != ESP_OK) return ESP_FAIL;
        if (commands_enabled) {
            for (size_t i = 0; i < sizeof(s_commands) / sizeof(s_commands[0]); ++i)
                if (esp_mn_commands_add(s_commands[i].id, s_commands[i].phrase) != ESP_OK) return ESP_FAIL;
        }
        if (esp_mn_commands_update() != NULL) return ESP_FAIL;
        const size_t expected = 1 + (commands_enabled ? sizeof(s_commands) / sizeof(s_commands[0]) : 0);
        for (size_t i = 0; i < expected; ++i) {
            esp_mn_phrase_t *phrase = esp_mn_commands_get_from_index((int)i);
            int id = i ? s_commands[i - 1].id : WAKE_COMMAND_ID;
            const char *text = i ? s_commands[i - 1].phrase : BOT_AUDIO_MULTINET_COMMAND;
            if (!phrase || phrase->command_id != id || !phrase->string || strcmp(phrase->string, text))
                return ESP_ERR_INVALID_STATE;
        }
        if (esp_mn_commands_get_from_index((int)expected) != NULL) return ESP_ERR_INVALID_STATE;
    }
    keyword->commands_enabled = commands_enabled;
    keyword->threshold_percent = wake_threshold_percent;
    keyword->available = true;
    return ESP_OK;
#else
    (void)keyword;
    (void)commands_enabled;
    (void)wake_threshold_percent;
    return ESP_ERR_NOT_SUPPORTED;
#endif
}

void bot_audio_keyword_reset(bot_audio_keyword_t *keyword)
{
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    if (keyword->data && keyword->iface && keyword->dirty) keyword->iface->clean(keyword->data);
#endif
    keyword->dirty = false;
    keyword->silence_samples = 0;
    clear_frame(keyword);
}

int bot_audio_keyword_detect(bot_audio_keyword_t *keyword, int16_t *samples, size_t count,
                              float volume_dbfs, bool allowed)
{
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    if (!keyword->data || !keyword->available) return 0;
    if (!allowed || !samples || !count || count > 4096 || !isfinite(volume_dbfs)) {
        bot_audio_keyword_reset(keyword);
        return false;
    }
    /* AFE VAD has a 600 ms release hold. Use its pre-AGC energy to expire a
     * partial command during real silence without adding another recognizer.
     * Silent frames still reach MultiNet until this bound so it can finish a
     * complete phrase. A later separate call cannot reuse this partial state. */
    if (volume_dbfs < SILENCE_DBFS) {
        if (keyword->silence_samples < SILENCE_RESET_SAMPLES)
            keyword->silence_samples += (uint32_t)count;
        if (keyword->silence_samples >= SILENCE_RESET_SAMPLES) {
            if (keyword->dirty) keyword->iface->clean(keyword->data);
            keyword->dirty = false;
            clear_frame(keyword);
            return false;
        }
    } else {
        keyword->silence_samples = 0;
    }
    size_t offset = 0;
    while (offset < count) {
        size_t copy = keyword->samples - keyword->buffered;
        if (copy > count - offset) copy = count - offset;
        memcpy(keyword->frame + keyword->buffered, samples + offset, copy * sizeof(int16_t));
        keyword->buffered += copy;
        offset += copy;
        if (keyword->buffered < keyword->samples) break;
        keyword->dirty = true;
        int64_t before = esp_timer_get_time();
        esp_mn_state_t state = keyword->iface->detect(keyword->data, keyword->frame);
        int64_t elapsed = esp_timer_get_time() - before;
        keyword->last_us = elapsed > UINT32_MAX ? UINT32_MAX : (uint32_t)elapsed;
        if (keyword->last_us > keyword->max_us) keyword->max_us = keyword->last_us;
        keyword->runs++;
        clear_frame(keyword);
        if (state == ESP_MN_STATE_DETECTED) {
            esp_mn_results_t *result = keyword->iface->get_results(keyword->data);
            /* Only the highest-scoring complete command qualifies. A lower-ranked
             * candidate, partial name, silence, or VAD alone never emits a wake. */
            bool valid = result && result->state == ESP_MN_STATE_DETECTED &&
                result->num > 0 && result->num <= ESP_MN_RESULT_MAX_NUM &&
                isfinite(result->prob[0]) && result->prob[0] >= 0.0f && result->prob[0] <= 1.0f;
            int accepted = 0;
            if (valid) {
                const int id = result->command_id[0], index = result->phrase_id[0];
                keyword->last_candidate_score = result->prob[0];
                keyword->last_candidate_command = id;
                if (id == WAKE_COMMAND_ID && index == 0 &&
                    result->prob[0] >= keyword->threshold_percent / 100.0f) accepted = id;
                if (keyword->commands_enabled && index > 0 &&
                    (size_t)index <= sizeof(s_commands) / sizeof(s_commands[0]) &&
                    id == (int)s_commands[index - 1].id &&
                    result->prob[0] >= CONFIG_BOT_AUDIO_LOCAL_COMMAND_THRESHOLD_PERCENT / 100.0f) accepted = id;
            }
            if (accepted) keyword->last_score = result->prob[0];
            else keyword->rejections++;
            bot_audio_keyword_reset(keyword);
            if (accepted) return accepted;
        } else if (state == ESP_MN_STATE_TIMEOUT) {
            keyword->timeouts++;
            /* Continuous local keyword mode: timeout starts a fresh window. */
            bot_audio_keyword_reset(keyword);
        } else if (state != ESP_MN_STATE_DETECTING) {
            bot_audio_keyword_reset(keyword);
        }
    }
    return false;
#else
    (void)keyword;
    (void)samples;
    (void)count;
    (void)volume_dbfs;
    (void)allowed;
    return false;
#endif
}

void bot_audio_keyword_close(bot_audio_keyword_t *keyword)
{
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    if (keyword->commands_owned) (void)esp_mn_commands_free();
    if (keyword->data && keyword->iface) keyword->iface->destroy(keyword->data);
#endif
    clear_frame(keyword);
    free(keyword->frame);
    memset(keyword, 0, sizeof(*keyword));
}

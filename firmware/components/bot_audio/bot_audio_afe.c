/* SPDX-License-Identifier: MIT
 * Thin integration of Espressif ESP-SR 2.6.1; see README for upstream references.
 * Signal processing and inference use the official AFE/WakeNet/MultiNet engines.
 */
#include "bot_audio_afe.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "esp_afe_config.h"
#include "esp_afe_sr_models.h"
#include "esp_partition.h"
#include "esp_log.h"
#include "sdkconfig.h"

/* ESP-SR's public model_path.c loader trusts its binary directory. Check the
 * directory before passing it an erased/partially provisioned flash partition.
 * This is a bounds check, not signature verification or a custom model format. */
static bool read_u32(const esp_partition_t *part, size_t offset, uint32_t *value)
{
    uint8_t bytes[4];
    if (offset > part->size || part->size - offset < sizeof(bytes) ||
        esp_partition_read(part, offset, bytes, sizeof(bytes)) != ESP_OK) return false;
    *value = (uint32_t)bytes[0] | ((uint32_t)bytes[1] << 8) |
             ((uint32_t)bytes[2] << 16) | ((uint32_t)bytes[3] << 24);
    return true;
}

static bool read_name(const esp_partition_t *part, size_t offset, char *name)
{
    if (offset > part->size || part->size - offset < SRMODEL_STRING_LENGTH ||
        esp_partition_read(part, offset, name, SRMODEL_STRING_LENGTH) != ESP_OK) return false;
    /* file names are used by strcmp in the upstream mmap loader */
    return name[0] != '\0' && memchr(name, '\0', SRMODEL_STRING_LENGTH) != NULL;
}

static bool model_directory_valid(const esp_partition_t *part)
{
    uint32_t models;
    size_t cursor = 4;
    size_t first_payload = part->size;
    if (!read_u32(part, 0, &models) || models == 0 || models > 32) return false;
    for (uint32_t i = 0; i < models; ++i) {
        char name[SRMODEL_STRING_LENGTH];
        uint32_t files;
        if (!read_name(part, cursor, name)) return false;
        /* The official MultiNet packer adds fst/commands_{cn,en}.txt. This
         * resource directory intentionally has no _MODEL_INFO_ of its own. */
        bool fst = strcmp(name, "fst") == 0;
        cursor += SRMODEL_STRING_LENGTH;
        if (!read_u32(part, cursor, &files) || files == 0 || files > 64) return false;
        cursor += 4;
        bool has_info = false, has_chinese_commands = false;
        for (uint32_t j = 0; j < files; ++j) {
            uint32_t offset, size;
            if (!read_name(part, cursor, name)) return false;
            cursor += SRMODEL_STRING_LENGTH;
            if (!read_u32(part, cursor, &offset) || !read_u32(part, cursor + 4, &size)) return false;
            cursor += 8;
            if (size == 0 || offset > part->size || size > part->size - offset) return false;
            if (offset < first_payload) first_payload = offset;
            if (strcmp(name, "_MODEL_INFO_") == 0) {
                if (size > 4096) return false;
                has_info = true;
            }
            if (strcmp(name, "commands_cn.txt") == 0) has_chinese_commands = true;
        }
        if (!has_info && !(fst && has_chinese_commands)) return false;
    }
    return cursor <= first_payload;
}

static void reason(bot_audio_afe_t *afe, const char *text)
{
    snprintf(afe->reason, sizeof(afe->reason), "%s", text);
}

esp_err_t bot_audio_afe_open(bot_audio_afe_t *afe, const char *label, const char *name, bool reference_input)
{
    memset(afe, 0, sizeof(*afe));
    afe->wake_status = BOT_AUDIO_WAKE_MODEL_MISSING;
    reason(afe, "WAKE_MODEL_MISSING: no usable full-phrase detector found");
    char *selected = NULL;
    char *multinet_name = NULL;
#if CONFIG_BOT_AUDIO_MULTINET_WAKE_FALLBACK
    const bool fallback_enabled = true;
#else
    const bool fallback_enabled = false;
#endif
    if (name[0] != '\0' || fallback_enabled) {
        const esp_partition_t *partition = esp_partition_find_first(
            ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_ANY, label);
        if (!partition) {
            reason(afe, "WAKE_MODEL_MISSING: model partition does not exist");
        } else if (!model_directory_valid(partition)) {
            reason(afe, "WAKE_MODEL_MISSING: blank or malformed model directory; flash generated srmodels.bin");
        } else {
            afe->models = esp_srmodel_init(label);
            if (!afe->models) {
                reason(afe, "WAKE_MODEL_MISSING: official model loader could not map partition");
            } else {
                /* Exact lookup; never select the first stock WakeNet or a
                 * different language/domain MultiNet model. */
                for (int i = 0; i < afe->models->num; ++i) {
                    if (name[0] && strcmp(afe->models->model_name[i], name) == 0) {
                        char *words = esp_srmodel_get_wake_words(afe->models, afe->models->model_name[i]);
                        /* Upstream joins multiple keywords with ';'. Whole-string
                         * equality requires exactly one complete acoustic keyword. */
                        if (words && strcmp(words, BOT_AUDIO_WAKE_PHRASE) == 0) {
                            selected = afe->models->model_name[i];
                        }
                        free(words); /* get_wake_words returns an allocation */
                    }
                    if (strcmp(afe->models->model_name[i], BOT_AUDIO_MULTINET_MODEL) == 0)
                        multinet_name = afe->models->model_name[i];
                }
                reason(afe, selected ? "Exact full-phrase model loaded; awaiting AFE initialization"
                                    : "WAKE_MODEL_MISSING: dedicated 小呆小呆 model unavailable; checking mn7_cn");
            }
        }
    }
    if (selected) snprintf(afe->model_name, sizeof(afe->model_name), "%s", selected);

    /* Official GMF aec_rec/sdkconfig.defaults.esp32s31 uses MRMN:
     * microphone, ES8389 DAC reference, microphone, unused. All four samples
     * share one hardware TDM frame. No software playback FIFO is used as ref. */
    afe_config_t *cfg = afe_config_init(reference_input ? "MRMN" : "M", afe->models,
                                       AFE_TYPE_SR, AFE_MODE_LOW_COST);
    if (!cfg) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "Official AFE configuration allocation failed");
        return ESP_ERR_NO_MEM;
    }
    cfg->aec_init = reference_input;
    cfg->se_init = false;
    cfg->ns_init = false;
    cfg->ns_model_name = NULL;
    cfg->vad_init = true;
    cfg->vad_model_name = NULL; /* official WebRTC VAD; no neural VAD model required */
    cfg->vad_mode = VAD_MODE_1;
    cfg->vad_min_speech_ms = 128;
    cfg->vad_min_noise_ms = 600;
    cfg->vad_delay_ms = 128;
    cfg->vad_mute_playback = false;
    cfg->vad_enable_channel_trigger = false;
    cfg->wakenet_init = selected != NULL;
    cfg->wakenet_model_name = selected;
    cfg->wakenet_model_name_2 = NULL;
    /* Use the official bounded WebRTC AGC for the relatively quiet codec
     * signal feeding MultiNet/ASR. VAD energy metadata remains pre-AGC.
     * No WakeNet model/reference or separate microphone owner is introduced. */
    cfg->agc_init = true;
    cfg->agc_mode = AFE_AGC_MODE_WEBRTC;
    cfg->agc_compression_gain_db = 12;
    cfg->agc_target_level_dbfs = 9;
    /* Keep the official ring capacity. ESP-SR reserves both min_speech and
     * delay history in this same ring, in addition to incoming PCM. Shrinking
     * it to four or eight 32 ms frames leaves no room beyond our 256 ms
     * history and eventually rejects every new feed. That history remains
     * local to ESP-SR and is never exported by the owner. */
    cfg->debug_init = false;
    cfg->fixed_first_channel = true;
    cfg->fixed_output_channel = true;
    cfg->output_playback_channel = false;
    cfg->memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM;

    afe_config_t *checked = afe_config_check(cfg);
    if (!checked || checked->aec_init != reference_input || checked->wakenet_init != (selected != NULL) ||
        (selected && (!checked->wakenet_model_name ||
                      strcmp(checked->wakenet_model_name, selected) != 0 ||
                      checked->wakenet_model_name_2 != NULL))) {
        afe_config_free(cfg);
        reason(afe, "AFE configuration rejected: exact model/reference policy was not preserved");
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        return ESP_ERR_INVALID_STATE;
    }
    afe->iface = esp_afe_handle_from_config(checked);
    ESP_LOGI("bot_audio", "AFE ring=%d frames, VAD onset history=%d+%d ms",
             checked->afe_ringbuf_size, checked->vad_min_speech_ms, checked->vad_delay_ms);
    ESP_LOGI("bot_audio", "Official AGC enabled=%d mode=%d compression=%d dB target=-%d dBFS",
             checked->agc_init, checked->agc_mode, checked->agc_compression_gain_db,
             checked->agc_target_level_dbfs);
    if (afe->iface) afe->data = afe->iface->create_from_config(checked);
    afe_config_free(cfg);
    if (!afe->iface || !afe->data) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "Official AFE allocation/initialization failed");
        return ESP_ERR_NO_MEM;
    }
    int samples = afe->iface->get_feed_chunksize(afe->data);
    int fetched = afe->iface->get_fetch_chunksize(afe->data);
    int input_channels = afe->iface->get_feed_channel_num(afe->data);
    int output_channels = afe->iface->get_fetch_channel_num(afe->data);
    int sample_rate = afe->iface->get_samp_rate(afe->data);
    ESP_LOGI("bot_audio", "AFE format: feed=%d/%d channels fetch=%d/%d channels rate=%d",
             samples, input_channels, fetched, output_channels, sample_rate);
    if (samples <= 0 || samples > 4096 || fetched <= 0 || fetched > 4096 ||
        input_channels != (reference_input ? 4 : 1) || output_channels != 1 || sample_rate != BOT_AUDIO_SAMPLE_RATE) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "AFE format does not match the 16 kHz mono owner contract");
        return ESP_ERR_INVALID_SIZE;
    }
    afe->reference_input = reference_input;
    afe->feed_samples = (size_t)samples;
    afe->fetch_samples = (size_t)fetched;
    /* Keep one official MN instance for explicitly authorized local controls,
     * even when a dedicated WakeNet owns passive wake. It is not inferred in
     * passive WakeNet mode. No second command/model owner is created. */
    esp_err_t keyword_error = ESP_ERR_NOT_FOUND;
    if (fallback_enabled && multinet_name) {
        keyword_error = bot_audio_keyword_open(&afe->keyword, multinet_name, afe->fetch_samples);
        if (keyword_error != ESP_OK) {
            bot_audio_keyword_close(&afe->keyword);
            ESP_LOGE("bot_audio", "MultiNet7 unavailable: %s", esp_err_to_name(keyword_error));
        }
    }
    if (selected) {
        afe->wake_status = BOT_AUDIO_WAKE_READY;
        afe->wake_backend = BOT_AUDIO_WAKE_BACKEND_WAKENET;
        reason(afe, reference_input ? "Dedicated wake model and hardware-reference AFE ready; acoustic calibration pending" :
                    "Dedicated full 小呆小呆 WakeNet ready; half-duplex suppression active");
    } else if (fallback_enabled && multinet_name) {
        if (keyword_error == ESP_OK) {
            afe->wake_status = BOT_AUDIO_WAKE_READY;
            afe->wake_backend = BOT_AUDIO_WAKE_BACKEND_MULTINET7;
            snprintf(afe->model_name, sizeof(afe->model_name), "%s", multinet_name);
            reason(afe, "MultiNet7 full 小呆小呆 command ready; acoustic accuracy and CPU/RAM not yet validated");
        } else {
            /* A missing/failed keyword engine does not take manual VAD/codec
             * interaction down with it. No alternative word is accepted. */
            afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
            reason(afe, "MultiNet7 initialization failed; manual interaction and local VAD remain available");
        }
    } else if (fallback_enabled && afe->models) {
        reason(afe, "WAKE_MODEL_MISSING: mn7_cn absent; build with SR_MN_CN_MULTINET7_QUANT and flash srmodels.bin");
    }
    return ESP_OK;
}

void bot_audio_afe_close(bot_audio_afe_t *afe)
{
    bot_audio_keyword_close(&afe->keyword);
    if (afe->data && afe->iface) afe->iface->destroy(afe->data);
    afe->data = NULL;
    afe->iface = NULL;
    if (afe->models) esp_srmodel_deinit_with_refcount(afe->models);
    afe->models = NULL;
}

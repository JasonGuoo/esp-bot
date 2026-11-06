/* SPDX-License-Identifier: MIT
 * Thin integration of Espressif ESP-SR 2.6.1; see README for upstream references.
 * Signal processing and inference are entirely the official AFE/WakeNet engines.
 */
#include "bot_audio_afe.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "esp_afe_config.h"
#include "esp_afe_sr_models.h"
#include "esp_partition.h"

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

static bool model_directory_valid(const esp_partition_t *part, const char *wanted)
{
    uint32_t models;
    size_t cursor = 4;
    size_t first_payload = part->size;
    bool found = false;
    if (!read_u32(part, 0, &models) || models == 0 || models > 32) return false;
    for (uint32_t i = 0; i < models; ++i) {
        char name[SRMODEL_STRING_LENGTH];
        uint32_t files;
        if (!read_name(part, cursor, name)) return false;
        bool matching = strcmp(name, wanted) == 0;
        cursor += SRMODEL_STRING_LENGTH;
        if (!read_u32(part, cursor, &files) || files == 0 || files > 64) return false;
        cursor += 4;
        bool has_info = false;
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
        }
        if (!has_info) return false;
        found |= matching;
    }
    return found && cursor <= first_payload;
}

static void reason(bot_audio_afe_t *afe, const char *text)
{
    snprintf(afe->reason, sizeof(afe->reason), "%s", text);
}

esp_err_t bot_audio_afe_open(bot_audio_afe_t *afe, const char *label, const char *name)
{
    memset(afe, 0, sizeof(*afe));
    afe->wake_status = BOT_AUDIO_WAKE_MODEL_MISSING;
    reason(afe, "WAKE_MODEL_MISSING: exact custom model is not configured");
    char *selected = NULL;
    if (name[0] != '\0') {
        const esp_partition_t *partition = esp_partition_find_first(
            ESP_PARTITION_TYPE_DATA, ESP_PARTITION_SUBTYPE_ANY, label);
        if (!partition) {
            reason(afe, "WAKE_MODEL_MISSING: model partition does not exist");
        } else if (!model_directory_valid(partition, name)) {
            reason(afe, "WAKE_MODEL_MISSING: blank, malformed, or nonmatching model directory");
        } else {
            afe->models = esp_srmodel_init(label);
            if (!afe->models) {
                reason(afe, "WAKE_MODEL_MISSING: official model loader could not map partition");
            } else {
                /* Exact lookup; never select the first wn9/wn9s stock model. */
                for (int i = 0; i < afe->models->num; ++i) {
                    if (strcmp(afe->models->model_name[i], name) == 0) {
                        char *words = esp_srmodel_get_wake_words(afe->models, afe->models->model_name[i]);
                        /* Upstream joins multiple keywords with ';'. Whole-string
                         * equality requires exactly one complete acoustic keyword. */
                        if (words && strcmp(words, BOT_AUDIO_WAKE_PHRASE) == 0) {
                            selected = afe->models->model_name[i];
                        }
                        free(words); /* get_wake_words returns an allocation */
                        break;
                    }
                }
                reason(afe, selected ? "Exact full-phrase model loaded; awaiting AFE initialization"
                                    : "WAKE_MODEL_MISSING: model metadata is not exactly 小呆小呆");
            }
        }
    }

    /* A single selected ADC lane; board routing awaits hardware validation. The physical second slot is NOT advertised as
     * a second microphone or a synchronized playback reference. */
    afe_config_t *cfg = afe_config_init("M", afe->models, AFE_TYPE_SR, AFE_MODE_LOW_COST);
    if (!cfg) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "Official AFE configuration allocation failed");
        return ESP_ERR_NO_MEM;
    }
    cfg->aec_init = false;
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
    cfg->agc_init = false;
    cfg->afe_ringbuf_size = 4;
    cfg->debug_init = false;
    cfg->fixed_first_channel = true;
    cfg->fixed_output_channel = true;
    cfg->output_playback_channel = false;
    cfg->memory_alloc_mode = AFE_MEMORY_ALLOC_MORE_PSRAM;

    afe_config_t *checked = afe_config_check(cfg);
    if (!checked || checked->aec_init || checked->wakenet_init != (selected != NULL) ||
        (selected && (!checked->wakenet_model_name ||
                      strcmp(checked->wakenet_model_name, selected) != 0 ||
                      checked->wakenet_model_name_2 != NULL))) {
        afe_config_free(cfg);
        reason(afe, "AFE configuration rejected: exact model/reference policy was not preserved");
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        return ESP_ERR_INVALID_STATE;
    }
    afe->iface = esp_afe_handle_from_config(checked);
    if (afe->iface) afe->data = afe->iface->create_from_config(checked);
    afe_config_free(cfg);
    if (!afe->iface || !afe->data) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "Official AFE allocation/initialization failed");
        return ESP_ERR_NO_MEM;
    }
    int samples = afe->iface->get_feed_chunksize(afe->data);
    if (samples <= 0 || samples > 4096 ||
        afe->iface->get_feed_channel_num(afe->data) != 1 ||
        afe->iface->get_samp_rate(afe->data) != BOT_AUDIO_SAMPLE_RATE) {
        afe->wake_status = BOT_AUDIO_WAKE_UNAVAILABLE;
        reason(afe, "AFE format does not match the 16 kHz mono owner contract");
        return ESP_ERR_INVALID_SIZE;
    }
    afe->feed_samples = (size_t)samples;
    if (selected) {
        afe->wake_status = BOT_AUDIO_WAKE_READY;
        reason(afe, "Full 小呆小呆 model ready; AEC/reference unverified, auto barge-in disabled");
    }
    return ESP_OK;
}

void bot_audio_afe_close(bot_audio_afe_t *afe)
{
    if (afe->data && afe->iface) afe->iface->destroy(afe->data);
    afe->data = NULL;
    afe->iface = NULL;
    if (afe->models) esp_srmodel_deinit_with_refcount(afe->models);
    afe->models = NULL;
}

/* SPDX-License-Identifier: MIT */
#pragma once
#include "bot_audio.h"
#include "esp_afe_sr_iface.h"
#include "model_path.h"

typedef struct {
    srmodel_list_t *models;
    const esp_afe_sr_iface_t *iface;
    esp_afe_sr_data_t *data;
    size_t feed_samples;
    bot_audio_wake_status_t wake_status;
    char reason[BOT_AUDIO_REASON_BYTES + 1];
} bot_audio_afe_t;

esp_err_t bot_audio_afe_open(bot_audio_afe_t *afe, const char *partition_label,
                            const char *model_name);
void bot_audio_afe_close(bot_audio_afe_t *afe);

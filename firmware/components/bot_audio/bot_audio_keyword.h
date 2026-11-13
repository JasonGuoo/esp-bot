/* SPDX-License-Identifier: MIT */
#pragma once
#include "bot_audio.h"
#include "esp_mn_iface.h"

#define BOT_AUDIO_MULTINET_MODEL "mn7_cn"
#define BOT_AUDIO_MULTINET_COMMAND "xiao dai xiao dai"

/* One instance, owned by the capture/AFE worker. No recording or upload cache. */
typedef struct {
    const esp_mn_iface_t *iface;
    model_iface_data_t *data;
    size_t samples;
    int16_t *frame;
    size_t buffered;
    uint32_t silence_samples;
    bool commands_owned, dirty;
    float last_score; /* native MultiNet score; not a calibrated probability */
} bot_audio_keyword_t;

esp_err_t bot_audio_keyword_open(bot_audio_keyword_t *keyword, char *model_name,
                                  size_t afe_samples);
void bot_audio_keyword_close(bot_audio_keyword_t *keyword);
void bot_audio_keyword_reset(bot_audio_keyword_t *keyword);
bool bot_audio_keyword_detect(bot_audio_keyword_t *keyword, int16_t *samples, size_t count,
                               float volume_dbfs, bool allowed);

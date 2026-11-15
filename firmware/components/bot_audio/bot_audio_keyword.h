/* SPDX-License-Identifier: MIT */
#pragma once
#include "bot_audio.h"
#include "esp_mn_iface.h"

#define BOT_AUDIO_MULTINET_MODEL "mn7_cn"
#define BOT_AUDIO_MULTINET_COMMAND "xiao dai xiao dai"

/* One instance, owned by the capture/AFE worker. No pre-wake recording cache. */
typedef struct {
    const esp_mn_iface_t *iface;
    model_iface_data_t *data;
    size_t samples;
    int16_t *frame;
    size_t buffered;
    bool previous_speech;
    bool commands_owned, dirty, commands_enabled, available;
    uint8_t threshold_percent;
    uint16_t input_peak;
    float last_score; /* native MultiNet score; not a calibrated probability */
    float last_candidate_score;
    int last_candidate_command;
    uint64_t runs, timeouts, rejections;
    uint32_t last_us, max_us;
} bot_audio_keyword_t;

esp_err_t bot_audio_keyword_open(bot_audio_keyword_t *keyword, char *model_name,
                                  size_t afe_samples);
void bot_audio_keyword_close(bot_audio_keyword_t *keyword);
void bot_audio_keyword_reset(bot_audio_keyword_t *keyword);
esp_err_t bot_audio_keyword_set_policy(bot_audio_keyword_t *keyword, bool commands_enabled,
                                       uint8_t wake_threshold_percent);
/* 0: none, 1: complete wake phrase, otherwise a bot_voice_command_t ID. */
int bot_audio_keyword_detect(bot_audio_keyword_t *keyword, int16_t *samples, size_t count,
                              float volume_dbfs, bool speech, bool allowed);

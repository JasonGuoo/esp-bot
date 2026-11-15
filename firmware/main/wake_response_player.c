/* SPDX-License-Identifier: MIT */
#include "wake_response_player.h"
#include <stdatomic.h>
#include <stdlib.h>
#include "esp_log.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

typedef struct {
    bot_session_token_t token;
    const bot_response_entry_t *entry;
    unsigned generation;
    uint8_t gain_percent;
    bool preview;
} response_job_t;

struct bot_response_player {
    bot_bus_t *bus;
    bot_audio_t *audio;
    QueueHandle_t jobs;
    atomic_uint generation;
    atomic_uint stack_free;
};

static const char *TAG = "wake_response";

static bool current(const bot_response_player_t *player, const response_job_t *job)
{
    return atomic_load(&player->generation) == job->generation;
}

static void report_failure(bot_response_player_t *player, const response_job_t *job, esp_err_t error)
{
    if (!current(player, job)) return;
    ESP_LOGW(TAG, "%s failed: %s", job->entry->id, esp_err_to_name(error));
    const bot_event_t event = {
        .kind = BOT_EVENT_LOCAL_RESPONSE_FAILED, .source = BOT_SOURCE_SYSTEM,
        .observed_ms = (uint32_t)(esp_timer_get_time() / 1000), .token = job->token,
    };
    /* A failed critical enqueue latches the bus fault; runtime then closes all gates. */
    (void)bot_bus_post(player->bus, &event, 0);
}

static void play(bot_response_player_t *player, const response_job_t *job)
{
    if (!current(player, job)) return;
    bot_response_audio_stream_t stream;
    if (!bot_responses_audio_open(&stream, job->entry)) {
        report_failure(player, job, ESP_ERR_INVALID_SIZE);
        return;
    }
    const bot_audio_token_t token = {job->token.epoch, job->token.session_id,
        job->token.turn_id, job->token.focus_generation};
    uint32_t playback_id = 0;
    esp_err_t error;
    const int64_t start_deadline = esp_timer_get_time() + 300000;
    do {
        if (!current(player, job)) return;
        error = job->preview ? bot_audio_playback_begin_preview(player->audio, token.epoch, &playback_id) :
            bot_audio_playback_begin_local(player->audio, &token, &playback_id);
        if (error != ESP_ERR_INVALID_STATE && error != ESP_ERR_TIMEOUT) break;
        vTaskDelay(pdMS_TO_TICKS(5)); /* allow the previous cancellation to drain */
    } while (esp_timer_get_time() < start_deadline);
    if (error != ESP_OK) {
        report_failure(player, job, error);
        return;
    }
    /* Stop can race the successful BEGIN. Never leave that late job playing. */
    if (!current(player, job)) {
        (void)bot_audio_playback_cancel(player->audio, playback_id);
        return;
    }
    ESP_LOGI(TAG, "id=%s scene=%s expression=%s gain=%u samples=%lu playback=%lu",
             job->entry->id, job->entry->scene, job->entry->expression,
             (unsigned)job->gain_percent, (unsigned long)job->entry->pcm_samples,
             (unsigned long)playback_id);
    int16_t samples[BOT_AUDIO_PLAYBACK_SAMPLES];
    const unsigned gain = job->gain_percent;
    const int64_t deadline = esp_timer_get_time() +
        (int64_t)job->entry->pcm_samples * 1000000 / BOT_RESPONSES_SAMPLE_RATE + 2000000;
    size_t count;
    while ((count = bot_responses_audio_read(&stream, samples, BOT_AUDIO_PLAYBACK_SAMPLES)) != 0) {
        for (size_t i = 0; i < count; i++) samples[i] = (int16_t)((int32_t)samples[i] * (int)gain / 100);
        do {
            if (!current(player, job)) goto cancelled;
            if (esp_timer_get_time() > deadline) { error = ESP_ERR_TIMEOUT; goto failed; }
            error = bot_audio_playback_write(player->audio, playback_id, samples, count);
            if (error == ESP_ERR_TIMEOUT) vTaskDelay(pdMS_TO_TICKS(5));
        } while (error == ESP_ERR_TIMEOUT);
        if (error != ESP_OK) goto failed;
    }
    if (stream.failed) { error = ESP_ERR_INVALID_SIZE; goto failed; }
    do {
        if (!current(player, job)) goto cancelled;
        if (esp_timer_get_time() > deadline) { error = ESP_ERR_TIMEOUT; goto failed; }
        error = bot_audio_playback_end(player->audio, playback_id);
        if (error == ESP_ERR_TIMEOUT) vTaskDelay(pdMS_TO_TICKS(5));
    } while (error == ESP_ERR_TIMEOUT);
    if (error != ESP_OK) goto failed;
    /* Only the audio owner's real DONE/FAILED event opens the next capture. */
    return;
failed:
    (void)bot_audio_playback_cancel(player->audio, playback_id);
    report_failure(player, job, error);
    return;
cancelled:
    (void)bot_audio_playback_cancel(player->audio, playback_id);
}

static void worker(void *context)
{
    bot_response_player_t *player = context;
    response_job_t job;
    for (;;) if (xQueueReceive(player->jobs, &job, portMAX_DELAY) == pdTRUE) {
        play(player, &job);
        atomic_store(&player->stack_free, uxTaskGetStackHighWaterMark(NULL));
    }
}

esp_err_t bot_response_player_create(bot_bus_t *bus, bot_audio_t *audio,
                                     bot_response_player_t **out)
{
    if (!bus || !audio || !out) return ESP_ERR_INVALID_ARG;
    *out = NULL;
    bot_response_player_t *player = calloc(1, sizeof(*player));
    if (!player) return ESP_ERR_NO_MEM;
    player->bus = bus;
    player->audio = audio;
    atomic_init(&player->generation, 1);
    atomic_init(&player->stack_free, 0);
    player->jobs = xQueueCreate(1, sizeof(response_job_t));
    if (!player->jobs) { free(player); return ESP_ERR_NO_MEM; }
    if (xTaskCreate(worker, "wake_reply", 4096, player, 4, NULL) != pdPASS) {
        vQueueDelete(player->jobs);
        free(player);
        return ESP_ERR_NO_MEM;
    }
    *out = player;
    return ESP_OK;
}

bool bot_response_player_queue(bot_response_player_t *player,
                                const bot_session_token_t *token,
                                const bot_response_entry_t *entry, uint8_t gain_percent)
{
    if (!player || !token || !token->session_id || !token->turn_id || !entry || gain_percent > 100) return false;
    const response_job_t job = { .token = *token, .entry = entry,
        .generation = atomic_fetch_add(&player->generation, 1) + 1, .gain_percent = gain_percent };
    return xQueueOverwrite(player->jobs, &job) == pdTRUE;
}

bool bot_response_player_preview(bot_response_player_t *player,
                                  const bot_session_token_t *token,
                                  const bot_response_entry_t *entry, uint8_t gain_percent)
{
    if (!player || !token || !token->epoch || token->session_id || token->turn_id ||
        !entry || gain_percent > 100) return false;
    const response_job_t job = { .token = *token, .entry = entry,
        .generation = atomic_fetch_add(&player->generation, 1) + 1,
        .gain_percent = gain_percent, .preview = true };
    return xQueueOverwrite(player->jobs, &job) == pdTRUE;
}

void bot_response_player_cancel(bot_response_player_t *player)
{
    if (!player) return;
    atomic_fetch_add(&player->generation, 1);
    (void)bot_audio_playback_cancel(player->audio, 0);
}

uint32_t bot_response_player_stack_free(const bot_response_player_t *player)
{
    return player ? atomic_load(&player->stack_free) : 0;
}

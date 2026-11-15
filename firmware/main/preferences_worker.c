#include "preferences_worker.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"

static QueueHandle_t requests, results;
static StaticTask_t task;
static StackType_t stack[4096 / sizeof(StackType_t)] __attribute__((aligned(portBYTE_ALIGNMENT)));

static void persist(void *context)
{
    bot_platform_t *platform = context;
    bot_preferences_request_t request;
    bot_preferences_result_t result = {0};
    for (;;) {
        if (xQueueReceive(requests, &request, portMAX_DELAY) != pdTRUE) continue;
        if (request.voice_revision && request.voice_revision != result.voice_revision) {
            result.voice_error = bot_platform_save_voice_settings(platform, &request.voice);
            result.voice_revision = request.voice_revision;
        }
        if (request.theme_revision && request.theme_revision != result.theme_revision) {
            result.theme_error = bot_platform_save_theme(platform, request.theme);
            result.theme_revision = request.theme_revision;
        }
        if (request.orientation_revision && request.orientation_revision != result.orientation_revision) {
            result.orientation_error = bot_platform_save_camera_orientation(platform, request.orientation);
            result.orientation_revision = request.orientation_revision;
        }
        /* Acknowledgements are cumulative as well. No flash operation or UI
         * access occurs on the runtime/Stop task or the LVGL task. */
        xQueueOverwrite(results, &result);
    }
}

esp_err_t bot_preferences_start(bot_platform_t *platform)
{
    if (!platform || requests) return ESP_ERR_INVALID_STATE;
    requests = xQueueCreate(1, sizeof(bot_preferences_request_t));
    results = xQueueCreate(1, sizeof(bot_preferences_result_t));
    if (requests && results && xTaskCreateStatic(persist, "bot_preferences", sizeof(stack),
                                                platform, 1, stack, &task)) return ESP_OK;
    if (requests) vQueueDelete(requests);
    if (results) vQueueDelete(results);
    requests = results = NULL;
    return ESP_ERR_NO_MEM;
}

bool bot_preferences_submit(const bot_preferences_request_t *request)
{
    return requests && request && xQueueOverwrite(requests, request) == pdTRUE;
}

bool bot_preferences_take_result(bot_preferences_result_t *result)
{
    return results && result && xQueueReceive(results, result, 0) == pdTRUE;
}

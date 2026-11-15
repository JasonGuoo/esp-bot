/* Host regression using the pinned official ESP-SR interface, no acoustic model
 * substitute: this checks our frame/boundary adapter, not recognition accuracy. */
#include "bot_audio_keyword.h"
#include <assert.h>
#include <math.h>
#include <stdio.h>

static unsigned frames, cleans, detect_at = 40;
static esp_mn_results_t result = { .state = ESP_MN_STATE_DETECTED, .num = 1,
    .command_id = {1}, .phrase_id = {42}, .prob = {0.95f} };
int64_t esp_timer_get_time(void) { static int64_t clock; return ++clock; }
static void clean(model_iface_data_t *data) { (void)data; frames = 0; ++cleans; }
static esp_mn_state_t detect(model_iface_data_t *data, int16_t *samples) {
    (void)data; (void)samples;
    return ++frames >= detect_at ? ESP_MN_STATE_DETECTED : ESP_MN_STATE_DETECTING;
}
static esp_mn_results_t *results(model_iface_data_t *data) { (void)data; return &result; }
int main(void) {
    const esp_mn_iface_t iface = { .detect = detect, .clean = clean, .get_results = results };
    int16_t pcm[512] = {1};
    bot_audio_keyword_t keyword = { .iface = &iface, .data = (void *)&frames,
        .samples = 512, .frame = pcm, .available = true, .threshold_percent = 80 };
    /* A complete quiet phrase must survive longer than the former 600 ms gate. */
    for (unsigned i = 0; i < 39; ++i)
        assert(bot_audio_keyword_detect(&keyword, pcm, 512, -54.0f, true, true) == 0);
    assert(cleans == 0);
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -54.0f, true, true) == 1);
    assert(cleans == 1);
    /* Two incomplete utterances cannot share decoder state across AFE silence. */
    for (unsigned i = 0; i < 8; ++i)
        assert(bot_audio_keyword_detect(&keyword, pcm, 512, -54.0f, true, true) == 0);
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -62.0f, false, true) == 0);
    assert(cleans == 2 && frames == 1);
    for (unsigned i = 0; i < 8; ++i)
        assert(bot_audio_keyword_detect(&keyword, pcm, 512, -54.0f, true, true) == 0);
    assert(frames == 9);
    /* Closing the permission gate immediately invalidates all partial input. */
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -20.0f, true, false) == 0);
    assert(cleans == 3 && frames == 0);
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, NAN, true, true) == 0);
    assert(frames == 0);
    detect_at = 1;
    result.command_id[0] = BOT_VOICE_COMMAND_VOLUME_UP;
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -20.0f, true, true) == 0);
    keyword.commands_enabled = true;
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -20.0f, true, true) == BOT_VOICE_COMMAND_VOLUME_UP);
    result.command_id[0] = 999;
    assert(bot_audio_keyword_detect(&keyword, pcm, 512, -20.0f, true, true) == 0);
    puts("PASS keyword frame retention, VAD boundaries, permission closure and invalid energy");
    return 0;
}

/* Host check of the real wake-response selector; no device or model required. */
#include "bot_responses.h"
#include <assert.h>
#include <string.h>

static void check_quiet_scene(bot_response_context_t context, const char *scene, unsigned gain)
{
    bot_responses_state_t state;
    bot_responses_init(&state, 0x12345678);
    const char *previous = NULL;
    for (unsigned i = 0; i < 500; ++i) {
        const bot_response_entry_t *reply = bot_responses_select(&state, &context);
        assert(reply && strcmp(reply->scene, scene) == 0 && reply->gain_percent == gain);
        assert(!strstr(reply->text, "小声") && !strstr(reply->text, "轻声") && !strstr(reply->text, "轻轻"));
        assert(!previous || strcmp(previous, reply->text) != 0);
        previous = reply->text;
    }
}

int main(void)
{
    check_quiet_scene((bot_response_context_t){.time_valid = true, .local_hour = 23}, "night", 30);
    check_quiet_scene((bot_response_context_t){.ambient_sample_valid = true, .ambient_dark = true}, "dark", 45);
    return 0;
}

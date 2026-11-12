/* SPDX-License-Identifier: MIT */
#include <assert.h>
#include <math.h>
#include "../followup_vad.h"

int main(void)
{
    /* Quiet audio after an intentional touch still uses the original VAD. */
    assert(bot_audio_speech_frame(true, -61.0f, false, false));
    assert(!bot_audio_speech_frame(false, -10.0f, false, false));

    /* After a reply, simulate quiet room noise, speech onset, a soft suffix
     * and the end of the utterance using the official AFE's frame values. */
    bool speaking = false;
    assert(!bot_audio_speech_frame(true, NAN, true, speaking));
    assert(!bot_audio_speech_frame(true, -42.1f, true, speaking));
    assert(!bot_audio_speech_frame(false, -20.0f, true, speaking));
    speaking = bot_audio_speech_frame(true, -42.0f, true, speaking);
    assert(speaking);
    speaking = bot_audio_speech_frame(true, -61.0f, true, speaking);
    assert(speaking);
    speaking = bot_audio_speech_frame(false, -30.0f, true, speaking);
    assert(!speaking);
    assert(!bot_audio_speech_frame(true, -61.0f, true, speaking));
    return 0;
}

/* SPDX-License-Identifier: MIT */
#include <assert.h>
#include "../followup_vad.h"

int main(void)
{
    /* Quiet audio after an intentional touch still uses the original VAD. */
    assert(bot_audio_speech_frame(true, false, false, false));
    assert(!bot_audio_speech_frame(false, true, false, false));

    /* Post-AGC VAD may classify amplified room noise as speech. Authorization
     * uses the actual ADC energy; exact integer boundary and full-scale input
     * remain safe for either native feed size. */
    const int16_t quiet[] = {100, -100, 100, -100};
    const int16_t below[] = {260, -260};
    const int16_t above[] = {261, -261};
    const int16_t loud[] = {INT16_MIN, INT16_MAX};
    assert(!bot_audio_pcm_speech_energy(NULL, 4));
    assert(!bot_audio_pcm_speech_energy(above, 0));
    assert(!bot_audio_pcm_speech_energy(above, 4097));
    assert(!bot_audio_pcm_speech_energy(quiet, 4));
    assert(!bot_audio_pcm_speech_energy(below, 2));
    assert(bot_audio_pcm_speech_energy(above, 2));
    assert(bot_audio_pcm_speech_energy(loud, 2));

    /* After a reply, simulate quiet room noise, speech onset, a soft suffix
     * and the end of the utterance. A VAD decision alone cannot start it. */
    bool speaking = false;
    assert(!bot_audio_speech_frame(true, false, true, speaking));
    assert(!bot_audio_speech_frame(false, true, true, speaking));
    speaking = bot_audio_speech_frame(true, true, true, speaking);
    assert(speaking);
    speaking = bot_audio_speech_frame(true, false, true, speaking);
    assert(speaking);
    speaking = bot_audio_speech_frame(false, true, true, speaking);
    assert(!speaking);
    assert(!bot_audio_speech_frame(true, false, true, speaking));
    return 0;
}

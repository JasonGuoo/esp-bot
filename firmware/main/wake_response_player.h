/* SPDX-License-Identifier: MIT
 * Playback adapter only. bot_runtime chooses whether/what/when to answer.
 */
#pragma once
#include "bot_audio.h"
#include "bot_bus.h"
#include "bot_responses.h"

typedef struct bot_response_player bot_response_player_t;

esp_err_t bot_response_player_create(bot_bus_t *bus, bot_audio_t *audio,
                                     bot_response_player_t **out);
/** Nonblocking; immutable Flash entry and runtime token are copied into a job. */
bool bot_response_player_queue(bot_response_player_t *player,
                                const bot_session_token_t *token,
                                const bot_response_entry_t *entry, uint8_t gain_percent);
/** Explicit settings audition: no session token and never opens the microphone. */
bool bot_response_player_preview(bot_response_player_t *player,
                                  const bot_session_token_t *token,
                                  const bot_response_entry_t *entry, uint8_t gain_percent);
/** Invalidates queued and in-flight work before cancelling the audio owner. */
void bot_response_player_cancel(bot_response_player_t *player);

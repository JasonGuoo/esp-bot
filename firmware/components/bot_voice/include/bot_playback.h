/* Shared semantic playback purpose; never grants a capture/session lease. */
#pragma once
typedef enum {
    BOT_PLAYBACK_FINAL = 0, /* legacy replies retain their terminal semantics */
    BOT_PLAYBACK_WAKE_ACK,
    BOT_PLAYBACK_TASK_ACK,
    BOT_PLAYBACK_PROGRESS,
    BOT_PLAYBACK_CONTENT,
    BOT_PLAYBACK_PREVIEW,
} bot_playback_purpose_t;

/* SPDX-License-Identifier: MIT
 * ESP32-S31-Korvo-1 local audio owner. No runtime, transport, or UI dependency.
 */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"

#ifdef __cplusplus
extern "C" {
#endif

#define BOT_AUDIO_SAMPLE_RATE 16000U
#define BOT_AUDIO_WAKE_PHRASE "小呆小呆"
#define BOT_AUDIO_MODEL_NAME_BYTES 63
#define BOT_AUDIO_REASON_BYTES 127
#define BOT_AUDIO_PLAYBACK_SAMPLES 320U /* 20 ms, mono signed PCM16LE */

typedef struct bot_audio bot_audio_t;
/* Copy these four fields explicitly from the runtime token; no shared ABI cast. */
typedef struct {
    uint32_t epoch, session_id, turn_id, focus_generation;
} bot_audio_token_t;

typedef enum {
    BOT_AUDIO_WAKE_MODEL_MISSING = 0,
    BOT_AUDIO_WAKE_READY,
    BOT_AUDIO_WAKE_UNAVAILABLE,
} bot_audio_wake_status_t;

typedef enum {
    BOT_AUDIO_EVENT_HEALTH = 0,
    BOT_AUDIO_EVENT_WAKE_STATUS,
    BOT_AUDIO_EVENT_WAKE_DETECTED,
    BOT_AUDIO_EVENT_VAD_STARTED,
    BOT_AUDIO_EVENT_VAD_ENDED,
    BOT_AUDIO_EVENT_PLAYBACK_STARTED,
    BOT_AUDIO_EVENT_PLAYBACK_DONE,
    BOT_AUDIO_EVENT_PLAYBACK_CANCELLED,
    BOT_AUDIO_EVENT_PLAYBACK_FAILED,
} bot_audio_event_kind_t;

typedef struct {
    bot_audio_event_kind_t kind;
    int64_t observed_us;          /* local esp_timer monotonic time, NOT capture time */
    bot_audio_token_t token;      /* operation's original token, never a newer session */
    esp_err_t error;
    bool available;
    bot_audio_wake_status_t wake_status;
    bool echo_safe;              /* false for VAD: no validated reference/AEC */
    bool confidence_available;   /* false: AFE wake event has no calibrated confidence */
    float confidence;
    uint32_t playback_id;
    int wake_word_index;
    const char *wake_phrase;      /* static BOT_AUDIO_WAKE_PHRASE, wake events only */
    const char *reason;           /* borrowed for this callback only */
} bot_audio_event_t;

typedef struct {
    const int16_t *samples;       /* borrowed, mono; copy before callback returns */
    size_t sample_count;
    uint32_t sample_rate;
    uint64_t sequence;           /* capture-read sequence, monotonic within this start */
    int64_t read_started_us;
    int64_t observed_us;          /* read completion; hardware capture time unavailable */
    bot_audio_token_t token;
    bool enhanced;               /* false: primary ADC lane; AFE is local detection */
    bool echo_cancelled;          /* false until a measured hardware reference exists */
} bot_audio_pcm_t;

/* Callbacks run on audio worker threads (or the lifecycle caller at startup).
 * They can be concurrent, must stay short/nonblocking, and must copy borrowed
 * event strings/PCM before returning. Queue work to the runtime/transport owner. */
typedef void (*bot_audio_event_cb_t)(void *context, const bot_audio_event_t *event);
typedef void (*bot_audio_pcm_cb_t)(void *context, const bot_audio_pcm_t *frame);

typedef struct {
    /* Strings are copied by create. Name must exactly match the flash model entry.
     * Full keyword metadata must exactly equal BOT_AUDIO_WAKE_PHRASE; no aliases,
     * split-name counting, fallback WakeNet model, or VAD wake are allowed. */
    const char *wake_model_name;
    const char *model_partition;
    uint8_t output_volume;        /* codec percentage, not an acoustic dB limit */
    float microphone_gain_db;
    uint32_t wake_debounce_ms;
    uint32_t playback_tail_ms;    /* suppress self-wake during playback and its tail */
    bot_audio_event_cb_t on_event;
    bot_audio_pcm_cb_t on_pcm;
    void *context;
} bot_audio_config_t;

typedef struct {
    bool started, capture_running, afe_running, playback_running;
    bool session_open, pcm_delivery_enabled;
    bool codec_available, vad_available, wake_available;
    bool hardware_capture_timestamps; /* false */
    bool aec_available;               /* false; no fabricated reference */
    bool automatic_barge_in;          /* false; use explicit local cancellation */
    bool voice_identity_available;    /* false; ESP-SR WakeNet is not voice identity */
    bool night_wake_supported;        /* true while started; audio has no sleep timer */
    bot_audio_wake_status_t wake_status;
    esp_err_t last_error;
    /* Read/result/write completion observations, never measured acoustic times. */
    int64_t last_capture_us, last_afe_result_us, last_playback_us;
    uint64_t capture_frames, afe_frames, delivered_pcm_frames;
    uint64_t gated_pcm_frames, capture_errors, afe_errors, playback_errors;
    uint64_t wake_detections, suppressed_wakes, playback_underruns;
    uint32_t rx_discard_frames;       /* remaining DMA FIFO fence before PCM release */
    uint32_t playback_id;
    char model_name[BOT_AUDIO_MODEL_NAME_BYTES + 1];
    char reason[BOT_AUDIO_REASON_BYTES + 1];
} bot_audio_status_t;

bot_audio_config_t bot_audio_default_config(void);
/** One instance per board. Only the application's audio owner calls lifecycle
 * methods; bsp_i2c_init is idempotent and shares the bus with display/camera. */
esp_err_t bot_audio_create(const bot_audio_config_t *config, bot_audio_t **out);
/** Opens one ES8389 IN_OUT device; starts capture, AFE fetch, and playback tasks.
 * Missing/blank/wrong wake model is a reported degraded state, not a boot error.
 * Start does NOT open a conversation or enable PCM delivery. */
esp_err_t bot_audio_start(bot_audio_t *audio);
/** Revoke gates/cancel playback and wait for workers. Timeout leaves resources
 * alive; retry stop later. Never frees a live task's buffers. Not callback-safe.
 * timeout_ms must be 0..10000; it bounds the worker wait, not hardware cleanup. */
esp_err_t bot_audio_stop(bot_audio_t *audio, uint32_t timeout_ms);
/** Calls stop first. On timeout the handle remains valid and must not be freed.
 * The caller must first quiesce all external producers/status readers. */
esp_err_t bot_audio_destroy(bot_audio_t *audio, uint32_t timeout_ms);

/** Runtime's explicit permission gate. NULL token closes the local session.
 * deliver_pcm is a SECOND permission for a currently authenticated, encrypted
 * transport; it must stay false for local-only/unauthenticated sessions.
 * No wake/VAD event opens this gate automatically. New gates discard the entire
 * possible RX DMA backlog before emitting raw PCM; no vad_cache is released.
 * Idempotent for an unchanged token/permission. Thread-safe, nonblocking.
 * Consumers must also reject stale tokens because revocation can race a callback
 * already in progress. Never make network/filesystem calls in these callbacks. */
esp_err_t bot_audio_set_session(bot_audio_t *audio, const bot_audio_token_t *token,
                                bool deliver_pcm);

/** One producer submits one bounded PCM stream at a time. Token must match the
 * open local session. BEGIN is silent until the first queued audio frame.
 * Enqueue APIs are nonblocking; ESP_ERR_TIMEOUT means backpressure, not success.
 * Input is exactly 16 kHz mono signed PCM16LE, <=320 samples per write. The API
 * copies it; output task duplicates the primary channel to stereo for the BSP.
 * No resampling/MP3/Opus decoder is hidden here. */
esp_err_t bot_audio_playback_begin(bot_audio_t *audio, const bot_audio_token_t *token,
                                  uint32_t *playback_id);
esp_err_t bot_audio_playback_write(bot_audio_t *audio, uint32_t playback_id,
                                  const int16_t *samples, size_t sample_count);
esp_err_t bot_audio_playback_end(bot_audio_t *audio, uint32_t playback_id);
/** Capacity-independent cancellation latch. Worker mutes/flushes local output;
 * does not close the shared ADC. playback_id=0 cancels the current stream.
 * Callback-safe; asynchronous completion is PLAYBACK_CANCELLED. */
void bot_audio_playback_cancel(bot_audio_t *audio, uint32_t playback_id);
void bot_audio_get_status(bot_audio_t *audio, bot_audio_status_t *out);

#ifdef __cplusplus
}
#endif

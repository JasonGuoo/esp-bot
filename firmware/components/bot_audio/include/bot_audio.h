/* SPDX-License-Identifier: MIT
 * ESP32-S31-Korvo-1 local audio owner. No runtime, transport, or UI dependency.
 */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "esp_err.h"
#include "bot_voice.h"

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
    BOT_AUDIO_WAKE_BACKEND_NONE = 0,
    BOT_AUDIO_WAKE_BACKEND_WAKENET,
    BOT_AUDIO_WAKE_BACKEND_MULTINET7,
} bot_audio_wake_backend_t;

typedef enum {
    BOT_AUDIO_PLAYBACK_SESSION = 0,
    BOT_AUDIO_PLAYBACK_LOCAL_RESPONSE,
    BOT_AUDIO_PLAYBACK_PREVIEW,
} bot_audio_playback_source_t;

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
    BOT_AUDIO_EVENT_AMBIENT_SOUND,
    BOT_AUDIO_EVENT_LOCAL_COMMAND,
    BOT_AUDIO_EVENT_CAPTURE_READY,
    BOT_AUDIO_EVENT_BUFFER_OVERFLOW,
    BOT_AUDIO_EVENT_CONTROLS_APPLIED,
    BOT_AUDIO_EVENT_PLAYBACK_PROGRESS, /* successful PCM writes only, <=1 event/s */
} bot_audio_event_kind_t;

typedef struct {
    bot_audio_event_kind_t kind;
    int64_t observed_us;          /* local esp_timer monotonic time, NOT capture time */
    bot_audio_token_t token;      /* operation's original token, never a newer session */
    esp_err_t error;
    bool available;
    bot_audio_wake_status_t wake_status;
    bot_audio_wake_backend_t wake_backend;
    bool echo_safe;              /* false for VAD: no validated reference/AEC */
    bool confidence_available;   /* MultiNet native score only; false for WakeNet */
    float confidence;            /* uncalibrated model score, NOT measured accuracy */
    uint32_t playback_id;
    bot_audio_playback_source_t playback_source; /* playback events only */
    int wake_word_index;
    bot_voice_command_t command;  /* LOCAL_COMMAND; runtime alone executes it */
    uint32_t controls_revision;
    bool direction_valid;         /* official DOA quality gate passed; orientation uncalibrated */
    bool speech;                  /* ESP-SR VAD; false may be a non-speech sound */
    float azimuth_deg, direction_quality;
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
     * split-name counting, stock WakeNet model, or VAD wake are allowed.
     * When absent, the enabled MultiNet7 fallback recognizes one complete
     * xiao dai xiao dai command locally; it is not a dedicated WakeNet model. */
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
    bool deliver_pcm;             /* current authenticated transport is ready */
    bool retain_start;            /* explicit post-wake, same-token RAM capture */
    bool waiting_for_followup;    /* stricter VAD onset; no implicit token change */
    bool local_commands;          /* fixed local commands only in an active session */
} bot_audio_session_options_t;

typedef struct {
    bool started, capture_running, afe_running, playback_running;
    bool capture_healthy;            /* last codec read succeeded; copied by get_status */
    bool session_open, pcm_delivery_enabled;
    bool self_playback_blocked;       /* reservation, playback, tail or RX/AFE cleanup still active */
    bool capture_ready;              /* current token's DMA/AFE fence is complete */
    bool retain_start_enabled, local_commands_available, command_consumed;
    bool start_buffer_available;      /* allocation failure degrades onset retention, never local wake */
    bool codec_available, vad_available, wake_available;
    bool hardware_capture_timestamps; /* false */
    bool aec_available;               /* false; no fabricated reference */
    bool automatic_barge_in;          /* false; use explicit local cancellation */
    bool voice_identity_available;    /* false; ESP-SR WakeNet is not voice identity */
    bool night_wake_supported;        /* true while started; audio has no sleep timer */
    bot_audio_wake_status_t wake_status;
    bot_audio_wake_backend_t wake_backend;
    esp_err_t last_error;
    /* Read/result/write completion observations, never measured acoustic times. */
    int64_t last_capture_us, last_afe_result_us, last_playback_us;
    uint64_t capture_frames, afe_frames, delivered_pcm_frames;
    uint64_t gated_pcm_frames, capture_errors, afe_errors, playback_errors;
    uint16_t microphone_peak; /* latest capture block's absolute PCM peak, 0..32768; no PCM retained */
    uint16_t microphone_peak_2;
    uint16_t microphone_mean, microphone_mean_2, ambient_noise_mean;
    bool ambient_activity, ambient_speech, direction_available, direction_valid;
    float azimuth_deg, direction_quality;
    uint64_t ambient_events, direction_estimates;
    uint64_t wake_detections, suppressed_wakes, playback_underruns;
    uint32_t rx_discard_frames;       /* remaining DMA FIFO fence before PCM release */
    uint32_t playback_id;
    bot_audio_playback_source_t playback_source;
    bot_voice_settings_t controls_requested, controls_applied;
    uint32_t controls_revision, controls_applied_revision;
    esp_err_t controls_error;
    uint32_t buffered_samples, buffer_high_water;
    uint64_t buffered_drops, replayed_frames;
    uint64_t local_commands, keyword_runs, keyword_timeouts, keyword_rejections;
    uint64_t afe_feed_rejections, capture_overruns;
    uint32_t read_us, read_max_us, afe_us, afe_max_us, keyword_us, keyword_max_us;
    uint32_t processing_us, processing_max_us, output_queue_high_water;
    uint32_t capture_stack_free, playback_stack_free; /* cached task watermarks; ESP-IDF bytes, 0 = not sampled */
    uint32_t afe_recovery_attempts, afe_recoveries;
    int64_t last_wake_us, last_command_us, last_capture_ready_us;
    int64_t last_playback_begin_us, last_playback_started_us, last_playback_done_us;
    float last_wake_score, last_candidate_score, keyword_dbfs;
    uint16_t keyword_input_peak;
    bool last_wake_score_available;
    int last_candidate_command;
    char model_name[BOT_AUDIO_MODEL_NAME_BYTES + 1];
    char reason[BOT_AUDIO_REASON_BYTES + 1];
} bot_audio_status_t;

bot_audio_config_t bot_audio_default_config(void);
/** One instance per board. Only the application's audio owner calls lifecycle
 * methods; bsp_i2c_init is idempotent and shares the bus with display/camera. */
esp_err_t bot_audio_create(const bot_audio_config_t *config, bot_audio_t **out);
/** Opens one ES8389 IN_OUT device; starts a capture/AFE worker and a playback worker.
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
 * deliver_pcm is a SECOND permission for a currently authenticated media
 * session using the project's negotiated transport; keep it false for
 * local-only/unauthenticated sessions.
 * No wake/VAD event opens this gate automatically. New tokens discard the entire
 * possible RX DMA backlog before emitting raw PCM; no vad_cache is released.
 * Same-token transport readiness changes preserve the capture fence. This
 * compatibility API enables neither RAM retention nor local commands.
 * Idempotent for an unchanged token/permission. Thread-safe, nonblocking.
 * Playback reservation, playback itself and its tail suppress PCM delivery even
 * when deliver_pcm=true. Session changes cancel SESSION playback only; runtime
 * must explicitly cancel a LOCAL_RESPONSE when its own authorization expires.
 * Consumers must also reject stale tokens because revocation can race a callback
 * already in progress. Never make network/filesystem calls in these callbacks. */
esp_err_t bot_audio_set_session(bot_audio_t *audio, const bot_audio_token_t *token,
                                bool deliver_pcm);

/** Explicit capture lease. retain_start permits a bounded RAM-only prefix AFTER
 * the new token's RX fence; it never captures passive/pre-wake audio. Changing
 * only transport readiness or waiting mode preserves the SAME token's prefix.
 * A different token, permission revocation, playback BEGIN, local command or
 * stop clears it. Old PCM is never relabelled. Consumers must still validate the
 * original token. NULL token/options closes all capture/command permissions.
 * A local command latches PCM closed for that token until runtime replaces it.
 * Calls are nonblocking and idempotent; runtime is the sole permission owner. */
esp_err_t bot_audio_set_session_ex(bot_audio_t *audio, const bot_audio_token_t *token,
                                   const bot_audio_session_options_t *options);

/** Queue settings for the capture owner, including before start. Does no
 * codec/model I/O on the caller. Identical requests remain idempotent on error.
 * Volume is 0..60; mute preserves its value and gates PA, never ES8389 ADC mute.
 * Only volume/mute/wake threshold are applied here; night gain and Mac TTS
 * preference are retained for the composing runtime. Status/events acknowledge
 * the revision actually applied. Persistence belongs to the platform owner. */
esp_err_t bot_audio_set_controls(bot_audio_t *audio, const bot_voice_settings_t *settings);
/** Queue a bounded AFE reset retry using the existing model/codec. No reboot,
 * allocation or microphone ownership transfer; an AFE never created cannot retry. */
esp_err_t bot_audio_retry_afe(bot_audio_t *audio);

/** Keep VAD local while an active conversation waits for audible follow-up speech. */
esp_err_t bot_audio_wait_for_followup(bot_audio_t *audio, const bot_audio_token_t *token);

/** One producer submits one bounded PCM stream at a time. Token must match the
 * open local session. BEGIN is silent until the first queued audio frame.
 * Enqueue APIs are nonblocking; ESP_ERR_TIMEOUT means backpressure, not success.
 * Input is exactly 16 kHz mono signed PCM16LE, <=320 samples per write. The API
 * copies it; output task duplicates the primary channel to stereo for the BSP.
 * No resampling/MP3/Opus decoder is hidden here. */
esp_err_t bot_audio_playback_begin(bot_audio_t *audio, const bot_audio_token_t *token,
                                  uint32_t *playback_id);
/** Runtime-authorized local reply using the same sole codec/queue. Does not open
 * a microphone session or authorize PCM. Nonzero epoch/session_id identify the
 * runtime operation; they need not match the network audio session. set_session
 * cannot cancel or replace it. Runtime MUST cancel it when that operation is
 * revoked. All completion events retain this token and LOCAL_RESPONSE source.
 * Only one playback may be reserved; a competing begin returns INVALID_STATE.
 * BEGIN immediately blocks wake/VAD/uplink through the eventual playback tail. */
esp_err_t bot_audio_playback_begin_local(bot_audio_t *audio, const bot_audio_token_t *runtime_token,
                                        uint32_t *playback_id);
/** Explicit settings-page preview lease. Event token is {preview_epoch,0,0,0}.
 * Never opens mic/session permissions. Runtime/UI must cancel when settings
 * close or preview authority expires; source is BOT_AUDIO_PLAYBACK_PREVIEW. */
esp_err_t bot_audio_playback_begin_preview(bot_audio_t *audio, uint32_t preview_epoch,
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

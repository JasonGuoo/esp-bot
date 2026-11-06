/** Official esp_video + ESP-DL adapter for the fixed ESP32-S31 Korvo board.
 * One worker owns the V4L2 stream, frame copies, HumanFaceDetect and vatt tracker.
 * No frame pointers escape this component; face tracks are ephemeral, not identity.
 */
#pragma once

#include <stdbool.h>
#include <stdint.h>
#include "esp_err.h"
#include "vision_attention.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    BOT_VISION_STOPPED = 0,
    BOT_VISION_PAUSED,
    BOT_VISION_STARTING,
    BOT_VISION_CAPTURING,
    BOT_VISION_DEGRADED,
} bot_vision_state_t;

typedef enum {
    BOT_VISION_FRAME_NONE = 0,
    BOT_VISION_FRAME_OK,
    BOT_VISION_FRAME_DARK,
    BOT_VISION_FRAME_INVALID,
    BOT_VISION_FRAME_STALE,
    BOT_VISION_FRAME_TIMEOUT,
    BOT_VISION_FRAME_CAMERA_ERROR,
} bot_vision_frame_quality_t;

typedef enum {
    BOT_VISION_TIME_NONE = 0,
    BOT_VISION_TIME_DRIVER_MONOTONIC,
    /** esp_video 2.4.1 DVP does not fill v4l2_buffer.timestamp. This is the
     * monotonic dequeue time after draining queued frames, before inference;
     * it is NOT a measured sensor exposure time. */
    BOT_VISION_TIME_DEQUEUE_ESTIMATE,
} bot_vision_time_source_t;

typedef struct {
    bot_vision_state_t state;
    bot_vision_frame_quality_t quality;
    esp_err_t error;
    bool enabled_requested;
    bool stream_active;
    bool observation_valid;
    bool calibration_valid;       /* explicit board orientation calibration, not gaze */
    bool capture_timestamp_exact; /* false for current esp_video DVP */
    bot_vision_time_source_t time_source;
    uint32_t observed_ms;         /* status observation, not a captured frame */
    uint32_t capture_ms;          /* low 32 bits of local monotonic ms */
    uint32_t published_ms;
    uint32_t inference_ms;
    uint32_t capture_uncertainty_ms; /* configured age budget, not a measured bound */
    uint32_t frames_seen;
    uint32_t frames_published;
    uint32_t dropped_queued;
    uint32_t dropped_invalid;
    uint32_t dropped_dark;
    uint32_t dropped_stale;
    uint16_t width;
    uint16_t height;
    uint8_t mean_luma;            /* coarse image darkness gate, never identity */
} bot_vision_status_t;

typedef struct {
    /** Called by vision's worker before first open. Platform must cache the
     * first result, including failure, across component restarts: attempt
     * bsp_camera_start(NULL) at most ONCE per boot. Current BSP does not unwind
     * its XCLK handle after esp_video_init failure. Platform owns shared I2C /
     * camera XCLK setup. A failed prepare therefore requires a device reboot.
     * Do not initialize esp32-camera, another SCCB bus, or a second video owner. */
    esp_err_t (*prepare_camera)(void *context);
    /** Called on worker; MUST copy before returning and MUST NOT block.
     * Valid snapshots retain pre-inference capture_ms (including documented
     * dequeue estimates). Unavailable snapshots are fresh status observations.
     * vatt primary_track_id is legacy: attention_manager selects the focus. */
    void (*on_snapshot)(void *context, const vatt_snapshot_t *snapshot);
    /** Status callback precedes a corresponding snapshot callback. */
    void (*on_status)(void *context, const bot_vision_status_t *status);
    void *context;
    const char *device_path;      /* copied at start; default /dev/video2 */
    bool start_enabled;
    bool rotate_180;              /* true for stock OV3660 installation */
    bool calibration_valid;       /* false until actual board orientation checked */
    uint16_t max_frame_width;
    uint16_t max_frame_height;
    uint16_t dequeue_timeout_ms;
    uint16_t frame_interval_ms;    /* target max inference rate; not sensor FPS */
    uint16_t max_result_age_ms;
    uint16_t capture_uncertainty_ms; /* configured estimate budget; default 200 ms */
    uint16_t retry_ms;
    uint8_t min_mean_luma;
    uint8_t warmup_frames;
    float min_face_confidence;
    vatt_config_t tracker;
} bot_vision_config_t;

typedef struct bot_vision bot_vision_t;
bot_vision_config_t bot_vision_default_config(void);
/** Asynchronously starts the owner task. Camera/model errors are on_status,
 * not false success claims from this return value. Exactly one instance. */
esp_err_t bot_vision_start(const bot_vision_config_t *config, bot_vision_t **out);
/** Latest requested state wins. Pause STREAMOFFs capture and stops inference.
 * BSP XCLK remains initialized; this does not claim camera rail power-off.
 * Pause takes effect after any already-running synchronous model invocation. */
esp_err_t bot_vision_set_enabled(bot_vision_t *vision, bool enabled);
/** Request shutdown and wait up to timeout_ms. On timeout, keep the handle and
 * retry stop later; never free resources while the worker is using them.
 * ESP_OK destroys the handle. Serialize stop with all other lifecycle/status
 * calls; concurrent stop callers are unsupported. Must not run in a callback. */
esp_err_t bot_vision_stop(bot_vision_t *vision, uint32_t timeout_ms);
/** Thread-safe copy; no pointer into driver/model storage. */
void bot_vision_get_status(bot_vision_t *vision, bot_vision_status_t *out);
const char *bot_vision_state_name(bot_vision_state_t state);
const char *bot_vision_quality_name(bot_vision_frame_quality_t quality);

#ifdef __cplusplus
}
#endif

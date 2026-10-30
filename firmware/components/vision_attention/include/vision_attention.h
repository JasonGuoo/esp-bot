/** ESP Bot camera-only multi-person visual-attention core.
 * Independent of ESP-DL, camera driver, LVGL, servo, network and Brain.
 * All image positions normalized 0..1, upright and unmirrored.
 * Coordinate origin at image top-left, x right and y down.
 * Capture times are monotonic milliseconds.
 */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#ifdef __cplusplus
extern "C" {
#endif

#define VATT_MAX_FACES 4
#define VATT_MAX_EVENTS 16

typedef enum { VATT_NO_FACE=0, VATT_FACE_PRESENT, VATT_LOOKING_AT_BOT,
               VATT_UNAVAILABLE } vatt_state_t;
typedef enum { VATT_GAZE_UNKNOWN=0, VATT_GAZE_AWAY, VATT_GAZE_AT_BOT } vatt_gaze_t;
typedef enum { VATT_FACE_ENTER=0, VATT_FACE_EXIT, VATT_LOOK_BEGIN, VATT_LOOK_END,
               VATT_PRIMARY_CHANGED, VATT_CAMERA_LOST, VATT_CAMERA_RECOVERED } vatt_event_kind_t;
typedef struct {float x,y;} vatt_point_t;
typedef struct {float left,top,right,bottom;} vatt_box_t;

/* One face from a frame. Calibrated gaze evidence is provided by an
 * independent gaze estimator; detecting a face, or finding a dark pupil,
 * cannot set eye_contact_valid by itself. Angles are camera-aligned estimates.
 */
typedef struct {
    vatt_box_t bbox;
    vatt_point_t left_eye,right_eye,left_pupil,right_pupil;
    bool left_pupil_valid,right_pupil_valid;
    float left_pupil_quality,right_pupil_quality;
    float face_confidence;
    float head_yaw_deg,head_pitch_deg;
    bool head_pose_valid;
    float gaze_yaw_deg,gaze_pitch_deg;
    bool gaze_direction_valid;
    float gaze_quality;          /* 0..1 */
    float eye_contact_score;    /* 0..1, gaze ray intersects Bot face/screen */
    bool eye_contact_valid;     /* calibrated estimator, never frontal face */
} vatt_observation_t;

typedef struct {
    uint32_t track_id;            /* ephemeral tracking, NOT identity recognition */
    bool visible;
    bool association_ambiguous;
    uint32_t first_seen_ms,last_seen_ms,stale_ms;
    vatt_point_t eye_midpoint;          /* target for Bot's physical eyes/head */
    vatt_point_t eye_velocity_uv_s;    /* normalized image coordinates / second */
    vatt_state_t status;
    vatt_gaze_t gaze_evidence;
    uint32_t continuous_look_ms;
    uint64_t cumulative_look_ms;
    uint32_t look_count;
    vatt_observation_t observation;
} vatt_face_t;

typedef struct {
    vatt_event_kind_t kind;
    uint32_t track_id;          /* 0 for camera events */
    uint32_t timestamp_ms;
    uint32_t duration_ms;       /* LOOK_END only */
} vatt_event_t;

typedef struct {
    uint32_t timestamp_ms;
    vatt_state_t state;
    uint32_t primary_track_id;  /* 0 when none */
    uint8_t face_count;
    vatt_face_t faces[VATT_MAX_FACES];
    uint8_t event_count;
    vatt_event_t events[VATT_MAX_EVENTS];
} vatt_snapshot_t;

typedef struct {
    uint32_t look_enter_ms,look_exit_ms,unknown_grace_ms;
    uint32_t no_face_ms,track_expire_ms;
    float positive_score,negative_score,min_gaze_quality,min_face_confidence;
} vatt_config_t;

typedef struct vatt_tracker vatt_tracker_t;
vatt_config_t vatt_default_config(void);
vatt_tracker_t *vatt_tracker_create(const vatt_config_t *config);
void vatt_tracker_destroy(vatt_tracker_t *tracker);

/** Ingest one frame; count may be zero (no faces in camera). If count exceeds
 * MAX_FACES only the first MAX_FACES are considered, so order by priority.
 * camera_ok=false makes the global status UNAVAILABLE, not NO_FACE.
 * Thread-safety: one owner task; copy snapshot to other threads.
 * On error or out-of-order capture timestamp returns false.
 */
bool vatt_tracker_update(vatt_tracker_t *tracker,
                         const vatt_observation_t *observations,
                         size_t count,uint32_t capture_ms,bool camera_ok,
                         vatt_snapshot_t *out);

/* Full-resolution GREY8 image view borrowed from capture pipeline. */
typedef struct {
    const uint8_t *data;
    uint16_t width,height,stride;
} vatt_gray8_t;

typedef struct {
    bool valid;
    vatt_point_t center_px;
    float eye_relative_u,eye_relative_v; /* near [-1..1], NOT gaze degrees */
    float quality;                      /* 0..1 */
} vatt_pupil_t;

/** Approximate dark iris candidate near one supplied ESP-DL eye landmark.
 * Requires enough pixels and contrast. This does not infer actual gaze, and
 * glasses/eyelashes/reflections may produce false pupil candidates.
 */
bool vatt_pupil_candidate(const vatt_gray8_t *image,
                          vatt_point_t eye_center_px,
                          vatt_point_t other_eye_center_px,
                          vatt_pupil_t *out);
#ifdef __cplusplus
}
#endif

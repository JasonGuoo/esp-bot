/*
 * SPDX-FileCopyrightText: 2026 esp-bot contributors
 * SPDX-License-Identifier: MIT
 *
 * Capture sequence adapted from Espressif's CC0 display_camera_video example;
 * model invocation follows esp-dl/examples/human_face_detect. See README.md
 * for exact upstream commits, API versions, ownership and timestamp limits.
 */
#include "bot_vision.h"

#include <algorithm>
#include <atomic>
#include <cerrno>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <initializer_list>
#include <new>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/time.h>
#include <unistd.h>

#include "sdkconfig.h"
#include "esp_heap_caps.h"
#include "esp_log.h"
#include "esp_timer.h"
#include "esp_video_device.h"
#include "esp_video_ioctl.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"
#include "freertos/task.h"
#include "human_face_detect.hpp"
#include "dl_image_define.hpp"

#if !CONFIG_IDF_TARGET_ESP32S31
#error "bot_vision targets the ESP32-S31 Korvo DVP path; port a different board explicitly"
#endif
#if !CONFIG_FLASH_HUMAN_FACE_DETECT_MSRMNP_S8_V1
#error "bot_vision needs FLASH_HUMAN_FACE_DETECT_MSRMNP_S8_V1 for five face landmarks"
#endif
#if !CONFIG_HUMAN_FACE_DETECT_MODEL_IN_FLASH_RODATA
#error "bot_vision currently uses the official human_face_detect flash-rodata model"
#endif

static const char *TAG = "bot_vision";
static constexpr unsigned BUFFER_COUNT = 3;
static constexpr size_t DEVICE_PATH_CAP = 48;
static std::atomic_flag s_claimed = ATOMIC_FLAG_INIT;

struct bot_vision {
    bot_vision_config_t config{};
    char device_path[DEVICE_PATH_CAP]{};
    std::atomic<bool> enabled{false};
    std::atomic<bool> stopping{false};
    SemaphoreHandle_t status_lock = nullptr;
    SemaphoreHandle_t done = nullptr;
    TaskHandle_t task = nullptr;
    bot_vision_status_t status{};        // published, protected by status_lock
    bot_vision_status_t working{};       // worker only
    vatt_tracker_t *tracker = nullptr;  // worker only after start
    HumanFaceDetect *detector = nullptr;
    bool camera_prepare_attempted = false;
    esp_err_t camera_prepare_error = ESP_OK;
    bool streaming = false;
    int fd = -1;
    unsigned buffer_count = 0;
    uint8_t *buffers[BUFFER_COUNT]{};    // driver-owned MMAP views
    size_t lengths[BUFFER_COUNT]{};
    uint8_t *image = nullptr;           // sole owned, packed PSRAM copy
    size_t image_size = 0;
    size_t stride = 0;
    size_t required_bytes = 0;
    uint16_t width = 0;
    uint16_t height = 0;
    uint8_t pixel_bytes = 0;
    uint32_t fourcc = 0;
    dl::image::pix_type_t image_type = dl::image::DL_IMAGE_PIX_TYPE_RGB888;
    unsigned warmup_remaining = 0;
    uint32_t last_snapshot_ms = 0;
    bool have_snapshot = false;
};

static uint32_t now_ms()
{
    return static_cast<uint32_t>(esp_timer_get_time() / 1000);
}

static TickType_t ticks_for(uint32_t ms)
{
    if (!ms) return 0;
    const TickType_t ticks = pdMS_TO_TICKS(ms);
    return ticks ? ticks : 1;
}

static void wait_control(bot_vision_t *v, uint32_t ms)
{
    if (!v->stopping.load()) ulTaskNotifyTake(pdTRUE, ticks_for(ms));
    else vTaskDelay(ticks_for(std::min<uint32_t>(ms, 100)));
}

static bool wants_capture(const bot_vision_t *v)
{
    return v->enabled.load() && !v->stopping.load();
}

static esp_err_t io_error(const char *operation)
{
    const int saved_errno = errno;
    ESP_LOGW(TAG, "%s: errno=%d (%s)", operation, saved_errno, strerror(saved_errno));
    return saved_errno == ETIMEDOUT || saved_errno == EAGAIN ? ESP_ERR_TIMEOUT : ESP_FAIL;
}

static void publish_status(bot_vision_t *v)
{
    v->working.enabled_requested = v->enabled.load() && !v->stopping.load();
    v->working.stream_active = v->streaming;
    v->working.calibration_valid = v->config.calibration_valid;
    v->working.observed_ms = now_ms();
    v->working.published_ms = v->working.observed_ms;
    xSemaphoreTake(v->status_lock, portMAX_DELAY);
    v->status = v->working;
    xSemaphoreGive(v->status_lock);
    if (v->config.on_status) v->config.on_status(v->config.context, &v->working);
}

static void publish_observations(bot_vision_t *v, const vatt_observation_t *observations,
                                 size_t count, uint32_t stamp, bool usable)
{
    vatt_snapshot_t snapshot{};
    // Equal/older observations are never re-stamped just to satisfy the bus.
    // Signed deltas support the low-32-bit monotonic clock wrapping at 49.7 days.
    if (v->have_snapshot && static_cast<int32_t>(stamp - v->last_snapshot_ms) <= 0) {
        publish_status(v);
        return;
    }
    if (!v->tracker && !usable && count == 0) {
        // Pause retires the tracker. Availability-only reports (including a
        // later stop or failed tracker allocation) need no retained tracks.
        snapshot.timestamp_ms = stamp;
        snapshot.state = VATT_UNAVAILABLE;
    } else if (!vatt_tracker_update(v->tracker, observations, count, stamp, usable, &snapshot)) {
        v->working.observation_valid = false;
        v->working.error = ESP_ERR_INVALID_STATE;
        publish_status(v);
        return;
    }
    v->last_snapshot_ms = stamp;
    v->have_snapshot = true;
    if (usable) ++v->working.frames_published;
    publish_status(v); // health/calibration metadata arrives before its snapshot
    if (v->config.on_snapshot) v->config.on_snapshot(v->config.context, &snapshot);
}

static void unavailable(bot_vision_t *v, bot_vision_state_t state,
                        bot_vision_frame_quality_t quality, esp_err_t error)
{
    v->working.state = state;
    v->working.quality = quality;
    v->working.error = error;
    v->working.observation_valid = false;
    // This is an availability observation, NOT an old frame with a fresh stamp.
    publish_observations(v, nullptr, 0, now_ms(), false);
}

static void retire_tracking(bot_vision_t *v)
{
    // A stopped stream has no continuity lease. Do not carry a 32-bit capture
    // ordering anchor across an arbitrarily long pause, or reuse its tracks.
    vatt_tracker_destroy(v->tracker);
    v->tracker = nullptr;
    v->have_snapshot = false;
    v->last_snapshot_ms = 0;
}

static esp_err_t set_dequeue_timeout(bot_vision_t *v, uint16_t ms)
{
    struct timeval timeout{};
    timeout.tv_sec = ms / 1000;
    timeout.tv_usec = (ms % 1000) * 1000;
    if (ioctl(v->fd, VIDIOC_S_DQBUF_TIMEOUT, &timeout) != 0) return io_error("DQBUF timeout");
    return ESP_OK;
}

static esp_err_t requeue(bot_vision_t *v, struct v4l2_buffer *buffer)
{
    // The buffer index comes from the driver and must not address arbitrary memory.
    if (buffer->index >= v->buffer_count) return ESP_ERR_INVALID_RESPONSE;
    buffer->m.userptr = reinterpret_cast<unsigned long>(v->buffers[buffer->index]);
    buffer->length = v->lengths[buffer->index];
    if (ioctl(v->fd, VIDIOC_QBUF, buffer) != 0) return io_error("QBUF");
    return ESP_OK;
}

static esp_err_t dequeue(bot_vision_t *v, struct v4l2_buffer *buffer)
{
    // esp_video 2.4.1 fills index/bytesused/flags, but NOT timestamp or sequence.
    // Always clear it: reusing an old timestamp would fabricate freshness.
    memset(buffer, 0, sizeof(*buffer));
    buffer->type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    buffer->memory = V4L2_MEMORY_MMAP;
    if (ioctl(v->fd, VIDIOC_DQBUF, buffer) != 0) {
        if (errno == ETIMEDOUT || errno == EAGAIN) return ESP_ERR_TIMEOUT;
        return io_error("DQBUF");
    }
    ++v->working.frames_seen;
    return ESP_OK;
}

static esp_err_t close_stream(bot_vision_t *v)
{
    if (v->fd >= 0 && v->streaming) {
        int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
        if (ioctl(v->fd, VIDIOC_STREAMOFF, &type) != 0) {
            // Keep the descriptor and DMA views alive if stop failed. Do not
            // free buffers under a driver which may still be writing to them.
            return io_error("STREAMOFF");
        }
        v->streaming = false;
    }
    if (v->fd >= 0) {
        if (close(v->fd) != 0) return io_error("close video");
        v->fd = -1;
    }
    // esp_video MMAP returns driver-owned memory; closing the last descriptor
    // releases it. Neither free() nor heap_caps_free() owns these pointers.
    v->buffer_count = 0;
    memset(v->buffers, 0, sizeof(v->buffers));
    memset(v->lengths, 0, sizeof(v->lengths));
    heap_caps_free(v->image);
    v->image = nullptr;
    v->image_size = 0;
    return ESP_OK;
}

static esp_err_t set_unmirrored_sensor(bot_vision_t *v)
{
    // BSP_CAMERA_VFLIP=0 / BSP_CAMERA_HFLIP=0 on S31-Korvo-1.
    // The board's 180-degree installation is corrected in our packed copy.
    struct v4l2_ext_control control{};
    struct v4l2_ext_controls controls{};
    controls.ctrl_class = V4L2_CID_USER_CLASS;
    controls.count = 1;
    controls.controls = &control;
    control.value = 0;
    control.id = V4L2_CID_VFLIP;
    if (ioctl(v->fd, VIDIOC_S_EXT_CTRLS, &controls) != 0) return io_error("sensor VFLIP");
    control.id = V4L2_CID_HFLIP;
    if (ioctl(v->fd, VIDIOC_S_EXT_CTRLS, &controls) != 0) return io_error("sensor HFLIP");
    return ESP_OK;
}

static esp_err_t open_stream(bot_vision_t *v)
{
    if (v->fd >= 0) return ESP_ERR_INVALID_STATE;
    v->fd = open(v->device_path, O_RDONLY);
    if (v->fd < 0) return io_error("open video");

    struct v4l2_capability capability{};
    if (ioctl(v->fd, VIDIOC_QUERYCAP, &capability) != 0) return io_error("QUERYCAP");
    struct v4l2_format format{};
    format.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    if (ioctl(v->fd, VIDIOC_G_FMT, &format) != 0) return io_error("G_FMT");

    if (format.fmt.pix.pixelformat == V4L2_PIX_FMT_RGB565X) {
        // Exact S31 cold-power-on workaround from the official BSP example:
        // reapply OV3660's selected sensor format after its settling delay.
        vTaskDelay(ticks_for(200));
        if (!wants_capture(v)) return ESP_ERR_INVALID_STATE;
        esp_cam_sensor_format_t sensor_format{};
        if (ioctl(v->fd, VIDIOC_G_SENSOR_FMT, &sensor_format) != 0 ||
            ioctl(v->fd, VIDIOC_S_SENSOR_FMT, &sensor_format) != 0) {
            return io_error("reapply OV3660 sensor format");
        }
    }
    esp_err_t result = set_unmirrored_sensor(v);
    if (result != ESP_OK) return result;
    if (ioctl(v->fd, VIDIOC_G_FMT, &format) != 0) return io_error("final G_FMT");
    const auto &pixel = format.fmt.pix;
    if (pixel.width < 2 || pixel.height < 2 || pixel.width > v->config.max_frame_width ||
        pixel.height > v->config.max_frame_height) return ESP_ERR_INVALID_SIZE;
    v->width = pixel.width;
    v->height = pixel.height;
    v->fourcc = pixel.pixelformat;
    switch (v->fourcc) {
    case V4L2_PIX_FMT_RGB565X:
        v->pixel_bytes = 2;
        v->image_type = dl::image::DL_IMAGE_PIX_TYPE_RGB565BE;
        break;
    case V4L2_PIX_FMT_RGB565:
        v->pixel_bytes = 2;
        v->image_type = dl::image::DL_IMAGE_PIX_TYPE_RGB565LE;
        break;
    case V4L2_PIX_FMT_RGB24:
        v->pixel_bytes = 3;
        v->image_type = dl::image::DL_IMAGE_PIX_TYPE_RGB888;
        break;
    default:
        ESP_LOGE(TAG, "Unsupported capture pixel format; select the official OV3660 RGB565 preset");
        return ESP_ERR_NOT_SUPPORTED;
    }
    const size_t row_bytes = static_cast<size_t>(v->width) * v->pixel_bytes;
    v->stride = pixel.bytesperline ? pixel.bytesperline : row_bytes;
    if (v->stride < row_bytes || v->stride > row_bytes + 4096) return ESP_ERR_INVALID_SIZE;
    v->required_bytes = (v->height - 1) * v->stride + row_bytes;
    if (pixel.sizeimage && pixel.sizeimage < v->required_bytes) return ESP_ERR_INVALID_SIZE;
    v->image_size = row_bytes * v->height;
    v->image = static_cast<uint8_t *>(heap_caps_malloc(v->image_size, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT));
    if (!v->image) return ESP_ERR_NO_MEM;

    struct v4l2_requestbuffers request{};
    request.count = BUFFER_COUNT;
    request.type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    request.memory = V4L2_MEMORY_MMAP;
    if (ioctl(v->fd, VIDIOC_REQBUFS, &request) != 0) return io_error("REQBUFS");
    if (request.count < 2 || request.count > BUFFER_COUNT) return ESP_ERR_INVALID_RESPONSE;
    v->buffer_count = request.count;
    for (unsigned i = 0; i < v->buffer_count; ++i) {
        struct v4l2_buffer buffer{};
        buffer.type = request.type;
        buffer.memory = request.memory;
        buffer.index = i;
        if (ioctl(v->fd, VIDIOC_QUERYBUF, &buffer) != 0) return io_error("QUERYBUF");
        if (buffer.length < v->required_bytes) return ESP_ERR_INVALID_SIZE;
        void *mapping = mmap(nullptr, buffer.length, PROT_READ | PROT_WRITE,
                             MAP_SHARED, v->fd, buffer.m.offset);
        if (!mapping || mapping == MAP_FAILED) return io_error("mmap video");
        v->buffers[i] = static_cast<uint8_t *>(mapping);
        v->lengths[i] = buffer.length;
        result = requeue(v, &buffer);
        if (result != ESP_OK) return result;
    }
    result = set_dequeue_timeout(v, v->config.dequeue_timeout_ms);
    if (result != ESP_OK) return result;
    if (!wants_capture(v)) return ESP_ERR_INVALID_STATE;
    int type = V4L2_BUF_TYPE_VIDEO_CAPTURE;
    if (ioctl(v->fd, VIDIOC_STREAMON, &type) != 0) return io_error("STREAMON");
    v->streaming = true;
    v->warmup_remaining = v->config.warmup_frames;
    v->working.width = v->width;
    v->working.height = v->height;
    ESP_LOGI(TAG, "DVP %ux%u, %u-byte pixels, 180deg=%d, calibration=%d",
             v->width, v->height, v->pixel_bytes, v->config.rotate_180, v->config.calibration_valid);
    return ESP_OK;
}

static esp_err_t drain_completed_frames(bot_vision_t *v)
{
    esp_err_t result = set_dequeue_timeout(v, 0);
    if (result != ESP_OK) return result;
    // esp_video's O_NONBLOCK flag is not implemented; use its documented timeout
    // ioctl. Bounded draining prevents a high-rate producer from starving control.
    bool drained = false;
    for (unsigned i = 0; i < v->buffer_count * 2 && wants_capture(v); ++i) {
        struct v4l2_buffer buffer{};
        result = dequeue(v, &buffer);
        if (result == ESP_ERR_TIMEOUT) {
            drained = true;
            result = ESP_OK;
            break;
        }
        if (result != ESP_OK) break;
        ++v->working.dropped_queued;
        result = requeue(v, &buffer);
        if (result != ESP_OK) break;
    }
    const esp_err_t restored = set_dequeue_timeout(v, v->config.dequeue_timeout_ms);
    if (result != ESP_OK) return result;
    if (restored != ESP_OK) return restored;
    if (!wants_capture(v)) return ESP_ERR_INVALID_STATE;
    // Do not run inference against an unbounded backlog with a fresh dequeue stamp.
    return drained ? ESP_OK : ESP_ERR_TIMEOUT;
}

static uint8_t pixel_luma(const uint8_t *pixel, uint32_t fourcc)
{
    unsigned r, g, b;
    if (fourcc == V4L2_PIX_FMT_RGB24) {
        r = pixel[0]; g = pixel[1]; b = pixel[2];
    } else {
        const unsigned packed = fourcc == V4L2_PIX_FMT_RGB565X ?
            (static_cast<unsigned>(pixel[0]) << 8) | pixel[1] :
            (static_cast<unsigned>(pixel[1]) << 8) | pixel[0];
        r = ((packed >> 11) & 31) * 255 / 31;
        g = ((packed >> 5) & 63) * 255 / 63;
        b = (packed & 31) * 255 / 31;
    }
    return static_cast<uint8_t>((77 * r + 150 * g + 29 * b) >> 8);
}

static bool copy_frame(bot_vision_t *v, const struct v4l2_buffer &buffer)
{
    if (buffer.index >= v->buffer_count || (buffer.flags & V4L2_BUF_FLAG_ERROR) ||
        !v->buffers[buffer.index] || buffer.bytesused < v->required_bytes ||
        buffer.bytesused > v->lengths[buffer.index]) return false;
    const uint8_t *source = v->buffers[buffer.index];
    uint64_t luma = 0;
    unsigned samples = 0;
    for (size_t y = 0; y < v->height; ++y) {
        const uint8_t *row = source + y * v->stride;
        if (!v->config.rotate_180) {
            memcpy(v->image + y * v->width * v->pixel_bytes, row, v->width * v->pixel_bytes);
        }
        for (size_t x = 0; x < v->width; ++x) {
            const uint8_t *pixel = row + x * v->pixel_bytes;
            if (v->config.rotate_180) {
                const size_t index = (v->height - 1 - y) * v->width + v->width - 1 - x;
                memcpy(v->image + index * v->pixel_bytes, pixel, v->pixel_bytes);
            }
            if ((x % 8) == 0 && (y % 8) == 0) {
                luma += pixel_luma(pixel, v->fourcc);
                ++samples;
            }
        }
    }
    v->working.mean_luma = samples ? luma / samples : 0;
    return true;
}

static bool face_observation(const dl::detect::result_t &face, const bot_vision_t *v,
                             vatt_observation_t *out)
{
    if (face.box.size() < 4 || face.keypoint.size() < 10 ||
        !std::isfinite(face.score) || face.score > 1.0f || face.score < 0.0f) return false;
    const float max_x = v->width - 1;
    const float max_y = v->height - 1;
    if (face.box[0] < 0 || face.box[1] < 0 || face.box[2] > max_x || face.box[3] > max_y ||
        face.box[0] >= face.box[2] || face.box[1] >= face.box[3]) return false;
    // Official MNP order: left eye, left mouth, nose, right eye, right mouth.
    // Indices 2/3 are a mouth corner, not the second eye.
    for (unsigned i : {0U, 6U}) {
        if (face.keypoint[i] < 0 || face.keypoint[i] > max_x ||
            face.keypoint[i + 1] < 0 || face.keypoint[i + 1] > max_y) return false;
    }
    *out = {};
    out->bbox = {face.box[0] / max_x, face.box[1] / max_y,
                 face.box[2] / max_x, face.box[3] / max_y};
    out->left_eye = {face.keypoint[0] / max_x, face.keypoint[1] / max_y};
    out->right_eye = {face.keypoint[6] / max_x, face.keypoint[7] / max_y};
    out->face_confidence = face.score;
    // Zero-initialized pupil/head_pose/gaze_direction/eye_contact validity flags
    // MUST stay false. Five face landmarks do not establish gaze or identity.
    return true;
}

static esp_err_t capture_and_infer(bot_vision_t *v)
{
    esp_err_t result = drain_completed_frames(v);
    if (result != ESP_OK) return result;
    struct v4l2_buffer buffer{};
    result = dequeue(v, &buffer);
    if (result != ESP_OK) return result;
    v->working.capture_ms = now_ms(); // before copy and inference, never at publish
    v->working.capture_timestamp_exact = false;
    v->working.time_source = BOT_VISION_TIME_DEQUEUE_ESTIMATE;
    v->working.capture_uncertainty_ms = v->config.capture_uncertainty_ms;
    v->working.inference_ms = 0;
    v->working.mean_luma = 0;

    const bool valid = copy_frame(v, buffer);
    // Every dequeued frame is returned, including invalid/dark/warmup frames.
    // Inference reads only the packed PSRAM copy; DMA may reuse its original.
    result = requeue(v, &buffer);
    if (result != ESP_OK) return result;
    if (!wants_capture(v)) return ESP_ERR_INVALID_STATE;
    if (!valid) {
        ++v->working.dropped_invalid;
        unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_INVALID, ESP_ERR_INVALID_RESPONSE);
        return ESP_OK;
    }
    if (v->warmup_remaining) {
        --v->warmup_remaining;
        unavailable(v, BOT_VISION_STARTING, BOT_VISION_FRAME_NONE, ESP_OK);
        return ESP_OK;
    }
    if (v->working.mean_luma < v->config.min_mean_luma) {
        ++v->working.dropped_dark;
        unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_DARK, ESP_OK);
        return ESP_OK;
    }
    dl::image::img_t image{};
    image.data = v->image;
    image.width = v->width;
    image.height = v->height;
    image.pix_type = v->image_type;
    const uint32_t infer_started = now_ms();
    const auto &faces = v->detector->run(image);
    v->working.inference_ms = now_ms() - infer_started;
    if (!wants_capture(v)) return ESP_ERR_INVALID_STATE;
    if (now_ms() - v->working.capture_ms + v->working.capture_uncertainty_ms >
        v->config.max_result_age_ms) {
        ++v->working.dropped_stale;
        unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_STALE, ESP_ERR_TIMEOUT);
        return ESP_OK;
    }
    vatt_observation_t observations[VATT_MAX_FACES]{};
    size_t count = 0;
    bool malformed = false;
    for (const auto &face : faces) {
        if (std::isfinite(face.score) && face.score < v->config.min_face_confidence) continue;
        vatt_observation_t observation{};
        if (!face_observation(face, v, &observation)) {
            malformed = true;
            continue;
        }
        size_t position = 0;
        while (position < count && observations[position].face_confidence >= observation.face_confidence) ++position;
        if (position == VATT_MAX_FACES) continue;
        const size_t end = std::min(count, static_cast<size_t>(VATT_MAX_FACES - 1));
        for (size_t i = end; i > position; --i) observations[i] = observations[i - 1];
        observations[position] = observation;
        count = std::min(count + 1, static_cast<size_t>(VATT_MAX_FACES));
    }
    if (!count && malformed) {
        ++v->working.dropped_invalid;
        unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_INVALID, ESP_ERR_INVALID_RESPONSE);
        return ESP_OK;
    }
    v->working.state = BOT_VISION_CAPTURING;
    v->working.quality = BOT_VISION_FRAME_OK;
    v->working.error = ESP_OK;
    v->working.observation_valid = true;
    // Calibration is reported separately. Runtime policy may allow approximate
    // visual following before calibration, but this is never gaze evidence.
    publish_observations(v, observations, count, v->working.capture_ms, true);
    return ESP_OK;
}

static void vision_task(void *arg)
{
    auto *v = static_cast<bot_vision_t *>(arg);
    bool paused_reported = false;
    bool recovering = false;
    unsigned consecutive_timeouts = 0;
    for (;;) {
        if (!wants_capture(v)) {
            const esp_err_t closed = close_stream(v);
            if (closed != ESP_OK) {
                unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, closed);
                wait_control(v, v->config.retry_ms);
                continue;
            }
            recovering = false;
            if (v->stopping.load()) break;
            if (!paused_reported) {
                unavailable(v, BOT_VISION_PAUSED, BOT_VISION_FRAME_NONE, ESP_OK);
                // Only retire after STREAMOFF/close succeeded and the final
                // unavailable observation was published. Resume starts fresh.
                retire_tracking(v);
                paused_reported = true;
            }
            ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
            continue;
        }
        paused_reported = false;
        if (recovering) {
            const esp_err_t closed = close_stream(v);
            if (closed != ESP_OK) {
                unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, closed);
                wait_control(v, v->config.retry_ms);
                continue;
            }
            recovering = false;
        }
        if (v->fd < 0) {
            if (!v->tracker) {
                v->tracker = vatt_tracker_create(&v->config.tracker);
                if (!v->tracker) {
                    unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_NONE, ESP_ERR_NO_MEM);
                    wait_control(v, v->config.retry_ms);
                    continue;
                }
            }
            unavailable(v, BOT_VISION_STARTING, BOT_VISION_FRAME_NONE, ESP_OK);
            if (!v->camera_prepare_attempted) {
                // The BSP does not unwind its local XCLK handle when video init
                // fails. Cache the first result (including failure), not just a
                // successful prepare; the platform must do the same across a
                // component stop/start within this boot.
                v->camera_prepare_attempted = true;
                v->camera_prepare_error = v->config.prepare_camera(v->config.context);
            }
            esp_err_t result = v->camera_prepare_error;
            if (result == ESP_OK && wants_capture(v) && !v->detector) {
                // The explicit MSR+MNP model yields landmarks. ESPDet-only model
                // choices are intentionally not silently accepted here.
                v->detector = new (std::nothrow) HumanFaceDetect(HumanFaceDetect::MSRMNP_S8_V1, false);
                if (!v->detector) result = ESP_ERR_NO_MEM;
                else {
                    v->detector->set_score_thr(v->config.min_face_confidence, 0);
                    v->detector->set_score_thr(v->config.min_face_confidence, 1);
                }
            }
            if (result == ESP_OK && wants_capture(v)) result = open_stream(v);
            if (result != ESP_OK || !wants_capture(v)) {
                const esp_err_t closed = close_stream(v);
                recovering = closed != ESP_OK;
                if (closed != ESP_OK) result = closed;
                if (wants_capture(v)) {
                    unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, result);
                    wait_control(v, v->config.retry_ms);
                }
                continue;
            }
            consecutive_timeouts = 0;
        }
        const uint32_t frame_started = now_ms();
        const esp_err_t result = capture_and_infer(v);
        if (!wants_capture(v)) continue;
        if (result == ESP_ERR_TIMEOUT) {
            ++consecutive_timeouts;
            unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_TIMEOUT, result);
            // A finite recovery attempt reopens video; it never reinitializes
            // the shared BSP bus or allocates another XCLK after any prepare attempt.
            if (consecutive_timeouts >= 3) {
                const esp_err_t closed = close_stream(v);
                recovering = closed != ESP_OK;
                if (closed != ESP_OK) unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, closed);
                wait_control(v, v->config.retry_ms);
            }
        } else if (result != ESP_OK) {
            unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, result);
            const esp_err_t closed = close_stream(v);
            recovering = closed != ESP_OK;
            if (closed != ESP_OK) unavailable(v, BOT_VISION_DEGRADED, BOT_VISION_FRAME_CAMERA_ERROR, closed);
            wait_control(v, v->config.retry_ms);
        } else {
            consecutive_timeouts = 0;
        }
        const uint32_t elapsed = now_ms() - frame_started;
        if (elapsed < v->config.frame_interval_ms) wait_control(v, v->config.frame_interval_ms - elapsed);
    }
    delete v->detector;
    v->detector = nullptr;
    unavailable(v, BOT_VISION_STOPPED, BOT_VISION_FRAME_NONE, ESP_OK);
    // Do not touch v after giving done. Keep the task handle valid until stop()
    // reaps us: a timed-out stop may be retried after cleanup has completed.
    // Self-deleting here would leave that retry notifying a freed task handle.
    SemaphoreHandle_t done = v->done;
    xSemaphoreGive(done);
    for (;;) ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
}

extern "C" bot_vision_config_t bot_vision_default_config(void)
{
    bot_vision_config_t config{};
    config.device_path = ESP_VIDEO_DVP_DEVICE_NAME;
    config.start_enabled = false;
    config.rotate_180 = true;
    config.calibration_valid = false;
    config.max_frame_width = 640;
    config.max_frame_height = 480;
    config.dequeue_timeout_ms = 250;
    config.frame_interval_ms = 200;
    config.max_result_age_ms = 750;
    config.capture_uncertainty_ms = 200;
    config.retry_ms = 2000;
    config.min_mean_luma = 12;
    config.warmup_frames = 2;
    config.min_face_confidence = 0.5f;
    config.tracker = vatt_default_config();
    config.tracker.min_face_confidence = config.min_face_confidence;
    return config;
}

extern "C" esp_err_t bot_vision_start(const bot_vision_config_t *config, bot_vision_t **out)
{
    if (!config || !out) return ESP_ERR_INVALID_ARG;
    *out = nullptr;
    if (!config->prepare_camera || !config->on_snapshot ||
        !config->dequeue_timeout_ms || !config->frame_interval_ms || !config->retry_ms ||
        config->max_frame_width < 2 || config->max_frame_height < 2 ||
        config->max_frame_width > 1920 || config->max_frame_height > 1080 ||
        config->max_result_age_ms <= config->capture_uncertainty_ms ||
        !std::isfinite(config->min_face_confidence) || config->min_face_confidence < 0.0f ||
        config->min_face_confidence > 1.0f) return ESP_ERR_INVALID_ARG;
    const char *path = config->device_path ? config->device_path : ESP_VIDEO_DVP_DEVICE_NAME;
    if (!path[0] || strlen(path) >= DEVICE_PATH_CAP) return ESP_ERR_INVALID_ARG;
    if (s_claimed.test_and_set()) return ESP_ERR_INVALID_STATE;
    auto *v = new (std::nothrow) bot_vision_t;
    if (!v) { s_claimed.clear(); return ESP_ERR_NO_MEM; }
    v->config = *config;
    snprintf(v->device_path, sizeof(v->device_path), "%s", path);
    v->config.device_path = v->device_path;
    v->enabled.store(config->start_enabled);
    v->working.state = BOT_VISION_STOPPED;
    v->working.calibration_valid = config->calibration_valid;
    v->status = v->working;
    v->status_lock = xSemaphoreCreateMutex();
    v->done = xSemaphoreCreateBinary();
    v->tracker = vatt_tracker_create(&config->tracker);
    if (!v->status_lock || !v->done || !v->tracker) {
        if (v->status_lock) vSemaphoreDelete(v->status_lock);
        if (v->done) vSemaphoreDelete(v->done);
        if (v->tracker) vatt_tracker_destroy(v->tracker);
        delete v;
        s_claimed.clear();
        return ESP_ERR_NO_MEM;
    }
    if (xTaskCreate(vision_task, "bot_vision", CONFIG_BOT_VISION_TASK_STACK_BYTES,
                    v, CONFIG_BOT_VISION_TASK_PRIORITY, &v->task) != pdPASS) {
        vatt_tracker_destroy(v->tracker);
        vSemaphoreDelete(v->done);
        vSemaphoreDelete(v->status_lock);
        delete v;
        s_claimed.clear();
        return ESP_ERR_NO_MEM;
    }
    *out = v;
    return ESP_OK;
}

extern "C" esp_err_t bot_vision_set_enabled(bot_vision_t *v, bool enabled)
{
    if (!v) return ESP_ERR_INVALID_ARG;
    if (v->stopping.load()) return ESP_ERR_INVALID_STATE;
    v->enabled.store(enabled);
    xTaskNotifyGive(v->task);
    return ESP_OK;
}

extern "C" esp_err_t bot_vision_stop(bot_vision_t *v, uint32_t timeout_ms)
{
    if (!v) return ESP_ERR_INVALID_ARG;
    if (xTaskGetCurrentTaskHandle() == v->task) return ESP_ERR_INVALID_STATE;
    v->stopping.store(true);
    v->enabled.store(false);
    xTaskNotifyGive(v->task);
    if (xSemaphoreTake(v->done, ticks_for(timeout_ms)) != pdTRUE) return ESP_ERR_TIMEOUT;
    vTaskDelete(v->task);
    vatt_tracker_destroy(v->tracker);
    vSemaphoreDelete(v->done);
    vSemaphoreDelete(v->status_lock);
    delete v;
    s_claimed.clear();
    return ESP_OK;
}

extern "C" void bot_vision_get_status(bot_vision_t *v, bot_vision_status_t *out)
{
    if (!v || !out) return;
    xSemaphoreTake(v->status_lock, portMAX_DELAY);
    *out = v->status;
    xSemaphoreGive(v->status_lock);
}

extern "C" const char *bot_vision_state_name(bot_vision_state_t state)
{
    switch (state) {
    case BOT_VISION_STOPPED: return "stopped";
    case BOT_VISION_PAUSED: return "paused";
    case BOT_VISION_STARTING: return "starting";
    case BOT_VISION_CAPTURING: return "capturing";
    case BOT_VISION_DEGRADED: return "degraded";
    default: return "unknown";
    }
}

extern "C" const char *bot_vision_quality_name(bot_vision_frame_quality_t quality)
{
    switch (quality) {
    case BOT_VISION_FRAME_NONE: return "none";
    case BOT_VISION_FRAME_OK: return "ok";
    case BOT_VISION_FRAME_DARK: return "dark";
    case BOT_VISION_FRAME_INVALID: return "invalid";
    case BOT_VISION_FRAME_STALE: return "stale";
    case BOT_VISION_FRAME_TIMEOUT: return "timeout";
    case BOT_VISION_FRAME_CAMERA_ERROR: return "camera_error";
    default: return "unknown";
    }
}

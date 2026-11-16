/* Physical USB preview adapter. JPEG/resize/snapshot are official engines.
 * One low-priority writer multiplexes bounded logs, metadata and latest frames.
 * No Wi-Fi media, persistent recording, or behavior decisions live here. */
#include "bot_debug.h"
#include "bot_bus.h"
#include <atomic>
#include <cstdarg>
#include <cstdio>
#include <cstring>
#include "dl_image_process.hpp"
#include "esp_heap_caps.h"
#include "esp_jpeg_enc.h"
#include "esp_log.h"
#include "esp_rom_crc.h"
#include "esp_rom_serial_output.h"
#include "esp_timer.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "freertos/task.h"

namespace {
constexpr size_t FRAME_BYTES = 400 * 240 * 2;
constexpr size_t JPEG_BYTES = 96 * 1024;
constexpr int64_t LEASE_US = 15000000;
struct LogLine { char text[512]; };
struct Event { unsigned kind, source; uint32_t observed_ms; };
const char *event_name(unsigned kind) {
    static const char *names[] = {"wake.detected", "touch.wake", "local.stop", "settings.open",
        "settings.close", "utterance.started", "utterance.ended", "playback.started",
        "playback.done", "playback.failed", "link.state", "audio.health", "camera.health",
        "display.health", "wake.status", "night.policy", "session.end", "fault", "sound.activity",
        "result.dismissed", "local.response.failed", "voice.settings", "sound.preview",
        "capture.ready", "local.command", "capture.overflow", "playback.progress", "turn.submitted",
        "barge.in"};
    static_assert(sizeof(names) / sizeof(names[0]) == BOT_EVENT_COUNT);
    return kind < BOT_EVENT_COUNT ? names[kind] : "unknown";
}
constexpr unsigned LOG_SLOTS = 32;
QueueHandle_t free_logs, pending_logs, events;
LogLine *log_slots;
SemaphoreHandle_t camera_lock;
std::atomic<int64_t> lease_until{0};
std::atomic<uint32_t> dropped_logs{0};
std::atomic<bool> ready{false};
std::atomic<bool> screen_snapshot{false};
std::atomic<uint32_t> camera_transforms{0}, camera_transform_total_us{0}, camera_transform_max_us{0};
bot_platform_t *platform;
// The producer fills camera_slot under camera_lock. Only the USB worker uses
// frame_work, synchronously, for both 400x240 RGB565 sources.
uint8_t *camera_slot, *frame_work, *jpeg;
bool camera_pending;
uint32_t camera_stamp;
int64_t next_camera_us;
uint32_t sequence;
jpeg_enc_handle_t encoder;
struct Timings {
    uint32_t count = 0, failures = 0, total_us = 0, max_us = 0;
    void record(int64_t elapsed, bool ok) {
        if (!ok) { ++failures; return; }
        const uint32_t us = elapsed < 0 ? 0 : elapsed > UINT32_MAX ? UINT32_MAX : (uint32_t)elapsed;
        ++count;
        total_us += us;
        if (us > max_us) max_us = us;
    }
    uint32_t avg() const { return count ? total_us / count : 0; }
};

bool enabled() { return lease_until.load() > esp_timer_get_time(); }

int queue_log(const char *format, va_list args) {
    uint8_t index;
    if (xQueueReceive(free_logs, &index, 0) != pdTRUE) {
        ++dropped_logs;
        return 0;
    }
    LogLine &line = log_slots[index];
    int count = vsnprintf(line.text, sizeof(line.text), format, args);
    if (xQueueSend(pending_logs, &index, 0) != pdTRUE) {
        ++dropped_logs;
        (void)xQueueSend(free_logs, &index, 0);
    }
    return count;
}

void drain_logs() {
    uint8_t index;
    for (unsigned i = 0; i < LOG_SLOTS && xQueueReceive(pending_logs, &index, 0) == pdTRUE; ++i) {
        fputs(log_slots[index].text, stdout);
        (void)xQueueSend(free_logs, &index, 0);
    }
    fflush(stdout);
}

void put16(uint8_t *p, uint16_t value) { p[0] = value; p[1] = value >> 8; }
void put32(uint8_t *p, uint32_t value) {
    for (unsigned i = 0; i < 4; ++i) p[i] = value >> (i * 8);
}

void raw_write(const uint8_t *bytes, size_t length) {
    for (size_t i = 0; i < length; ++i) {
        esp_rom_output_tx_one_char(bytes[i]);
        if ((i & 255) == 255) vTaskDelay(1);
    }
}

void packet(uint8_t source, uint8_t encoding, uint16_t width, uint16_t height,
            uint32_t stamp, const uint8_t *bytes, size_t length) {
    if (!enabled() || length > JPEG_BYTES) return;
    const uint8_t magic[] = {0xd7, 0x39, 'X', 'D', 'B', 'G', 1, 0xa5};
    uint8_t header[32]{};
    memcpy(header, magic, sizeof(magic));
    header[8] = source; header[9] = encoding;
    put16(header + 10, width); put16(header + 12, height);
    put32(header + 16, ++sequence); put32(header + 20, stamp);
    put32(header + 24, length);
    put32(header + 28, esp_rom_crc32_le(0, bytes, length));
    /* Raw ROM UART avoids the console VFS's LF -> CRLF transformation.
     * Normal ESP logs are queued and written by this same task, so control/
     * capture tasks do not block behind a slow JPEG transfer. */
    raw_write(header, sizeof(header));
    raw_write(bytes, length);
}

void image(jpeg_enc_handle_t encoder, uint8_t source, uint16_t width, uint16_t height,
           uint8_t *pixels, size_t bytes, uint32_t stamp, Timings &encode, Timings &send) {
    int length = 0;
    int64_t started = esp_timer_get_time();
    const bool ok = jpeg_enc_process(encoder, pixels, bytes, jpeg, JPEG_BYTES, &length) == JPEG_ERR_OK && length > 0;
    encode.record(esp_timer_get_time() - started, ok);
    if (ok) {
        started = esp_timer_get_time();
        packet(source, 1, width, height, stamp, jpeg, length);
        send.record(esp_timer_get_time() - started, true);
    }
}

void worker(void *) {
    int64_t next_screen = 0;
    int64_t next_health = 0;
    bool was_enabled = false;
    Timings screen_copy, jpeg_encode, serial_send;
    for (;;) {
        drain_logs();
        if (!enabled()) {
            if (was_enabled) {
                screen_snapshot.store(false);
                xSemaphoreTake(camera_lock, portMAX_DELAY);
                camera_pending = false;
                memset(camera_slot, 0, FRAME_BYTES);
                xSemaphoreGive(camera_lock);
                memset(frame_work, 0, FRAME_BYTES);
                ESP_LOGI("bot_debug", "USB preview disabled or lease expired");
            }
            was_enabled = false;
            xQueueReset(events);
            vTaskDelay(pdMS_TO_TICKS(10));
            continue;
        }
        was_enabled = true;
        if (esp_timer_get_time() >= next_health) {
            bot_platform_display_metrics_t display{};
            if (bot_platform_read_display_metrics(platform, &display) == ESP_OK) {
                ESP_LOGI("display_trace", "screen=%ux%u rect=(%ld,%ld)-(%ld,%ld) scroll_y=%ld render_mode=%d refresh_n=%lu refresh_avg_us=%llu refresh_max_us=%lu",
                    display.display_width, display.display_height, (long)display.x1, (long)display.y1,
                    (long)display.x2, (long)display.y2, (long)display.scroll_y, display.render_mode,
                    (unsigned long)display.refreshes,
                    (unsigned long long)(display.refreshes ? display.refresh_total_us / display.refreshes : 0),
                    (unsigned long)display.refresh_max_us);
            }
            const uint32_t transforms = camera_transforms.exchange(0);
            const uint32_t transform_total = camera_transform_total_us.exchange(0);
            const uint32_t transform_max = camera_transform_max_us.exchange(0);
            ESP_LOGI("preview_trace", "screen_mode=%s copy_n=%lu fail=%lu avg_us=%lu max_us=%lu camera_n=%lu camera_avg_us=%lu camera_max_us=%lu",
                screen_snapshot.load() ? "snapshot" : "frame", (unsigned long)screen_copy.count,
                (unsigned long)screen_copy.failures, (unsigned long)screen_copy.avg(), (unsigned long)screen_copy.max_us,
                (unsigned long)transforms, (unsigned long)(transforms ? transform_total / transforms : 0), (unsigned long)transform_max);
            ESP_LOGI("preview_trace", "jpeg_n=%lu fail=%lu avg_us=%lu max_us=%lu uart_n=%lu avg_us=%lu max_us=%lu dropped_logs=%lu",
                (unsigned long)jpeg_encode.count, (unsigned long)jpeg_encode.failures,
                (unsigned long)jpeg_encode.avg(), (unsigned long)jpeg_encode.max_us,
                (unsigned long)serial_send.count, (unsigned long)serial_send.avg(), (unsigned long)serial_send.max_us,
                (unsigned long)dropped_logs.load());
            screen_copy = Timings{}; jpeg_encode = Timings{}; serial_send = Timings{};
            ESP_LOGI("bot_debug", "internal_free=%u internal_largest=%u preview_stack_free=%u heap_ok=%d",
                (unsigned)heap_caps_get_free_size(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
                (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_INTERNAL | MALLOC_CAP_8BIT),
                (unsigned)uxTaskGetStackHighWaterMark(nullptr),
                heap_caps_check_integrity(MALLOC_CAP_INTERNAL, true));
            ESP_LOGI("bot_debug", "psram_free=%u psram_largest=%u",
                (unsigned)heap_caps_get_free_size(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT),
                (unsigned)heap_caps_get_largest_free_block(MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT));
            next_health = esp_timer_get_time() + 15000000;
        }
        Event event;
        for (unsigned i = 0; i < 8 && xQueueReceive(events, &event, 0) == pdTRUE; ++i) {
            char json[192];
            int length = snprintf(json, sizeof(json),
                "{\"kind\":\"%s\",\"event\":%u,\"source\":%u,\"device_ms\":%lu,\"dropped_logs\":%lu}",
                event_name(event.kind), event.kind, event.source, (unsigned long)event.observed_ms,
                (unsigned long)dropped_logs.load());
            if (length > 0 && length < sizeof(json))
                packet(3, 2, 0, 0, event.observed_ms, (const uint8_t *)json, length);
        }
        const int64_t now = esp_timer_get_time();
        if (now >= next_screen) {
            uint32_t stamp;
            const int64_t started = esp_timer_get_time();
            const esp_err_t copy_result = bot_platform_copy_screen(platform, frame_work, FRAME_BYTES, 400, 240, &stamp, screen_snapshot.load());
            const bool copied = copy_result == ESP_OK;
            screen_copy.record(esp_timer_get_time() - started, copied);
            if (copied) image(encoder, 1, 400, 240, frame_work, FRAME_BYTES, stamp, jpeg_encode, serial_send);
            /* A busy display lock is transient. Retrying soon avoids keeping
             * a stale frame for another full two-second preview interval. */
            next_screen = esp_timer_get_time() + (copied ? 2000000 : copy_result == ESP_ERR_TIMEOUT ? 200000 : 1000000);
        }
        drain_logs();
        bool have_camera = false;
        uint32_t stamp = 0;
        if (xSemaphoreTake(camera_lock, 0) == pdTRUE) {
            if (camera_pending) {
                // Hand off the complete camera frame without copying or holding the
                // producer lock during JPEG encoding and the serial write.
                auto *previous_work = frame_work;
                frame_work = camera_slot;
                camera_slot = previous_work;
                stamp = camera_stamp;
                camera_pending = false;
                have_camera = true;
            }
            xSemaphoreGive(camera_lock);
        }
        if (have_camera && enabled()) image(encoder, 2, 400, 240,
                                           frame_work, FRAME_BYTES, stamp, jpeg_encode, serial_send);
        vTaskDelay(pdMS_TO_TICKS(10));
    }
}
}

extern "C" esp_err_t bot_debug_start(bot_platform_t *p) {
    if (!p || platform) return ESP_ERR_INVALID_ARG;
    platform = p;
    /* Keep message storage out of the calling task's stack and out of scarce
     * internal RAM. The queues contain only indices into this bounded pool. */
    log_slots = (LogLine *)heap_caps_malloc(LOG_SLOTS * sizeof(LogLine), MALLOC_CAP_SPIRAM);
    free_logs = xQueueCreate(LOG_SLOTS, sizeof(uint8_t));
    pending_logs = xQueueCreate(LOG_SLOTS, sizeof(uint8_t));
    events = xQueueCreate(8, sizeof(Event));
    camera_lock = xSemaphoreCreateMutex();
    camera_slot = (uint8_t *)heap_caps_aligned_alloc(16, FRAME_BYTES, MALLOC_CAP_SPIRAM);
    frame_work = (uint8_t *)heap_caps_aligned_alloc(16, FRAME_BYTES, MALLOC_CAP_SPIRAM);
    jpeg = (uint8_t *)heap_caps_aligned_alloc(16, JPEG_BYTES, MALLOC_CAP_SPIRAM);
    auto fail = [](esp_err_t error) {
        if (encoder) jpeg_enc_close(encoder);
        encoder = nullptr;
        heap_caps_free(camera_slot);
        heap_caps_free(frame_work); heap_caps_free(jpeg);
        camera_slot = frame_work = jpeg = nullptr;
        if (free_logs) vQueueDelete(free_logs);
        if (pending_logs) vQueueDelete(pending_logs);
        heap_caps_free(log_slots); log_slots = nullptr;
        if (events) vQueueDelete(events);
        if (camera_lock) vSemaphoreDelete(camera_lock);
        free_logs = pending_logs = events = nullptr; camera_lock = nullptr; platform = nullptr;
        return error;
    };
    if (!log_slots || !free_logs || !pending_logs || !events || !camera_lock || !camera_slot || !frame_work || !jpeg)
        return fail(ESP_ERR_NO_MEM);
    for (uint8_t i = 0; i < LOG_SLOTS; ++i) (void)xQueueSend(free_logs, &i, 0);
    jpeg_enc_config_t config = DEFAULT_JPEG_ENC_CONFIG();
    config.width = 400; config.height = 240;
    config.src_type = JPEG_PIXEL_FORMAT_RGB565_LE;
    config.quality = 40;
    /* Both frames share one fixed-size encoder to conserve internal RAM. */
    if (jpeg_enc_open(&config, &encoder) != JPEG_ERR_OK) return fail(ESP_FAIL);
    if (xTaskCreate(worker, "bot_usb_preview", 6144, nullptr, 2, nullptr) != pdPASS)
        return fail(ESP_ERR_NO_MEM);
    ready.store(true);
    esp_log_set_vprintf(queue_log);
    ESP_LOGI("bot_debug", "USB dual preview ready; disabled until explicit preview on");
    return ESP_OK;
}

extern "C" void bot_debug_set_preview(bool value) {
    if (!ready.load()) return;
    lease_until.store(value ? esp_timer_get_time() + LEASE_US : 0);
}
extern "C" void bot_debug_keepalive() {
    if (enabled()) lease_until.store(esp_timer_get_time() + LEASE_US);
}
extern "C" bool bot_debug_preview_enabled() { return ready.load() && enabled(); }
extern "C" void bot_debug_set_screen_snapshot(bool value) {
    screen_snapshot.store(value);
    ESP_LOGI("preview_trace", "screen capture mode=%s", value ? "snapshot" : "frame");
}

extern "C" void bot_debug_camera(const void *pixels, uint16_t width, uint16_t height,
                                  bot_debug_pixels_t format, uint32_t stamp,
                                  const vatt_observation_t *observations, size_t count) {
    if (!bot_debug_preview_enabled() || !pixels || esp_timer_get_time() < next_camera_us ||
        xSemaphoreTake(camera_lock, 0) != pdTRUE) return;
    const int64_t started = esp_timer_get_time();
    static const dl::image::pix_type_t types[] = {
        dl::image::DL_IMAGE_PIX_TYPE_RGB565LE, dl::image::DL_IMAGE_PIX_TYPE_RGB565BE,
        dl::image::DL_IMAGE_PIX_TYPE_RGB888, dl::image::DL_IMAGE_PIX_TYPE_UYVY,
        dl::image::DL_IMAGE_PIX_TYPE_YUYV,
    };
    if ((unsigned)format < sizeof(types) / sizeof(types[0])) {
        dl::image::img_t source{const_cast<void *>(pixels), width, height, types[format]};
        dl::image::img_t target{camera_slot, 400, 240, dl::image::DL_IMAGE_PIX_TYPE_RGB565LE};
        dl::image::ImageTransformer transform;
        int left = 0, right = 0, top = 0, bottom = 0;
        if ((unsigned)width * 240 > (unsigned)height * 400) {
            int used = (unsigned)height * 400 / width;
            top = (240 - used) / 2; bottom = 240 - used - top;
        } else {
            int used = (unsigned)width * 240 / height;
            left = (400 - used) / 2; right = 400 - used - left;
        }
        if (transform.set_src_img(source).set_dst_img(target).set_bg_value(std::array<uint8_t,3>{0,0,0})
                .set_dst_img_border({top,bottom,left,right}).transform() == ESP_OK) {
            /* Draw only accepted results, and only on the debug copy. The
             * inference input and behavior pipeline remain untouched. */
            const int used_width = 400 - left - right, used_height = 240 - top - bottom;
            auto *out = reinterpret_cast<uint16_t *>(camera_slot);
            for (size_t i = 0; observations && i < count && i < VATT_MAX_FACES; ++i) {
                const auto &b = observations[i].bbox;
                int x0 = left + (int)(b.left * (used_width - 1));
                int x1 = left + (int)(b.right * (used_width - 1));
                int y0 = top + (int)(b.top * (used_height - 1));
                int y1 = top + (int)(b.bottom * (used_height - 1));
                if (x0 < left || x1 >= 400 - right || y0 < top || y1 >= 240 - bottom ||
                    x0 >= x1 || y0 >= y1) continue;
                for (int x = x0; x <= x1; ++x) {
                    out[y0 * 400 + x] = 0x07e0;
                    out[y1 * 400 + x] = 0x07e0;
                }
                for (int y = y0; y <= y1; ++y) {
                    out[y * 400 + x0] = 0x07e0;
                    out[y * 400 + x1] = 0x07e0;
                }
            }
            camera_pending = true;
            camera_stamp = stamp;
            next_camera_us = esp_timer_get_time() + 2000000;
        }
    }
    const int64_t elapsed = esp_timer_get_time() - started;
    if (elapsed >= 0 && elapsed <= UINT32_MAX) {
        ++camera_transforms;
        camera_transform_total_us.fetch_add((uint32_t)elapsed);
        uint32_t previous = camera_transform_max_us.load();
        while (previous < (uint32_t)elapsed &&
               !camera_transform_max_us.compare_exchange_weak(previous, (uint32_t)elapsed)) {}
    }
    xSemaphoreGive(camera_lock);
}

extern "C" void bot_debug_event(unsigned kind, unsigned source, uint32_t stamp) {
    if (!bot_debug_preview_enabled()) return;
    Event event{kind, source, stamp};
    (void)xQueueSend(events, &event, 0);
}

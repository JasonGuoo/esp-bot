#include "bot_bus.h"
#include <stdlib.h>
#include <string.h>
#include "freertos/queue.h"
#include "freertos/semphr.h"

struct bot_bus {
    QueueHandle_t critical_events, events, commands;
    SemaphoreHandle_t snapshot_lock;
    portMUX_TYPE signal_lock;
    bot_bus_signals_t signals;
    bot_bus_stats_t stats;
    bool has_vision;
    vatt_snapshot_t vision;
};

static bool critical_event(const bot_event_t *event)
{
    /* Only replaceable health observations may use the lossy queue. */
    if (event->kind == BOT_EVENT_DISPLAY_HEALTH && event->data.display.error == ESP_OK) return false;
    return true;
}

bot_bus_config_t bot_bus_default_config(void)
{
    return (bot_bus_config_t){ .critical_event_capacity = 24, .event_capacity = 16,
                              .command_capacity = 8 };
}

esp_err_t bot_bus_create(const bot_bus_config_t *config, bot_bus_t **out)
{
    if (!out) return ESP_ERR_INVALID_ARG;
    *out = NULL;
    bot_bus_config_t cfg = config ? *config : bot_bus_default_config();
    if (!cfg.critical_event_capacity || !cfg.event_capacity || !cfg.command_capacity ||
        cfg.critical_event_capacity > 256 || cfg.event_capacity > 256 || cfg.command_capacity > 64)
        return ESP_ERR_INVALID_ARG;
    bot_bus_t *bus = calloc(1, sizeof(*bus));
    if (!bus) return ESP_ERR_NO_MEM;
    bus->signal_lock = (portMUX_TYPE)portMUX_INITIALIZER_UNLOCKED;
    bus->critical_events = xQueueCreate(cfg.critical_event_capacity, sizeof(bot_event_t));
    bus->events = xQueueCreate(cfg.event_capacity, sizeof(bot_event_t));
    bus->commands = xQueueCreate(cfg.command_capacity, sizeof(bot_command_t));
    bus->snapshot_lock = xSemaphoreCreateMutex();
    if (!bus->critical_events || !bus->events || !bus->commands || !bus->snapshot_lock) {
        bot_bus_destroy(bus);
        return ESP_ERR_NO_MEM;
    }
    *out = bus;
    return ESP_OK;
}

void bot_bus_destroy(bot_bus_t *bus)
{
    if (!bus) return;
    if (bus->critical_events) vQueueDelete(bus->critical_events);
    if (bus->events) vQueueDelete(bus->events);
    if (bus->commands) vQueueDelete(bus->commands);
    if (bus->snapshot_lock) vSemaphoreDelete(bus->snapshot_lock);
    free(bus);
}

void bot_bus_request_stop(bot_bus_t *bus, uint32_t observed_ms)
{
    if (!bus) return;
    portENTER_CRITICAL(&bus->signal_lock);
    bus->signals.local_stop = true;
    bus->signals.observed_ms = observed_ms;
    portEXIT_CRITICAL(&bus->signal_lock);
}

bot_bus_signals_t bot_bus_take_signals(bot_bus_t *bus)
{
    bot_bus_signals_t signals = {0};
    if (!bus) return signals;
    portENTER_CRITICAL(&bus->signal_lock);
    signals = bus->signals;
    memset(&bus->signals, 0, sizeof(bus->signals));
    portEXIT_CRITICAL(&bus->signal_lock);
    return signals;
}

esp_err_t bot_bus_post(bot_bus_t *bus, const bot_event_t *event, TickType_t wait_ticks)
{
    if (!bus || !event || event->kind < 0 || event->kind >= BOT_EVENT_COUNT ||
        event->source < BOT_SOURCE_PLATFORM || event->source > BOT_SOURCE_SYSTEM)
        return ESP_ERR_INVALID_ARG;
    if (event->kind == BOT_EVENT_LOCAL_STOP) {
        bot_bus_request_stop(bus, event->observed_ms);
        portENTER_CRITICAL(&bus->signal_lock);
        bus->stats.events_posted++;
        portEXIT_CRITICAL(&bus->signal_lock);
        return ESP_OK;
    }
    bool critical = critical_event(event);
    BaseType_t accepted = xQueueSend(critical ? bus->critical_events : bus->events,
                                     event, wait_ticks);
    portENTER_CRITICAL(&bus->signal_lock);
    if (accepted == pdTRUE) {
        bus->stats.events_posted++;
    } else if (critical) {
        bus->stats.critical_events_rejected++;
        bus->signals.critical_overflow = true;
        bus->signals.observed_ms = event->observed_ms;
    } else {
        bus->stats.normal_events_dropped++;
    }
    portEXIT_CRITICAL(&bus->signal_lock);
    return accepted == pdTRUE ? ESP_OK : ESP_ERR_TIMEOUT;
}

bool bot_bus_receive(bot_bus_t *bus, bot_event_t *out)
{
    if (!bus || !out) return false;
    if (xQueueReceive(bus->critical_events, out, 0) != pdTRUE &&
        xQueueReceive(bus->events, out, 0) != pdTRUE) return false;
    portENTER_CRITICAL(&bus->signal_lock);
    bus->stats.events_received++;
    portEXIT_CRITICAL(&bus->signal_lock);
    return true;
}

esp_err_t bot_bus_submit_command(bot_bus_t *bus, const bot_command_t *command,
                                  TickType_t wait_ticks)
{
    if (!bus || !command) return ESP_ERR_INVALID_ARG;
    BaseType_t accepted = xQueueSend(bus->commands, command, wait_ticks);
    portENTER_CRITICAL(&bus->signal_lock);
    if (accepted == pdTRUE) bus->stats.commands_posted++;
    else bus->stats.commands_rejected++;
    portEXIT_CRITICAL(&bus->signal_lock);
    return accepted == pdTRUE ? ESP_OK : ESP_ERR_TIMEOUT;
}

bool bot_bus_receive_command(bot_bus_t *bus, bot_command_t *out)
{
    return bus && out && xQueueReceive(bus->commands, out, 0) == pdTRUE;
}

esp_err_t bot_bus_publish_vision(bot_bus_t *bus, const vatt_snapshot_t *snapshot)
{
    if (!bus || !snapshot || snapshot->face_count > VATT_MAX_FACES ||
        snapshot->event_count > VATT_MAX_EVENTS) return ESP_ERR_INVALID_ARG;
    /* Mutex protects only a bounded struct copy; neither inference nor driver I/O
     * may run while it is held. Snapshot contention must not stall capture. */
    if (xSemaphoreTake(bus->snapshot_lock, 0) != pdTRUE) return ESP_ERR_TIMEOUT;
    bool stale = bus->has_vision && (int32_t)(snapshot->timestamp_ms - bus->vision.timestamp_ms) <= 0;
    if (!stale) {
        bus->vision = *snapshot;
        bus->has_vision = true;
    }
    xSemaphoreGive(bus->snapshot_lock);
    portENTER_CRITICAL(&bus->signal_lock);
    if (stale) bus->stats.vision_stale_rejected++;
    else bus->stats.vision_published++;
    portEXIT_CRITICAL(&bus->signal_lock);
    return stale ? ESP_ERR_INVALID_STATE : ESP_OK;
}

bool bot_bus_read_vision(bot_bus_t *bus, vatt_snapshot_t *out)
{
    if (!bus || !out || xSemaphoreTake(bus->snapshot_lock, 0) != pdTRUE) return false;
    bool available = bus->has_vision;
    if (available) *out = bus->vision;
    xSemaphoreGive(bus->snapshot_lock);
    return available;
}

bool bot_bus_clear_vision(bot_bus_t *bus)
{
    if (!bus || xSemaphoreTake(bus->snapshot_lock, 0) != pdTRUE) return false;
    bus->has_vision = false;
    memset(&bus->vision, 0, sizeof(bus->vision));
    xSemaphoreGive(bus->snapshot_lock);
    return true;
}

void bot_bus_get_stats(bot_bus_t *bus, bot_bus_stats_t *out)
{
    if (!bus || !out) return;
    portENTER_CRITICAL(&bus->signal_lock);
    *out = bus->stats;
    portEXIT_CRITICAL(&bus->signal_lock);
}

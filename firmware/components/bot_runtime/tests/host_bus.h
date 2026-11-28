/* Single-step host adapter shared by runtime tests and the simulator. */
#pragma once
#include "bot_runtime.h"

struct bot_bus {
    bool has_event, has_vision, has_command;
    bot_event_t event;
    bot_command_t command;
    vatt_snapshot_t vision;
    bot_bus_signals_t signals;
};

bool bot_bus_receive(bot_bus_t *bus, bot_event_t *event)
{
    if (!bus->has_event) return false;
    *event = bus->event;
    bus->has_event = false;
    return true;
}
bool bot_bus_receive_command(bot_bus_t *bus, bot_command_t *command)
{
    if (!bus->has_command) return false;
    *command = bus->command;
    bus->has_command = false;
    return true;
}
bool bot_bus_events_pending(bot_bus_t *bus) { return bus->has_event; }
bot_bus_signals_t bot_bus_take_signals(bot_bus_t *bus)
{
    bot_bus_signals_t signals = bus->signals;
    bus->signals = (bot_bus_signals_t){0};
    return signals;
}
bool bot_bus_read_vision(bot_bus_t *bus, vatt_snapshot_t *vision)
{
    if (!bus->has_vision) return false;
    *vision = bus->vision;
    return true;
}
bool bot_bus_clear_vision(bot_bus_t *bus)
{
    bus->has_vision = false;
    return true;
}

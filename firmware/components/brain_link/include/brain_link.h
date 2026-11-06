#pragma once
#include "esp_err.h"
#include "bot_bus.h"
#ifdef __cplusplus
extern "C" {
#endif
/** Optional Wi-Fi STA + existing LAN discovery. Starts once; blank SSID is
 * offline success. Current WS handshake is NOT authenticated. Incoming
 * requests receive AUTH_REQUIRED and never become runtime commands.
 * No camera frames, microphone audio, or face/person data are sent. */
esp_err_t brain_link_start(bot_bus_t *bus);
#ifdef __cplusplus
}
#endif

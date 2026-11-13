#pragma once
#include "esp_err.h"
#include "bot_bus.h"
#include "bot_audio.h"
#include "bot_runtime.h"
#ifdef __cplusplus
extern "C" {
#endif
/** Optional Wi-Fi STA + existing LAN discovery. Starts once; blank SSID is
 * offline success. Current WS handshake is NOT authenticated. Incoming
 * requests receive AUTH_REQUIRED and never become runtime commands.
 * No camera frames, microphone audio, or face/person data are sent. */
esp_err_t brain_link_start(bot_bus_t *bus);
/** Start optional Bonjour diagnostics after the authenticated client has its resources. */
void brain_link_start_diagnostics(void);
/* Save a 2.4 GHz Wi-Fi configuration received through the local USB console.
 * This does not log credentials; it becomes active on the next boot. */
esp_err_t brain_link_wifi_provision(const char *base64_json);
/* Authenticated WSS is independent of LAN diagnostics. Provisioning accepts
 * base64 JSON over a physically connected USB console, never the LAN. */
esp_err_t brain_secure_start(bot_bus_t *bus, bot_audio_t *audio);
esp_err_t brain_secure_provision(const char *base64_json);
void brain_secure_update(const bot_runtime_snapshot_t *state);
bool brain_secure_uplink(const bot_session_token_t *token);
void brain_secure_pcm(const bot_audio_pcm_t *frame);
void brain_secure_network_down(void);

#define BOT_DISPLAY_ASSET_ID_BYTES 48
typedef struct {
    bot_session_token_t token;
    uint8_t *pixels;
    uint16_t width, height;
    char asset_id[BOT_DISPLAY_ASSET_ID_BYTES];
} bot_display_image_t;
/** Transfer a decoded image to the caller. Caller owns pixels on success. */
bool brain_secure_take_image(bot_display_image_t *image);
/** Queue a display receipt for the authenticated session. False means the
 * bounded queue could not accept it; callers may retry while the token is current. */
bool brain_secure_display_report(bot_session_token_t token, const char *part,
                                 const char *asset_id, bool rendered, const char *code);
/** Record a terminal image-only failure without allocating. The transport task
 * posts a runtime notice and an unavailable receipt; neither replaces the answer. */
void brain_secure_image_failed(bot_session_token_t token, const char *asset_id, const char *code);
#ifdef __cplusplus
}
#endif

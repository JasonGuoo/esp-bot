# brain_link — local runtime ingress boundary

Starts optional Wi-Fi STA using the ESP-IDF station pattern, then reuses `lan_link`. Blank SSID leaves the local runtime offline. Reconnect attempts are rate-limited to at most one every three seconds.

The existing LAN WS hello/welcome protocol is unauthenticated and unencrypted. This adapter reports `connected` separately from `authenticated=false/encrypted=false`; incoming request envelopes receive an `AUTH_REQUIRED` response. Nothing calls Face or mutates runtime from remote JSON. No raw microphone, camera, face track or conversation content is transmitted.

The runtime's typed semantic command queue exists, but connecting it to an authenticated media/session transport is future work. Do not "enable" it by marking plaintext LAN trusted. The legacy HTTP `brain_client` is no longer built by main.

`lan_link` responses and events are bound to a connection generation and are dropped across reconnect. Enqueue errors remain explicit. Source written without build, tests, hardware or CI; see [integration guide](../../../docs/local-runtime.md).

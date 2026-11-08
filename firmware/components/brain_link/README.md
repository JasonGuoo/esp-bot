# brain_link — local runtime ingress boundary

Starts optional Wi-Fi STA using the ESP-IDF station pattern, then reuses `lan_link`.
On every boot, a configured SSID is applied in RAM; when blank, the adapter tries
the driver's previously provisioned NVS configuration. With neither, it stays
offline. Normal boots and reconnects do not rewrite Wi-Fi credentials. Reconnect
attempts are rate-limited to at most one every three seconds.

`ESPBOT_SERVER_ID` optionally selects the intended diagnostic Mac server by its
stable public ID. Discovery runs again after boot or disconnection, so an old
DHCP address or ephemeral port is not persisted. A configured but absent server
does not cause fallback to a different server. Its ID is not authentication.

The existing LAN WS hello/welcome protocol is unauthenticated and unencrypted. This adapter reports `connected` separately from `authenticated=false/encrypted=false`; incoming request envelopes receive an `AUTH_REQUIRED` response. Nothing calls Face or mutates runtime from remote JSON. No raw microphone, camera, face track or conversation content is transmitted.

The runtime's typed semantic command queue exists, but connecting it to an authenticated media/session transport is future work. Do not "enable" it by marking plaintext LAN trusted. The legacy HTTP `brain_client` is no longer built by main.

`lan_link` responses and events are bound to a connection generation and are dropped across reconnect. Enqueue errors remain explicit. The original source delivery was unbuilt; the first local build and hardware bring-up are recorded separately in [the bring-up log](../../../docs/board-bringup-2026-11-06.md). No CI is used. See also [integration guide](../../../docs/local-runtime.md).

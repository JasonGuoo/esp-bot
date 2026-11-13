# brain_link — local runtime ingress boundary

Starts optional Wi-Fi STA using the ESP-IDF station pattern, then reuses `lan_link`.
On every boot, a USB-provisioned NVS override takes priority, followed by a
build-time SSID and then the driver's previously provisioned NVS configuration.
With none, it stays offline. Normal boots and reconnects do not rewrite Wi-Fi credentials. Reconnect
attempts are rate-limited to at most one every three seconds.

The local USB configuration command accepts a bounded SSID/password payload,
persists it only when valid, and restarts the board. The console redacts this
command from events and exports. Current board flash/NVS is not encrypted, so
physical possession can expose saved credentials.

`ESPBOT_SERVER_ID` optionally selects the intended diagnostic Mac server by its
stable public ID. Discovery runs again after boot or disconnection, so an old
DHCP address or ephemeral port is not persisted. A configured but absent server
does not cause fallback to a different server. Its ID is not authentication.

The existing LAN WS hello/welcome protocol is unauthenticated and unencrypted. This adapter reports `connected` separately from `authenticated=false/encrypted=false`; incoming request envelopes receive an `AUTH_REQUIRED` response. Nothing calls Face or mutates runtime from remote JSON. No raw microphone, camera, face track or conversation content is transmitted.

`brain_secure.c` separately implements the Mac v3 TLS WebSocket client,
physically provisioned certificate/token, post-activation plaintext LAN TCP PCM
(per-frame authenticated but not encrypted), bounded playback,
and runtime-token checks. It initializes only after the network stack. The old
diagnostic connection never reports itself as authenticated. The legacy HTTP
`brain_client` is no longer built by main. Build/flash/acceptance and remaining
Wi-Fi, device memory, image and speaker limitations are recorded in
[the current integration log](../../../docs/server-integration-2026-11-08.md).

Before every v3 connection the client resolves the paired Mac's
`_espbot-brain._tcp.local.` address and port and verifies the TLS identity saved
during pairing. It first tries the discovered host directly. For a connection
timeout on an on-link peer, it can retry the same destination using the DHCP
gateway as its next hop. This handles the LAN observed on the first board, where
the TCP SYN-ACK and subsequent packets arrived with different source addresses
on the direct path. See the [board results](../../../docs/specs/server-voice-tasks/acceptance.md).

## Display result failures and receipts

Image-only failures preserve the accepted text reply and its caption. The current
image's token and first failure code enter a fixed mailbox under the transport
lock, so descriptor/chunk/JPEG/RGB565 allocation failures do not need another
allocation to record an error. The worker posts `BOT_COMMAND_IMAGE_FAILED` for the
runtime-owned notice and queues `display.report` with `status=unavailable` and a
bounded `code`. Full command/report queues retry while the token remains current;
they do not turn an image-only failure into a session-wide Stop.

Covered paths include invalid descriptors/chunks, request send failure, server
errors/expiry, exhausted chunk retry, SHA-256 mismatch, JPEG header/decode failure,
allocation failure, image queue saturation and rejected LVGL mounting. The worker
checks live authority after dequeuing image messages, since a WSS callback can
finish the turn between two worker instructions. Stale images and notices cannot
replace a new turn's result.

Positive receipts come from the application only after the result widget's
visible draw, refresh completion and the driver's following flush-wait fence.
The Mac retains its existing current-token and 30-second result window. An image
first scrolled into view after that window remains unconfirmed on the Mac. If a
malformed descriptor has no valid asset identity, the local notice can still be
shown, but the Mac cannot accept an image receipt for an unmatched asset.

These changes are source-only as of 2026-11-13. No build, tests, lint, flashing,
hardware interaction, model calls or CI were run for this correction.

`lan_link` responses and events are bound to a connection generation and are dropped across reconnect. Enqueue errors remain explicit. The original source delivery was unbuilt; the first local build and hardware bring-up are recorded separately in [the bring-up log](../../../docs/board-bringup-2026-11-06.md). No CI is used. See also [integration guide](../../../docs/local-runtime.md).

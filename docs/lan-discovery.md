# LAN discovery: isolated integration checkpoint

## Existing code and deliberate non-integration

The repository's Python Brain in server/espbot/ and fixed URL ESP-IDF polling
client in firmware/main/brain_client.c are untouched. The new modules do not
replace the current executable firmware or Python server yet:

| Module | Location | Scope |
| --- | --- | --- |
| Rust/Tauri console | server/bot-server/ | mDNS advertise; WebSocket accept; connected devices; local events |
| ESP-IDF lan_link | firmware/components/lan_link/ | mDNS query; auto-connect (single server only); manual scan/connect; retry; JSON events |

The wire contract is shared/lan-discovery-v1.md. There is no cloud broker,
Internet discovery, BLE, LLM, ASR, TTS, face or motor integration in this work.

## Developer bring-up, later

1. On a Mac/Linux with Rust, Node and Tauri 2 native prerequisites installed,
   run: cd server/bot-server && npm install && npm run tauri dev
2. Allow local-network/multicast permissions and the TCP port in the host
   firewall. Console should display "Advertising on LAN".
3. On macOS inspect with: dns-sd -B _espbot-server._tcp local
   On Linux: avahi-browse -rt _espbot-server._tcp
4. In a future firmware main integration, initialize Wi-Fi STA/netif and call
   bot_lan_start() with auto_connect=true; observe callbacks. **Flashing the
   existing firmware without integration will not start this component.**
5. One server: auto hello/welcome, device row in console. Two servers: expect
   SELECTION_REQUIRED; use bot_lan_scan(), bot_lan_copy_servers() and
   bot_lan_connect() from the future settings UI.
6. Disconnect server/Wi-Fi; expect a disconnected state and rescan. Verify
   malformed hello, wrong TXT version/UUID and external peer rejection.
7. Verify the previous Python server and face preview remain unchanged.

## Verification status

- Rust protocol/session-state unit tests are included under lan/.
  Run: cargo test --manifest-path server/bot-server/src-tauri/Cargo.toml
- Hardware remains ESP32-S31 Korvo-1 with ESP-IDF 6.1+, not ESP32-S3.
- OS-level Tauri/WebKit libraries and ESP-IDF are needed for compilation and
  execution. **No board or end-to-end tests have been performed here.**
- Guest Wi-Fi, access-point client isolation and VPNs may block mDNS.
  This is a development transport, not authenticated pairing.

## 2026-10-30 next-transport decision (not implemented)

[Local-First Hybrid Brain](local-first-hybrid-brain.md) supersedes the LAN-only **scope** of this v1 implementation for future work. Existing Rust/Tauri mDNS + WS and firmware lan_link remain valid, standalone **Wi-Fi** components. Future additions, in separate modules, will add **secure Mac BLE provisioning + Wi-Fi onboarding and a low-bandwidth BLE control fallback**, using Espressif maintained provisioning rather than another homemade setup stack. Local-first/MLX speech on the Mac remains the default; Bluetooth audio is a measured optional extension and is not a solved feature. See [reuse catalog](upstream-reuse-catalog.md).

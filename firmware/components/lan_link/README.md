# lan_link — reusable ESP32-S31 LAN discovery and WebSocket client

Reusable ESP-IDF component, now composed by app_main through **brain_link**.
It neither starts Wi-Fi nor touches LVGL. The old brain_client is not started. The main application must own Wi-Fi credentials and create a STA
network interface. This component waits for a usable IPv4 address and handles
mDNS plus a persistent WebSocket session without a fixed server IP or port.

## Public API

Header: include/bot_lan.h. The API is asynchronous except for snapshot reads.

- bot_lan_start(config, callback, user_ctx): start worker once
- bot_lan_scan(): manual discovery, emits SCAN_COMPLETE
- bot_lan_copy_servers(list, capacity): copied list for settings picker
- bot_lan_connect(&list[index]): manual connection to selected server
- bot_lan_disconnect(): disconnect and pause auto-connect
- bot_lan_resume_auto(): re-enable automatic searches
- bot_lan_state(): current status
- bot_lan_send_event(json): bounded event envelope
- bot_lan_send_response(json): bounded response with id/ok

Both send paths bind queued frames to the current connection generation; old
queued output is dropped across reconnect. Enqueue failure is returned explicitly.

Automatic mode connects only when **exactly one** compatible server is found.
When multiple exist, it emits SELECTION_REQUIRED and waits for the settings UI
to let the user choose. Wi-Fi loss triggers recovery; after a failed handshake,
the worker retries discovery on its configured interval.

### Example integration (the current app's adapter is in brain_link)

    static void lan_event(const bot_lan_event_t *ev, void *ctx) {
        (void)ctx;
        // Send a copy to the UI/LVGL queue; never draw here.
        // SCAN_COMPLETE: read bot_lan_copy_servers()
        // CONNECTED: update the connection indicator
    }

    bot_lan_config_t cfg = {
        .device_id = "korvo-01",
        .device_name = "Desktop Bot",
        .auto_connect = true,
        .scan_interval_ms = 5000,
    };
    // After Wi-Fi STA/netif setup:
    ESP_ERROR_CHECK(bot_lan_start(&cfg, lan_event, NULL));

### DNS-SD

Queries PTR for _espbot-server._tcp.local. TXT must include proto=1,
path=/v1/lan/ws and id=<UUID>. The worker prefers a server IP in the same Wi-Fi
subnet. The mDNS resolver and WebSocket client are installed via the
component's idf_component.yml.

### Important boundaries

- Callback runs in a networking context; never call LVGL directly.
- No Wi-Fi setup, credentials, audio, face or motor changes.
- No pairing or cryptographic verification yet. Current brain_link rejects remote
  requests with AUTH_REQUIRED; this channel is only diagnostic and carries no private media.
- Discovery records are not identity proofs. The server ID and protocol are
  compatibility checks, not authenticity guarantees.
- Automatic discovery requires working LAN multicast (UDP 5353); guest Wi-Fi
  with client isolation may block it.
- Initial link uses IPv4. No Internet or cross-subnet rendezvous.
- Do not call mdns_free from this module because other components may share
  the resolver. Set mdns_preinitialized=true if the app initialized it.
- This integration was not compiled, tested, or run on hardware. No CI was added.

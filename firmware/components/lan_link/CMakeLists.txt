idf_component_register(
    SRCS "bot_lan.c" "discovery.c"
    INCLUDE_DIRS "include"
    PRIV_INCLUDE_DIRS "."
    REQUIRES esp_wifi esp_netif esp_event
             espressif__mdns espressif__esp_websocket_client espressif__cjson
)

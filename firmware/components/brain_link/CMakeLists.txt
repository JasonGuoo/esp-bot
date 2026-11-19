idf_component_register(
    SRCS "brain_link.c" "brain_secure.c" "peer_path.c"
    INCLUDE_DIRS "include"
    REQUIRES bot_bus bot_audio bot_runtime lan_link esp_app_format esp_wifi esp_event esp_netif esp_timer
             nvs_flash mbedtls espressif__esp_websocket_client espressif__cjson espressif__esp_new_jpeg
)

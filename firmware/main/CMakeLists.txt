idf_component_register(
    SRCS "app_main.c" "brain_client.c"
    INCLUDE_DIRS "."
    REQUIRES face esp_wifi esp_event esp_netif
             esp_http_client nvs_flash
)

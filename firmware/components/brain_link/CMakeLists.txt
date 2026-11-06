idf_component_register(
    SRCS "brain_link.c"
    INCLUDE_DIRS "include"
    REQUIRES bot_bus lan_link esp_wifi esp_event esp_netif esp_timer espressif__cjson
)

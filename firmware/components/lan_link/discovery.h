#pragma once

#include <stddef.h>
#include "esp_err.h"
#include "bot_lan.h"

/* Worker-only. The caller initializes mDNS and owns the result snapshot. */
esp_err_t bot_lan_discovery_scan(bot_lan_server_t *servers,
                                 size_t capacity, size_t *count);

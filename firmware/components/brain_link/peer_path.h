#pragma once
#include <stdbool.h>

/* Affects only packets addressed to this paired LAN peer on WIFI_STA_DEF. */
bool brain_peer_path(const char *peer_ip, bool via_dhcp_gateway);

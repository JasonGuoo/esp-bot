#include "discovery.h"

#include <stdio.h>
#include <string.h>
#include "mdns.h"
#include "esp_netif.h"
#include "esp_netif_ip_addr.h"

static const char *txt_value(const mdns_result_t *result, const char *key) {
    for (size_t i = 0; i < result->txt_count; ++i) {
        if (result->txt[i].key && result->txt[i].value &&
            strcmp(result->txt[i].key, key) == 0) {
            return result->txt[i].value;
        }
    }
    return NULL;
}

/* Prefer the service address reachable from our Wi-Fi subnet on multihomed Macs. */
static const esp_ip4_addr_t *best_ipv4(const mdns_result_t *result,
                                     const esp_netif_ip_info_t *local) {
    const esp_ip4_addr_t *fallback = NULL;
    for (const mdns_ip_addr_t *addr = result->addr; addr; addr = addr->next) {
        if (addr->addr.type != ESP_IPADDR_TYPE_V4) continue;
        const esp_ip4_addr_t *candidate = &addr->addr.u_addr.ip4;
        if (!fallback) fallback = candidate;
        if (local && local->ip.addr != 0 &&
            (candidate->addr & local->netmask.addr) ==
            (local->ip.addr & local->netmask.addr)) {
            return candidate;
        }
    }
    return fallback;
}

esp_err_t bot_lan_find_secure_gateway(const char *ca_sha256, char ip[16], uint16_t *port) {
    if (!ca_sha256 || strlen(ca_sha256) != 64 || !ip || !port) return ESP_ERR_INVALID_ARG;
    esp_netif_ip_info_t local = {0};
    esp_netif_t *wifi = esp_netif_get_handle_from_ifkey("WIFI_STA_DEF");
    if (!wifi || esp_netif_get_ip_info(wifi, &local) != ESP_OK || !local.ip.addr) return ESP_ERR_INVALID_STATE;
    mdns_result_t *results = NULL;
    esp_err_t err = mdns_query_ptr("_espbot-brain", "_tcp", 1500, 12, &results);
    if (err != ESP_OK) return err;
    err = ESP_ERR_NOT_FOUND;
    for (const mdns_result_t *r = results; r; r = r->next) {
        const char *v = txt_value(r, "proto"), *path = txt_value(r, "path"), *ca = txt_value(r, "ca");
        if (!r->port || !v || strcmp(v,"3") || !path || strcmp(path,"/v3/brain/ws") ||
            !ca || strcmp(ca,ca_sha256)) continue;
        esp_ip4_addr_t resolved = {0};
        const esp_ip4_addr_t *addr = best_ipv4(r, &local);
        if (!addr && r->hostname && mdns_query_a(r->hostname, 600, &resolved) == ESP_OK) addr = &resolved;
        if (!addr || !addr->addr || (addr->addr & local.netmask.addr) != (local.ip.addr & local.netmask.addr)) continue;
        char candidate[16];
        snprintf(candidate,sizeof(candidate),IPSTR,IP2STR(addr));
        if (err == ESP_OK && (*port != r->port || strcmp(ip,candidate))) { err = ESP_ERR_INVALID_RESPONSE; break; }
        snprintf(ip,16,"%s",candidate); *port=r->port; err=ESP_OK;
    }
    if (results) mdns_query_results_free(results);
    return err;
}

esp_err_t bot_lan_discovery_scan(bot_lan_server_t *servers,
                                 size_t capacity, size_t *count) {
    if (!servers || !count || capacity == 0) return ESP_ERR_INVALID_ARG;
    *count = 0;
    mdns_result_t *results = NULL;
    esp_err_t err = mdns_query_ptr("_espbot-server", "_tcp", 1500, 12, &results);
    if (err == ESP_ERR_NOT_FOUND) return ESP_OK;
    if (err != ESP_OK) return err;

    esp_netif_ip_info_t local = {0};
    esp_netif_t *wifi = esp_netif_get_handle_from_ifkey("WIFI_STA_DEF");
    if (wifi) (void)esp_netif_get_ip_info(wifi, &local);

    for (const mdns_result_t *r = results; r && *count < capacity; r = r->next) {
        const char *proto = txt_value(r, "proto");
        const char *id = txt_value(r, "id");
        const char *path = txt_value(r, "path");
        if (!proto || strcmp(proto, "1") != 0 || !id ||
            strlen(id) < 8 || strlen(id) >= sizeof(servers[0].server_id) ||
            !path || strcmp(path, BOT_LAN_WS_PATH) != 0 || r->port == 0) {
            continue;
        }
        const esp_ip4_addr_t *addr = best_ipv4(r, &local);
        if (!addr) continue;

        bool duplicate = false;
        for (size_t i = 0; i < *count; ++i) {
            if (strcmp(servers[i].server_id, id) == 0) {
                duplicate = true;
                break;
            }
        }
        if (duplicate) continue;

        bot_lan_server_t *out = &servers[*count];
        memset(out, 0, sizeof(*out));
        snprintf(out->server_id, sizeof(out->server_id), "%s", id);
        snprintf(out->instance, sizeof(out->instance), "%s",
                 r->instance_name ? r->instance_name : "ESP Bot Server");
        snprintf(out->ip, sizeof(out->ip), IPSTR, IP2STR(addr));
        out->port = r->port;
        (*count)++;
    }

    if (results) mdns_query_results_free(results);
    return ESP_OK;
}

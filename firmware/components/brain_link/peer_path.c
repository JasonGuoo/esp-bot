#include "peer_path.h"
#include "esp_netif.h"
#include "esp_netif_net_stack.h"
#include "lwip/etharp.h"
#include "lwip/netif.h"

/* Accessed only on the lwIP thread. Reuse the STA driver's normal ARP output
 * with a different next hop for this single peer. IP headers, DHCP leases,
 * multicast (Bonjour), other peers, and the Mac routing table are untouched. */
static ip4_addr_t routed_peer;
static err_t peer_output(struct netif *sta, struct pbuf *packet, const ip4_addr_t *dest) {
    const ip4_addr_t *gw=netif_ip4_gw(sta);
    if (ip4_addr_eq(dest,&routed_peer) && !ip4_addr_isany(gw) &&
        ip4_addr_net_eq(dest,netif_ip4_addr(sta),netif_ip4_netmask(sta)))
        return etharp_output(sta,packet,gw);
    return etharp_output(sta,packet,dest);
}

typedef struct { esp_netif_t *handle; const char *peer; bool enable; } request_t;
static esp_err_t apply(void *arg) {
    request_t *r=arg;
    struct netif *sta=esp_netif_get_netif_impl(r->handle);
    if (!sta || (sta->output!=etharp_output && sta->output!=peer_output)) return ESP_ERR_NOT_SUPPORTED;
    sta->output=etharp_output;
    if (!r->enable) return ESP_OK;
    ip4_addr_t peer;
    if (!r->peer || !ip4addr_aton(r->peer,&peer) || ip4_addr_isany(&peer) || ip4_addr_isany(netif_ip4_gw(sta)) ||
        ip4_addr_ismulticast(&peer) || ip4_addr_isbroadcast(&peer,sta) || ip4_addr_islinklocal(&peer) ||
        ip4_addr_eq(&peer,netif_ip4_addr(sta)) || ip4_addr_eq(&peer,netif_ip4_gw(sta)) ||
        !ip4_addr_net_eq(&peer,netif_ip4_addr(sta),netif_ip4_netmask(sta))) return ESP_ERR_INVALID_ARG;
    routed_peer=peer;
    sta->output=peer_output;
    return ESP_OK;
}

bool brain_peer_path(const char *peer_ip, bool via_dhcp_gateway) {
    request_t request={esp_netif_get_handle_from_ifkey("WIFI_STA_DEF"),peer_ip,via_dhcp_gateway};
    return request.handle && esp_netif_tcpip_exec(apply,&request)==ESP_OK;
}

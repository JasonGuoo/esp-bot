use mdns_sd::{ServiceDaemon, ServiceInfo};

use super::protocol::{PROTOCOL_VERSION, SERVICE_TYPE, WS_PATH};

/// Advertise only the local WebSocket transport, never the Python Brain or LLM API.
pub fn advertise(server_id: &str, port: u16) -> Result<ServiceDaemon, String> {
    let suffix = &server_id[..8];
    let instance_name = format!("ESP Bot Server {suffix}");
    let host_name = format!("espbot-{suffix}.local.");
    let props = [
        ("proto", PROTOCOL_VERSION.to_string()),
        ("id", server_id.to_owned()),
        ("path", WS_PATH.to_owned()),
    ];
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &instance_name,
        &host_name,
        "",
        port,
        &props[..],
    )
    .map_err(|e| format!("invalid mDNS advertisement: {e}"))?
    .enable_addr_auto();
    let daemon = ServiceDaemon::new().map_err(|e| format!("mDNS start failed: {e}"))?;
    if let Err(err) = daemon.register(info) {
        let _ = daemon.shutdown();
        return Err(format!("mDNS registration failed: {err}"));
    }
    Ok(daemon)
}

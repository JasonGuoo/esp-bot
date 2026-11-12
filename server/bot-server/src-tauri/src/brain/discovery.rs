use std::{net::SocketAddr, path::Path};
use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use sha2::{Digest, Sha256};

pub const SERVICE: &str = "_espbot-brain._tcp.local.";

pub struct Advertisement { daemon: ServiceDaemon, fullname: String }
impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

pub fn advertise(dir: &Path, ca: &Path, bind: SocketAddr) -> Result<Advertisement, String> {
    let id = crate::identity::load_or_create_server_id(dir)?;
    let ca = CertificateDer::from_pem_file(ca).map_err(|_| "bonjour_pairing_ca_unavailable")?;
    let fingerprint = format!("{:x}", Sha256::digest(ca.as_ref()));
    let host = format!("xiaodai-{}.local.", id.to_ascii_lowercase().replace('_', "-"));
    let instance = format!("Xiaodai Brain {}", &id[..8]);
    let properties = [("proto","3"),("path",super::gateway::PATH),("ca",fingerprint.as_str())];
    let ip = if bind.ip().is_unspecified() { String::new() } else { bind.ip().to_string() };
    let info = ServiceInfo::new(SERVICE,&instance,&host,ip.as_str(),bind.port(),&properties[..])
        .map_err(|e|format!("bonjour_record: {e}"))?;
    let info = if ip.is_empty() {info.enable_addr_auto()} else {info};
    let daemon=ServiceDaemon::new().map_err(|e|format!("bonjour_start: {e}"))?;
    let advertisement=Advertisement { daemon, fullname:info.get_fullname().to_owned() };
    if bind.is_ipv4() { advertisement.daemon.disable_interface(IfKind::IPv6).map_err(|e|e.to_string())?; }
    advertisement.daemon.disable_interface(IfKind::LoopbackV4).map_err(|e|e.to_string())?;
    advertisement.daemon.register(info).map_err(|e|format!("bonjour_register: {e}"))?;
    Ok(advertisement)
}

//! Physical provisioning is separate from LAN discovery. Per-device random
//! credentials are issued only by the local console and used ONLY over TLS.
use std::{collections::BTreeMap, path::PathBuf, sync::{Arc, RwLock}};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use super::settings;

#[derive(Clone, Serialize, Deserialize)]
struct DeviceCredential {
    token_sha256: String,
    created_at_ms: u64,
}

pub struct Credentials {
    path: PathBuf,
    records: RwLock<BTreeMap<String, DeviceCredential>>,
    changed: watch::Sender<u64>,
}

impl Credentials {
    pub fn open(dir: PathBuf) -> Result<Arc<Self>, String> {
        settings::private_dir(&dir)?;
        let path = dir.join("secure-devices.json");
        let records = if path.exists() {
            serde_json::from_slice(&std::fs::read(&path).map_err(|e|e.to_string())?).map_err(|e|format!("device_credentials_invalid: {e}"))?
        } else { BTreeMap::new() };
        let (changed, _) = watch::channel(0);
        Ok(Arc::new(Self { path, records: RwLock::new(records), changed }))
    }

    pub fn issue(&self, device_id: &str) -> Result<Value, String> {
        if !valid_id(device_id) { return Err("device_id_invalid".into()); }
        let token = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let record = DeviceCredential { token_sha256: digest(&token), created_at_ms: crate::lan::now_ms() };
        let mut records = self.records.write().map_err(|_|"device_credentials_poisoned")?;
        if records.len() >= 64 && !records.contains_key(device_id) { return Err("device_credentials_capacity".into()); }
        let mut next = records.clone();
        next.insert(device_id.to_owned(), record);
        settings::private_write(&self.path, &serde_json::to_vec_pretty(&next).map_err(|e|e.to_string())?)?;
        *records = next;
        self.changed.send_modify(|version| *version = version.wrapping_add(1));
        // Returned ONCE to the parent console; never emit it as telemetry.
        Ok(json!({"device_id":device_id,"token":token,"transport":"wss","requires_certificate_trust":true}))
    }

    pub fn revoke(&self, device_id: &str) -> Result<(), String> {
        let mut records = self.records.write().map_err(|_|"device_credentials_poisoned")?;
        let mut next = records.clone(); next.remove(device_id);
        settings::private_write(&self.path, &serde_json::to_vec_pretty(&next).map_err(|e|e.to_string())?)?;
        *records = next;
        self.changed.send_modify(|version| *version = version.wrapping_add(1));
        Ok(())
    }

    pub fn list(&self) -> Result<Value, String> {
        let records = self.records.read().map_err(|_|"device_credentials_poisoned")?;
        Ok(json!(records.iter().map(|(id,r)|json!({"device_id":id,"created_at_ms":r.created_at_ms})).collect::<Vec<_>>()))
    }

    pub fn authenticate(&self, device_id: &str, token: &str) -> bool {
        if !valid_id(device_id) || token.len() != 64 { return false; }
        self.records.read().ok().and_then(|records| records.get(device_id).cloned()).is_some_and(|record|
            record.token_sha256.as_bytes().ct_eq(digest(token).as_bytes()).into())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> { self.changed.subscribe() }
}

fn digest(token: &str) -> String { Sha256::digest(token.as_bytes()).iter().map(|b|format!("{b:02x}")).collect() }
pub fn valid_id(id: &str) -> bool { !id.is_empty() && id.len() <= 64 && id.bytes().all(|c|c.is_ascii_alphanumeric() || c == b'-' || c == b'_') }

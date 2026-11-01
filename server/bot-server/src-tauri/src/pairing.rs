//! Device pairing registry — a trusted-LAN soft gate, NOT authentication.
//!
//! The v1 firmware hello carries no token (`{type,protocol,device_id,name}`
//! is fixed), so a device id is self-claimed and pairing is server-side
//! knowledge only: unknown devices connect but never receive commands
//! until approved in the console. A persistent keypair and an authenticated
//! encrypted channel arrive with the audio round (docs/local-first-hybrid-
//! brain.md §4); until then this file honestly does not claim crypto.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::lan::{EventSink, LanEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    Pending,
    Approved,
    Denied,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub state: PairState,
    pub name: String,
    pub first_seen_ms: u64,
    pub approved_at_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    devices: BTreeMap<String, DeviceRecord>,
}

pub struct PairingStore {
    path: PathBuf,
    sink: EventSink,
    inner: Mutex<BTreeMap<String, DeviceRecord>>,
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

impl PairingStore {
    pub fn new(dir: &Path, sink: EventSink) -> Self {
        let path = dir.join("devices.json");
        let devices = load_file(&path).unwrap_or_default();
        Self { path, sink, inner: Mutex::new(devices) }
    }

    /// First-seen devices are recorded as pending and reported to the
    /// console; known devices return their stored state without events.
    pub fn classify(&self, device_id: &str, name: &str) -> PairState {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(record) = inner.get(device_id) {
            return record.state;
        }
        let record = DeviceRecord {
            state: PairState::Pending,
            name: name.to_owned(),
            first_seen_ms: now_ms(),
            approved_at_ms: None,
        };
        inner.insert(device_id.to_owned(), record);
        let snapshot = inner.clone();
        drop(inner);
        let _ = self.persist(&snapshot);
        (self.sink)(LanEvent::new(
            "pairing_pending",
            Some(device_id),
            "new device waiting for approval",
            Some(serde_json::json!({ "device_id": device_id, "name": name })),
        ));
        PairState::Pending
    }

    pub fn approve(&self, device_id: &str) -> Result<(), String> {
        self.transition(device_id, PairState::Approved)?;
        (self.sink)(LanEvent::new("pairing_resolved", Some(device_id), "approved", None));
        Ok(())
    }

    pub fn deny(&self, device_id: &str) -> Result<(), String> {
        self.transition(device_id, PairState::Denied)?;
        (self.sink)(LanEvent::new("pairing_resolved", Some(device_id), "denied", None));
        Ok(())
    }

    /// Removes the record entirely so the device starts over as pending.
    pub fn remove(&self, device_id: &str) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.remove(device_id).is_none() {
            return Err(format!("unknown device: {device_id}"));
        }
        let snapshot = inner.clone();
        drop(inner);
        self.persist(&snapshot)
    }

    pub fn is_approved(&self, device_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(device_id)
            .is_some_and(|record| record.state == PairState::Approved)
    }

    /// Console listing; sorted by device id through BTreeMap ordering.
    pub fn list(&self) -> Vec<(String, DeviceRecord)> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(id, record)| (id.clone(), record.clone()))
            .collect()
    }

    fn transition(&self, device_id: &str, state: PairState) -> Result<(), String> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let record = inner
            .get_mut(device_id)
            .ok_or_else(|| format!("unknown device: {device_id}"))?;
        record.state = state;
        if state == PairState::Approved {
            record.approved_at_ms = Some(now_ms());
        }
        let snapshot = inner.clone();
        drop(inner);
        self.persist(&snapshot)
    }

    /// Atomic write (tmp + rename) so a crash never leaves half a registry.
    fn persist(&self, devices: &BTreeMap<String, DeviceRecord>) -> Result<(), String> {
        let file = StoreFile { version: 1, devices: devices.clone() };
        let json = serde_json::to_string_pretty(&file)
            .map_err(|e| format!("cannot serialize devices.json: {e}"))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("cannot create config dir: {e}"))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, format!("{json}\n"))
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        fs::rename(&tmp, &self.path)
            .map_err(|e| format!("cannot replace {}: {e}", self.path.display()))
    }
}

fn load_file(path: &Path) -> Option<BTreeMap<String, DeviceRecord>> {
    let text = fs::read_to_string(path).ok()?;
    let file: StoreFile = serde_json::from_str(&text).ok()?;
    (file.version == 1).then_some(file.devices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use uuid::Uuid;

    fn sink() -> EventSink {
        static EVENTS: AtomicUsize = AtomicUsize::new(0);
        Arc::new(|event: LanEvent| {
            assert!(["pairing_pending", "pairing_resolved"].contains(&event.kind.as_str()));
            EVENTS.fetch_add(1, Ordering::SeqCst);
        })
    }

    fn temp_store() -> (PathBuf, PairingStore) {
        let dir = std::env::temp_dir().join(format!("espbot-pairing-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = PairingStore::new(&dir, sink());
        (dir, store)
    }

    #[test]
    fn first_seen_is_pending_then_transitions() {
        let (_dir, store) = temp_store();
        assert_eq!(store.classify("korvo-01", "Desk Bot"), PairState::Pending);
        assert!(!store.is_approved("korvo-01"));

        store.approve("korvo-01").unwrap();
        assert!(store.is_approved("korvo-01"));
        assert_eq!(store.classify("korvo-01", "Desk Bot"), PairState::Approved);

        store.deny("korvo-01").unwrap();
        assert_eq!(store.classify("korvo-01", "Desk Bot"), PairState::Denied);
    }

    #[test]
    fn unknown_device_transition_fails() {
        let (_dir, store) = temp_store();
        assert!(store.approve("ghost").unwrap_err().contains("unknown device"));
    }

    #[test]
    fn state_survives_restart() {
        let (dir, store) = temp_store();
        store.classify("korvo-01", "Desk Bot");
        store.approve("korvo-01").unwrap();
        store.classify("korvo-02", "Other Bot");

        let reopened = PairingStore::new(&dir, sink());
        assert!(reopened.is_approved("korvo-01"));
        assert_eq!(reopened.classify("korvo-02", "Other Bot"), PairState::Pending);
        assert!(reopened.list().len() == 2);
    }

    #[test]
    fn remove_resets_device_to_pending() {
        let (_dir, store) = temp_store();
        store.classify("korvo-01", "Desk Bot");
        store.approve("korvo-01").unwrap();
        store.remove("korvo-01").unwrap();
        assert!(!store.is_approved("korvo-01"));
        assert_eq!(store.classify("korvo-01", "Desk Bot"), PairState::Pending);
    }

    #[test]
    fn corrupt_registry_starts_empty_not_panicking() {
        let dir = std::env::temp_dir().join(format!("espbot-pairing-{}", Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("devices.json"), "{ broken").unwrap();
        let store = PairingStore::new(&dir, sink());
        assert!(store.list().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

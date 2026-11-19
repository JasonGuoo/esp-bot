//! Ephemeral, authenticated per-device controls. No offline command queue.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU32, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

const FRESH: Duration = Duration::from_secs(8);
const DEADLINE: Duration = Duration::from_secs(12);

#[derive(Clone)]
struct Pending {
    id: u32,
    volume: Option<u8>,
    muted: Option<bool>,
    created: Instant,
}
struct State {
    connection: String,
    volume: u8,
    muted: bool,
    settings_open: bool,
    applied: bool,
    last_seen: Instant,
    last_id: u32,
    pending: Option<Pending>,
    result: &'static str,
    firmware: Option<Value>,
}

#[derive(Default)]
pub(super) struct DeviceVolumes {
    states: Mutex<BTreeMap<String, State>>,
    next: AtomicU32,
}

fn firmware_report(report: &Value) -> Option<Value> {
    let value = report.get("firmware")?;
    let release = value["release"].as_str().filter(|s| !s.is_empty() && s.len() <= 31 &&
        s.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'_' | b'+')))?;
    let digest = value["elf_sha256"].as_str().filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))?;
    Some(json!({"release":release,"elf_sha256":digest}))
}

impl DeviceVolumes {
    fn expire(state: &mut State) {
        if state
            .pending
            .as_ref()
            .is_some_and(|p| p.created.elapsed() > DEADLINE)
        {
            state.pending = None;
            state.result = "timed_out";
        }
    }
    fn value(id: &str, state: &State) -> Value {
        json!({"device_id":id,"volume":state.volume,"muted":state.muted,
            "settings_open":state.settings_open,"hardware_applied":state.applied,
            "online":state.last_seen.elapsed()<=FRESH,"pending":state.pending.is_some(),"result":state.result,
            "firmware":state.firmware})
    }
    pub fn status(&self) -> Vec<Value> {
        let Ok(mut states) = self.states.lock() else {
            return vec![];
        };
        states
            .iter_mut()
            .map(|(id, state)| {
                Self::expire(state);
                Self::value(id, state)
            })
            .collect()
    }
    pub fn set(
        &self,
        device: &str,
        volume: Option<u8>,
        muted: Option<bool>,
    ) -> Result<Value, String> {
        if volume.is_none() && muted.is_none() {
            return Err("volume_change_required".into());
        }
        if volume.is_some_and(|v| v > 60) {
            return Err("volume_out_of_range".into());
        }
        let mut states = self.states.lock().map_err(|_| "device_volume_lock")?;
        let state = states.get_mut(device).ok_or("device_volume_unavailable")?;
        Self::expire(state);
        if state.last_seen.elapsed() > FRESH {
            return Err("device_volume_unavailable".into());
        }
        if state.settings_open {
            return Err("device_settings_open".into());
        }
        if state.pending.is_some() {
            return Err("device_volume_busy".into());
        }
        if state.applied
            && volume.is_none_or(|v| v == state.volume)
            && muted.is_none_or(|v| v == state.muted)
        {
            state.result = "applied";
            return Ok(Self::value(device, state));
        }
        let mut id = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        if id == 0 || id == state.last_id {
            id = self.next.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        }
        if id == 0 || id == state.last_id {
            return Err("device_volume_id_exhausted".into());
        }
        state.pending = Some(Pending {
            id,
            volume,
            muted,
            created: Instant::now(),
        });
        state.result = "pending";
        Ok(Self::value(device, state))
    }
    pub fn observe(
        &self,
        device: &str,
        connection: &str,
        report: &Value,
    ) -> Result<(Value, bool), String> {
        let volume = report["volume"]
            .as_u64()
            .filter(|v| *v <= 60)
            .ok_or("device_volume_invalid")? as u8;
        let muted = report["muted"].as_bool().ok_or("device_volume_invalid")?;
        let open = report["settings_open"]
            .as_bool()
            .ok_or("device_volume_invalid")?;
        let applied = report["hardware_applied"]
            .as_bool()
            .ok_or("device_volume_invalid")?;
        let last = report["last_id"]
            .as_u64()
            .filter(|v| *v <= u32::MAX as u64)
            .ok_or("device_volume_invalid")? as u32;
        let result = report["last_result"]
            .as_u64()
            .filter(|v| *v <= 2)
            .ok_or("device_volume_invalid")?;
        let firmware = firmware_report(report);
        let mut states = self.states.lock().map_err(|_| "device_volume_lock")?;
        let new_connection = states
            .get(device)
            .is_none_or(|s| s.connection != connection);
        let state = states.entry(device.to_owned()).or_insert_with(|| State {
            connection: connection.into(),
            volume,
            muted,
            settings_open: open,
            applied,
            last_seen: Instant::now(),
            last_id: last,
            pending: None,
            result: "synced",
            firmware: firmware.clone(),
        });
        if state.connection != connection {
            *state = State {
                connection: connection.into(),
                volume,
                muted,
                settings_open: open,
                applied,
                last_seen: Instant::now(),
                last_id: last,
                pending: None,
                result: "synced",
                firmware: firmware.clone(),
            };
        }
        let before = Self::value(device, state);
        Self::expire(state);
        state.volume = volume;
        state.muted = muted;
        state.settings_open = open;
        state.applied = applied;
        state.last_seen = Instant::now();
        state.last_id = last;
        state.firmware = firmware;
        if let Some(pending) = &state.pending {
            if last == pending.id && result == 2 {
                state.pending = None;
                state.result = "rejected";
            } else if last == pending.id
                && result == 1
                && applied
                && pending.volume.is_none_or(|v| v == volume)
                && pending.muted.is_none_or(|v| v == muted)
            {
                state.pending = None;
                state.result = "applied";
            }
        }
        let command = state
            .pending
            .as_ref()
            .map(|p| json!({"id":p.id,"volume":p.volume,"muted":p.muted}));
        Ok((
            json!({"command":command}),
            new_connection || before != Self::value(device, state),
        ))
    }
    pub fn disconnect(&self, device: &str, connection: &str) -> bool {
        let Ok(mut states) = self.states.lock() else {
            return false;
        };
        if states
            .get(device)
            .is_some_and(|s| s.connection == connection)
        {
            states.remove(device);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(volume: u8, muted: bool, open: bool, applied: bool, id: u32, result: u8) -> Value {
        json!({"volume":volume,"muted":muted,"settings_open":open,"hardware_applied":applied,"last_id":id,"last_result":result})
    }
    #[test]
    fn requires_device_confirmation_and_never_replays_after_reconnect() {
        let controls = DeviceVolumes::default();
        assert!(controls.set("bot", Some(45), None).is_err());
        controls
            .observe("bot", "first", &report(30, false, false, true, 0, 0))
            .unwrap();
        assert_eq!(
            controls.set("bot", Some(45), None).unwrap()["result"],
            "pending"
        );
        let (reply, _) = controls
            .observe("bot", "first", &report(30, false, false, true, 0, 0))
            .unwrap();
        let id = reply["command"]["id"].as_u64().unwrap() as u32;
        controls
            .observe("bot", "first", &report(45, false, false, false, id, 1))
            .unwrap();
        assert_eq!(controls.status()[0]["pending"], true);
        controls
            .observe("bot", "first", &report(45, false, false, true, id, 1))
            .unwrap();
        assert_eq!(controls.status()[0]["result"], "applied");
        controls.set("bot", None, Some(true)).unwrap();
        let (reply, _) = controls
            .observe("bot", "second", &report(45, false, false, true, 0, 0))
            .unwrap();
        assert!(reply["command"].is_null());
        assert!(!controls.disconnect("bot", "first"));
        assert!(controls.disconnect("bot", "second"));
    }
    #[test]
    fn firmware_version_is_bound_to_the_live_connection() {
        let controls = DeviceVolumes::default();
        let mut old = report(17,false,false,true,0,0);
        controls.observe("bot","old",&old).unwrap();
        assert!(controls.status()[0]["firmware"].is_null());
        old["firmware"] = json!({"release":"0.2.0","elf_sha256":"a".repeat(64)});
        controls.observe("bot","new",&old).unwrap();
        assert_eq!(controls.status()[0]["firmware"]["release"],"0.2.0");
        old["firmware"]["elf_sha256"] = json!("broken");
        controls.observe("bot","next",&old).unwrap();
        assert!(controls.status()[0]["firmware"].is_null());
    }

    #[test]
    fn refuses_local_settings_and_rejects_invalid_reports() {
        let controls = DeviceVolumes::default();
        controls
            .observe("bot", "first", &report(30, false, true, true, 0, 0))
            .unwrap();
        assert_eq!(
            controls.set("bot", Some(40), None).unwrap_err(),
            "device_settings_open"
        );
        assert!(controls
            .observe("bot", "first", &report(61, false, false, true, 0, 0))
            .is_err());
        assert!(controls.set("bot", Some(61), None).is_err());
    }
}

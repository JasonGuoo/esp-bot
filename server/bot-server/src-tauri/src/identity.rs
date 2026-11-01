//! Persistent server identity.
//!
//! The firmware matches the advertised mDNS TXT `id` against the
//! `welcome.server_id` frame, so the id must stay stable across restarts;
//! an ephemeral per-boot UUID would look like a new server every launch.
//!
//! No keypair is persisted yet: nothing on the wire is authenticated this
//! round. A persistent key arrives together with channel encryption (see
//! docs/local-first-hybrid-brain.md).

use std::{fs, path::Path};
use uuid::Uuid;

/// Firmware accepts 8..=47 chars of `[A-Za-z0-9_-]` for the discovered id.
fn valid_server_id(id: &str) -> bool {
    (8..=47).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Reads `server_id.txt` from the config dir, creating it on first run.
///
/// An existing but invalid file is regenerated rather than trusted: a
/// malformed id would silently break the firmware-side id match on every
/// reconnect.
pub fn load_or_create_server_id(dir: &Path) -> Result<String, String> {
    let path = dir.join("server_id.txt");
    if let Ok(text) = fs::read_to_string(&path) {
        let id = text.trim().to_owned();
        if valid_server_id(&id) {
            return Ok(id);
        }
    }
    let id = Uuid::new_v4().simple().to_string();
    fs::create_dir_all(dir).map_err(|e| format!("cannot create config dir {}: {e}", dir.display()))?;
    fs::write(&path, format!("{id}\n"))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir() -> std::path::PathBuf {
        std::env::temp_dir().join(format!("espbot-identity-{}", Uuid::new_v4().simple()))
    }

    #[test]
    fn creates_then_reuses_stable_id() {
        let dir = temp_dir();
        let first = load_or_create_server_id(&dir).unwrap();
        let second = load_or_create_server_id(&dir).unwrap();
        assert_eq!(first, second);
        assert!(valid_server_id(&first));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn regenerates_invalid_stored_id() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("server_id.txt"), "not a valid id!!").unwrap();
        let id = load_or_create_server_id(&dir).unwrap();
        assert!(valid_server_id(&id));
        assert_ne!(id, "not a valid id!!");
        let _ = fs::remove_dir_all(&dir);
    }
}

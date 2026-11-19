//! Identity of the executable actually running, independent of the source checkout.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read, sync::OnceLock};

pub fn info() -> &'static Value {
    static INFO: OnceLock<Value> = OnceLock::new();
    INFO.get_or_init(|| {
        let executable_sha256 = std::env::current_exe().ok().and_then(|path| {
            let mut file = File::open(path).ok()?;
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 65536];
            loop {
                let read = file.read(&mut buffer).ok()?;
                if read == 0 { break; }
                hash.update(&buffer[..read]);
            }
            Some(format!("{:x}", hash.finalize()))
        });
        json!({
            "release": env!("CARGO_PKG_VERSION"),
            "source_revision": env!("ESPBOT_SOURCE_REVISION"),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "executable_sha256": executable_sha256,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifies_the_running_executable() {
        let value = info();
        assert_eq!(value["release"], env!("CARGO_PKG_VERSION"));
        assert!(value["executable_sha256"].as_str().is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())));
        assert!(!value["source_revision"].as_str().unwrap().is_empty());
    }
}

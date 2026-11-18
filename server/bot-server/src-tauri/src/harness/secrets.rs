//! Model keys live only as long as this console process. Do not consult or
//! migrate legacy Keychain items, even for status. Devices have separate credentials.
use std::{collections::BTreeMap, sync::RwLock};
use zeroize::Zeroizing;

#[derive(Default)]
pub(crate) struct SessionKeys(RwLock<BTreeMap<String, Zeroizing<String>>>);

impl SessionKeys {
    pub(crate) fn set(&self, account: &str, key: Option<Zeroizing<String>>) -> Result<(), String> {
        if let Some(key) = &key { validate_key(key)?; }
        let mut values = self.0.write().map_err(|_| "api_key_session_unavailable")?;
        match key {
            Some(key) => { values.insert(account.to_owned(), key); }
            None => { values.remove(account); }
        }
        Ok(())
    }

    pub(super) fn load(&self, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        Ok(self.0.read().map_err(|_| "api_key_session_unavailable")?.get(account).cloned())
    }

    pub(super) fn contains(&self, account: &str) -> bool {
        self.0.read().is_ok_and(|values| values.contains_key(account))
    }
}

pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 4096 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("api_key_invalid: enter 1..4096 printable ASCII characters without spaces".into());
    }
    Ok(())
}

pub fn environment_present(account: &str) -> bool {
    std::env::var_os(account).is_some_and(|key| !key.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_keys_are_isolated_and_invalid_replacement_keeps_the_original() {
        let session = SessionKeys::default();
        session.set("TEST_KEY", Some(Zeroizing::new("test-value".to_owned()))).unwrap();
        for invalid in ["", "line\ninjected", "with space", "密钥", &"x".repeat(4097)] {
            assert!(session.set("TEST_KEY", Some(Zeroizing::new(invalid.to_owned()))).is_err());
        }
        assert_eq!(session.load("TEST_KEY").unwrap().unwrap().as_str(), "test-value");
        assert!(!SessionKeys::default().contains("TEST_KEY"));
        session.set("TEST_KEY", None).unwrap();
        assert!(!session.contains("TEST_KEY"));
    }
}

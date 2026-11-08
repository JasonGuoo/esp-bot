//! Provider keys stay in the Mac login Keychain, outside JSON and status output.
//! Only a managed OpenCode child's environment receives the saved values.
use zeroize::Zeroizing;

const SERVICE: &str = "ai.reasonia.espbot.server.provider-api-keys";

pub fn validate_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > 4096 || !key.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err("api_key_invalid: enter 1..4096 printable ASCII characters without spaces".into());
    }
    Ok(())
}

pub fn environment_present(account: &str) -> bool {
    std::env::var(account).is_ok_and(|key| !key.trim().is_empty())
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use security_framework::{item::{ItemClass, ItemSearchOptions}, passwords};

    const NOT_FOUND: i32 = -25300;

    fn error(operation: &str, code: i32) -> String {
        // OS status only: never include the key, a process command or IPC input.
        format!("api_key_keychain_{operation}: macOS status {code}; unlock the login Keychain and retry")
    }

    pub(super) fn exists(service: &str, account: &str) -> Result<bool, String> {
        // Metadata-only lookup. Polling status must not read secret data.
        match ItemSearchOptions::new().class(ItemClass::generic_password())
            .service(service).account(account).case_insensitive(Some(false))
            .load_attributes(true).limit(1).search()
        {
            Ok(items) => Ok(!items.is_empty()),
            Err(err) if err.code() == NOT_FOUND => Ok(false),
            Err(err) => Err(error("status", err.code())),
        }
    }

    pub(super) fn load(service: &str, account: &str) -> Result<Option<Zeroizing<String>>, String> {
        match passwords::generic_password(passwords::PasswordOptions::new_generic_password(service, account)) {
            Ok(bytes) => {
                let bytes = Zeroizing::new(bytes);
                let text = std::str::from_utf8(&bytes).map_err(|_| "api_key_keychain_invalid_encoding")?;
                validate_key(text)?;
                Ok(Some(Zeroizing::new(text.to_owned())))
            }
            Err(err) if err.code() == NOT_FOUND => Ok(None),
            Err(err) => Err(error("read", err.code())),
        }
    }

    pub(super) fn save(service: &str, account: &str, key: &str) -> Result<(), String> {
        validate_key(key)?;
        passwords::set_generic_password(service, account, key.as_bytes())
            .map_err(|err| error("save", err.code()))
    }

    pub(super) fn delete(service: &str, account: &str) -> Result<(), String> {
        match passwords::delete_generic_password(service, account) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == NOT_FOUND => Ok(()),
            Err(err) => Err(error("delete", err.code())),
        }
    }
}

pub fn exists(account: &str) -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    { platform::exists(SERVICE, account) }
    #[cfg(not(target_os = "macos"))]
    { let _ = account; Ok(false) }
}

pub fn load(account: &str) -> Result<Option<Zeroizing<String>>, String> {
    #[cfg(target_os = "macos")]
    { platform::load(SERVICE, account) }
    #[cfg(not(target_os = "macos"))]
    { let _ = account; Ok(None) }
}

pub fn save(account: &str, key: &str) -> Result<(), String> {
    validate_key(key)?;
    #[cfg(target_os = "macos")]
    { platform::save(SERVICE, account, key) }
    #[cfg(not(target_os = "macos"))]
    { let _ = account; Err("api_key_storage_requires_macos: use environment variables on this platform".into()) }
}

pub fn delete(account: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    { platform::delete(SERVICE, account) }
    #[cfg(not(target_os = "macos"))]
    { let _ = account; Err("api_key_storage_requires_macos".into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_keys_that_cannot_be_used_as_http_credentials() {
        for key in ["", "sk-test\nInjected: value", "sk test", "密钥"] {
            assert!(validate_key(key).is_err());
        }
        assert!(validate_key(&"a".repeat(4097)).is_err());
        assert!(validate_key("sk-test-only-1234567890").is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "explicit Mac login Keychain smoke check; uses an isolated disposable service"]
    fn mac_keychain_roundtrip() {
        let service = format!("{SERVICE}.smoke.{}", uuid::Uuid::new_v4());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) { let _ = platform::delete(&self.0, "test-account"); }
        }
        let _cleanup = Cleanup(service.clone());
        assert!(!platform::exists(&service, "test-account").unwrap());
        platform::save(&service, "test-account", "sk-test-only-first").unwrap();
        assert!(platform::exists(&service, "test-account").unwrap());
        assert_eq!(platform::load(&service, "test-account").unwrap().as_deref().map(String::as_str), Some("sk-test-only-first"));
        platform::save(&service, "test-account", "sk-test-only-replacement").unwrap();
        assert_eq!(platform::load(&service, "test-account").unwrap().as_deref().map(String::as_str), Some("sk-test-only-replacement"));
        platform::delete(&service, "test-account").unwrap();
        assert!(!platform::exists(&service, "test-account").unwrap());
        assert!(platform::load(&service, "test-account").unwrap().is_none());
    }
}

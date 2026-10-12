//! macOS saved-session credential. The standard Keychain item permits an already
//! unlocked desktop to resume the session. The old Touch ID item is read only
//! to migrate sessions saved by the previous release.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::base::Error;
use security_framework::passwords::{
    PasswordOptions, delete_generic_password_options, generic_password,
};
use security_framework_sys::base::{errSecItemNotFound, errSecUnimplemented};

type KeychainResult<T> = security_framework::base::Result<T>;

trait CredentialStore {
    fn read_protected(&self, username: &str) -> KeychainResult<Vec<u8>>;
    fn read_legacy(&self, username: &str) -> Option<String>;
    fn save_legacy(&self, username: &str, password: &str) -> Result<(), String>;
}

struct MacKeychain;

impl CredentialStore for MacKeychain {
    fn read_protected(&self, username: &str) -> KeychainResult<Vec<u8>> {
        generic_password(options(username).ok_or_else(|| Error::from_code(errSecUnimplemented))?)
    }

    fn read_legacy(&self, username: &str) -> Option<String> {
        keyring::Entry::new(LEGACY_SERVICE, username)
            .ok()?
            .get_password()
            .ok()
    }

    fn save_legacy(&self, username: &str, password: &str) -> Result<(), String> {
        keyring::Entry::new(LEGACY_SERVICE, username)
            .map_err(|error| error.to_string())?
            .set_password(password)
            .map_err(|error| error.to_string())
    }
}

const SERVICE: &str = "mesh-talk-touch-id";
const LEGACY_SERVICE: &str = "mesh-talk";

fn options(username: &str) -> Option<PasswordOptions> {
    let mut options = PasswordOptions::new_generic_password(SERVICE, username);
    // This key was added in macOS 10.15, while the app still supports 10.13.
    // Resolve it at runtime so an older Mac can launch and use the password form.
    let symbol = unsafe {
        libc::dlsym(
            libc::RTLD_DEFAULT,
            c"kSecUseDataProtectionKeychain".as_ptr(),
        )
    };
    if symbol.is_null() {
        return None;
    }
    // SAFETY: the exported Security.framework symbol is a CFStringRef variable.
    // dlsym returns its address; CFString's get rule retains the system-owned value.
    let key = unsafe { *(symbol as *const CFStringRef) };
    if key.is_null() {
        return None;
    }
    #[allow(deprecated)]
    options.query.push((
        unsafe { CFString::wrap_under_get_rule(key) },
        CFBoolean::from(true).into_CFType(),
    ));
    Some(options)
}

fn clear_legacy(username: &str) {
    let Ok(entry) = keyring::Entry::new(LEGACY_SERVICE, username) else {
        return;
    };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => log::warn!("failed to remove old saved-session credential: {error}"),
    }
}

fn clear_protected(username: &str) {
    if let Some(options) = options(username) {
        match delete_generic_password_options(options) {
            Ok(()) => {}
            Err(error) if error.code() == errSecItemNotFound => {}
            Err(error) => log::warn!("failed to remove Touch ID credential: {error}"),
        }
    }
}

/// Save the raw login password in the OS Keychain for the next launch.
pub fn save(username: &str, password: &str) {
    save_to(&MacKeychain, username, password);
}

fn save_to(store: &impl CredentialStore, username: &str, password: &str) {
    if let Err(error) = store.save_legacy(username, password) {
        log::warn!("failed to save stay-signed-in credential: {error}");
    }
}

/// Read a standard Keychain item without prompting. An old Touch ID item can
/// be migrated once after a successful biometric read.
pub fn load(username: &str) -> Option<String> {
    load_from(&MacKeychain, username)
}

fn load_from(store: &impl CredentialStore, username: &str) -> Option<String> {
    if let Some(password) = store.read_legacy(username) {
        return Some(password);
    }
    let bytes = match store.read_protected(username) {
        Ok(bytes) => bytes,
        Err(error) if error.code() == errSecItemNotFound => return None,
        Err(error) => {
            log::info!("Touch ID credential was not unlocked during migration: {error}");
            return None;
        }
    };
    let password = String::from_utf8(bytes).ok()?;
    save_to(store, username, &password);
    Some(password)
}

/// Delete both the standard item and the biometric unlock item.
pub fn clear(username: &str) {
    clear_protected(username);
    clear_legacy(username);
}

#[cfg(test)]
mod tests {
    use super::*;
    use security_framework::base::Error;
    use security_framework_sys::base::errSecAuthFailed;
    use std::cell::{Cell, RefCell};

    struct FakeStore {
        protected: RefCell<Option<Vec<u8>>>,
        legacy: RefCell<Option<String>>,
        deny_save: bool,
        deny_read_on: Option<usize>,
        reads: Cell<usize>,
        legacy_reads: Cell<usize>,
    }

    impl CredentialStore for FakeStore {
        fn read_protected(&self, _username: &str) -> KeychainResult<Vec<u8>> {
            self.reads.set(self.reads.get() + 1);
            if self.deny_read_on == Some(self.reads.get()) {
                return Err(Error::from_code(errSecAuthFailed));
            }
            self.protected
                .borrow()
                .clone()
                .ok_or_else(|| Error::from_code(errSecItemNotFound))
        }

        fn read_legacy(&self, _username: &str) -> Option<String> {
            self.legacy_reads.set(self.legacy_reads.get() + 1);
            self.legacy.borrow().clone()
        }

        fn save_legacy(&self, _username: &str, password: &str) -> Result<(), String> {
            if self.deny_save {
                return Err("Keychain unavailable".into());
            }
            *self.legacy.borrow_mut() = Some(password.into());
            Ok(())
        }
    }

    fn fake(legacy: Option<&str>, protected: Option<&str>) -> FakeStore {
        FakeStore {
            protected: RefCell::new(protected.map(|value| value.as_bytes().to_vec())),
            legacy: RefCell::new(legacy.map(str::to_owned)),
            deny_save: false,
            deny_read_on: None,
            reads: Cell::new(0),
            legacy_reads: Cell::new(0),
        }
    }

    #[test]
    fn old_touch_id_session_migrates_after_successful_read() {
        let store = fake(None, Some("raw password"));
        assert_eq!(load_from(&store, "alice"), Some("raw password".into()));
        assert_eq!(store.reads.get(), 1);
        assert_eq!(store.legacy.borrow().as_deref(), Some("raw password"));
        assert_eq!(
            store.protected.borrow().as_deref(),
            Some(b"raw password".as_slice())
        );
    }

    #[test]
    fn canceled_old_touch_id_session_falls_back_to_manual_login() {
        let mut store = fake(None, Some("raw password"));
        store.deny_read_on = Some(1);
        assert_eq!(load_from(&store, "alice"), None);
        assert!(store.legacy.borrow().is_none());
    }

    #[test]
    fn failed_save_keeps_old_touch_id_credential() {
        let mut store = fake(None, Some("raw password"));
        store.deny_save = true;
        assert_eq!(load_from(&store, "alice"), Some("raw password".into()));
        assert_eq!(
            store.protected.borrow().as_deref(),
            Some(b"raw password".as_slice())
        );
    }

    #[test]
    fn existing_standard_keychain_session_resumes_without_biometric_read() {
        let mut store = fake(Some("raw password"), None);
        store.deny_read_on = Some(1);
        assert_eq!(load_from(&store, "alice"), Some("raw password".into()));
        assert_eq!(store.reads.get(), 0);
    }

    #[test]
    fn signing_in_saves_session_without_biometric_enrollment() {
        let store = fake(None, None);
        save_to(&store, "alice", "raw password");
        assert_eq!(load_from(&store, "alice"), Some("raw password".into()));
        assert_eq!(store.reads.get(), 0);
    }
}

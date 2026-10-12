//! macOS saved-session credential. Keychain enforces Touch ID on every read;
//! the app never treats a standalone biometric success as permission to read an
//! unprotected password.

use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::base::Error;
use security_framework::passwords::{
    AccessControlOptions, PasswordOptions, delete_generic_password_options, generic_password,
    set_generic_password_options,
};
use security_framework_sys::base::{errSecItemNotFound, errSecUnimplemented};

type KeychainResult<T> = security_framework::base::Result<T>;

trait CredentialStore {
    fn read_protected(&self, username: &str) -> KeychainResult<Vec<u8>>;
    fn read_legacy(&self, username: &str) -> Option<String>;
    fn save_protected(&self, username: &str, password: &str) -> KeychainResult<()>;
    fn clear_legacy(&self, username: &str);
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

    fn save_protected(&self, username: &str, password: &str) -> KeychainResult<()> {
        save_protected(username, password)
    }

    fn clear_legacy(&self, username: &str) {
        clear_legacy(username);
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

fn save_protected(username: &str, password: &str) -> KeychainResult<()> {
    let mut item_options =
        options(username).ok_or_else(|| Error::from_code(errSecUnimplemented))?;
    // Recreate the item to preserve the access rule even when a password changes.
    // ThisDeviceOnly also prevents the resumable credential from syncing to iCloud.
    match delete_generic_password_options(
        options(username).ok_or_else(|| Error::from_code(errSecUnimplemented))?,
    ) {
        Ok(()) => {}
        Err(error) if error.code() == errSecItemNotFound => {}
        Err(error) => return Err(error),
    }
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleWhenUnlockedThisDeviceOnly),
        AccessControlOptions::BIOMETRY_CURRENT_SET.bits(),
    )?;
    item_options.set_access_control(access);
    set_generic_password_options(password.as_bytes(), item_options)
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

/// Save the raw login password only behind the current enrolled Touch ID set.
pub fn save(username: &str, password: &str) {
    if let Err(error) = save_protected(username, password) {
        log::warn!("failed to save Touch ID credential: {error}");
    }
    // An older release stored this password in a standard Keychain item. Never
    // leave that weaker copy behind, even if biometric enrollment is unavailable.
    clear_legacy(username);
}

/// Read the secret through Keychain's access control. A canceled or unavailable
/// biometric prompt returns None, so the caller shows the app password form.
pub fn load(username: &str) -> Option<String> {
    load_from(&MacKeychain, username)
}

fn load_from(store: &impl CredentialStore, username: &str) -> Option<String> {
    let bytes = match store.read_protected(username) {
        Ok(bytes) => bytes,
        Err(error) if error.code() == errSecItemNotFound => {
            // Existing saved sessions migrate once. Do not use the legacy secret
            // to authenticate directly: the new item must first pass Touch ID.
            let password = store.read_legacy(username)?;
            let saved = store.save_protected(username, &password);
            store.clear_legacy(username);
            if let Err(error) = saved {
                log::warn!("failed to migrate saved session to Touch ID: {error}");
                return None;
            }
            match store.read_protected(username) {
                Ok(bytes) => bytes,
                Err(error) => {
                    log::warn!("Touch ID credential unavailable after migration: {error}");
                    return None;
                }
            }
        }
        Err(error) => {
            log::info!("Touch ID credential was not unlocked: {error}");
            return None;
        }
    };
    String::from_utf8(bytes).ok()
}

/// Delete both the biometric item and any saved item from an older release.
pub fn clear(username: &str) {
    if let Some(options) = options(username) {
        match delete_generic_password_options(options) {
            Ok(()) => {}
            Err(error) if error.code() == errSecItemNotFound => {}
            Err(error) => log::warn!("failed to remove Touch ID credential: {error}"),
        }
    }
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
        legacy: Option<String>,
        deny_read_on: Option<usize>,
        reads: Cell<usize>,
        legacy_reads: Cell<usize>,
        legacy_cleared: Cell<bool>,
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
            self.legacy.clone()
        }

        fn save_protected(&self, _username: &str, password: &str) -> KeychainResult<()> {
            *self.protected.borrow_mut() = Some(password.as_bytes().to_vec());
            Ok(())
        }

        fn clear_legacy(&self, _username: &str) {
            self.legacy_cleared.set(true);
        }
    }

    #[test]
    fn migration_must_read_back_through_protected_item() {
        let store = FakeStore {
            protected: RefCell::new(None),
            legacy: Some("raw password".into()),
            deny_read_on: None,
            reads: Cell::new(0),
            legacy_reads: Cell::new(0),
            legacy_cleared: Cell::new(false),
        };
        assert_eq!(load_from(&store, "alice"), Some("raw password".into()));
        assert_eq!(store.reads.get(), 2);
        assert!(store.legacy_cleared.get());
    }

    #[test]
    fn canceled_touch_id_does_not_use_legacy_secret() {
        let store = FakeStore {
            protected: RefCell::new(Some(b"raw password".to_vec())),
            legacy: Some("raw password".into()),
            deny_read_on: Some(1),
            reads: Cell::new(0),
            legacy_reads: Cell::new(0),
            legacy_cleared: Cell::new(false),
        };
        assert_eq!(load_from(&store, "alice"), None);
        assert_eq!(store.legacy_reads.get(), 0);
    }

    #[test]
    fn migration_cannot_unlock_when_protected_read_is_denied() {
        let store = FakeStore {
            protected: RefCell::new(None),
            legacy: Some("raw password".into()),
            deny_read_on: Some(2),
            reads: Cell::new(0),
            legacy_reads: Cell::new(0),
            legacy_cleared: Cell::new(false),
        };
        assert_eq!(load_from(&store, "alice"), None);
        assert_eq!(store.reads.get(), 2);
        assert!(store.legacy_cleared.get());
    }
}

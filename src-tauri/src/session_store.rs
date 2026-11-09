//! "Stay signed in": persist the login password in the OS credential store so the app can
//! reopen the encrypted stores on the next launch.
//!
//! The macOS auto-login item uses the standard OS Keychain. The in-app lock
//! screen separately uses macOS device-owner authentication.
//! Optional macOS convenience mode uses an encrypted local entry protected by
//! filesystem user permissions; its local key does not isolate same-user processes.
//! The password is never written to a plaintext file or localStorage. The feature
//! remains gated behind the user-controllable "Stay signed in" toggle.
//!
//! Every operation here is best-effort: a keychain that's locked, unavailable, or
//! returns an error must degrade to manual login rather than break sign-in. So all
//! reads degrade to a missing credential; writes report whether storage succeeded.

#[cfg(target_os = "macos")]
// Unit tests exercise migration with a fake provider; they must not touch the real Keychain.
#[cfg_attr(test, allow(dead_code))]
mod macos;
#[cfg(all(not(test), target_os = "macos"))]
pub use macos::{load, save};

/// The credential service name; the account is the mesh-talk username.
#[cfg(all(not(test), not(target_os = "macos")))]
const SERVICE: &str = "mesh-talk";

/// Build the keychain entry for a username, or `None` if the platform keyring
/// can't be reached (e.g. no Secret Service on a headless Linux box).
#[cfg(all(not(test), not(target_os = "macos")))]
fn entry(username: &str) -> Option<keyring::Entry> {
    match keyring::Entry::new(SERVICE, username) {
        Ok(e) => Some(e),
        Err(e) => {
            log::warn!("keychain unavailable for stay-signed-in: {e}");
            None
        }
    }
}

/// Persist the password for `username` in the OS keychain (best-effort). Overwrites
/// any existing secret for that account.
#[cfg(all(not(test), not(target_os = "macos")))]
pub fn save(username: &str, password: &str) -> bool {
    let Some(entry) = entry(username) else {
        return false;
    };
    match entry.set_password(password) {
        Ok(()) => true,
        Err(e) => {
            log::warn!("failed to save stay-signed-in secret: {e}");
            false
        }
    }
}

/// Load the saved password for `username`, or `None` if there is no entry, the
/// keychain is unavailable, or the read failed (any failure → manual login).
#[cfg(all(not(test), not(target_os = "macos")))]
pub fn load(username: &str) -> Option<String> {
    let entry = entry(username)?;
    match entry.get_password() {
        Ok(pw) => Some(pw),
        Err(keyring::Error::NoEntry) => None,
        Err(e) => {
            log::warn!("failed to load stay-signed-in secret: {e}");
            None
        }
    }
}

/// Clear any saved password for `username` (best-effort; a missing entry is fine).
#[cfg(all(not(test), not(target_os = "macos")))]
pub fn clear(username: &str) {
    let Some(entry) = entry(username) else { return };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => log::warn!("failed to clear stay-signed-in secret: {e}"),
    }
}

// The OS credential provider is deliberately mocked in unit/IPC tests. This
// avoids touching the developer's real credentials and permits deterministic
// owner-isolation checks; production still uses the platform keyring above.
#[cfg(test)]
static TEST_SECRETS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::LazyLock::new(Default::default);
#[cfg(test)]
type TestIoHook = Box<dyn FnOnce() + Send>;
#[cfg(test)]
static TEST_IO_HOOKS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<(String, &'static str), TestIoHook>>,
> = std::sync::LazyLock::new(Default::default);
#[cfg(test)]
pub(crate) fn before_next_io(username: &str, operation: &'static str, hook: TestIoHook) {
    TEST_IO_HOOKS
        .lock()
        .unwrap()
        .insert((username.into(), operation), hook);
}
#[cfg(test)]
fn test_io(username: &str, operation: &'static str) {
    let hook = TEST_IO_HOOKS
        .lock()
        .unwrap()
        .remove(&(username.into(), operation));
    if let Some(hook) = hook {
        hook();
    }
}
#[cfg(test)]
pub fn save(username: &str, password: &str) -> bool {
    test_io(username, "save");
    TEST_SECRETS
        .lock()
        .unwrap()
        .insert(username.into(), password.into());
    true
}
#[cfg(test)]
pub fn load(username: &str) -> Option<String> {
    test_io(username, "load");
    TEST_SECRETS.lock().unwrap().get(username).cloned()
}
#[cfg(test)]
pub fn clear(username: &str) {
    test_io(username, "clear");
    let mut secrets = TEST_SECRETS.lock().unwrap();
    secrets.remove(username);
    secrets.remove(&format!("local:{username}"));
}

// Ownership tests use the explicit cfg(test) provider above. They prove auth and
// metadata sequencing, not a real platform keyring round trip: keyring's built-in
// mock is per Entry, and the real keyring can be locked or absent in CI. Production
// platform integration remains a separate end-to-end validation boundary.

#[cfg(any(test, target_os = "macos"))]
mod local;

/// Select exactly one credential provider; convenience mode never touches Keychain.
pub(crate) fn load_with_policy(username: &str, verification: bool) -> Option<String> {
    #[cfg(all(not(test), target_os = "macos"))]
    if !verification {
        return local::load(&local_root(), username).ok().flatten();
    }
    #[cfg(test)]
    if !verification {
        return load(&format!("local:{username}"));
    }
    let _ = verification;
    load(username)
}

pub(crate) fn save_with_policy(username: &str, password: &str, verification: bool) -> bool {
    #[cfg(all(not(test), target_os = "macos"))]
    if !verification {
        return local::save(&local_root(), username, password).is_ok();
    }
    #[cfg(test)]
    if !verification {
        return save(&format!("local:{username}"), password);
    }
    let _ = verification;
    save(username, password)
}

#[cfg(all(not(test), target_os = "macos"))]
fn local_root() -> std::path::PathBuf {
    crate::data_dir().join("saved-sessions")
}

pub(crate) fn clear_local(username: &str) -> Result<(), String> {
    #[cfg(all(not(test), target_os = "macos"))]
    {
        local::clear(&local_root(), username)
    }
    #[cfg(test)]
    {
        clear(&format!("local:{username}"));
        if FAIL_LOCAL_CLEAR.lock().unwrap().remove(username) {
            return Err("Injected directory sync failure after local removal".into());
        }
        Ok(())
    }
    #[cfg(all(not(test), not(target_os = "macos")))]
    {
        let _ = username;
        Ok(())
    }
}

#[cfg(all(not(test), target_os = "macos"))]
pub fn clear(username: &str) {
    // Local recovery is withdrawn before potentially interactive native cleanup.
    if let Err(error) = clear_local(username) {
        log::error!("Could not clear local saved login: {error}");
    }
    macos::clear(username);
}

/// Automatic recovery cleans only the selected provider. Explicit sign-out clears all.
pub(crate) fn clear_with_policy(username: &str, verification: bool) -> Result<(), String> {
    #[cfg(any(test, target_os = "macos"))]
    if !verification {
        return clear_local(username);
    }
    let _ = verification;
    clear(username);
    Ok(())
}

#[cfg(test)]
static FAIL_LOCAL_CLEAR: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);
#[cfg(test)]
pub(crate) fn fail_local_clear_after_removal(username: &str) {
    FAIL_LOCAL_CLEAR.lock().unwrap().insert(username.into());
}

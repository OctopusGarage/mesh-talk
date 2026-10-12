//! "Stay signed in": persist the login password in the OS credential store so the app can
//! reopen the encrypted stores on the next launch. On macOS a separate Touch ID
//! item is used only to unlock the in-app lock screen.
//!
//! The macOS auto-login item uses the standard OS Keychain; the optional lock-screen
//! item is device-local and tied to the current enrolled fingerprints.
//! The password is never written to a plaintext file or localStorage. The feature
//! remains gated behind the user-controllable "Stay signed in" toggle.
//!
//! Every operation here is best-effort: a keychain that's locked, unavailable, or
//! returns an error must degrade to manual login rather than break sign-in. So all
//! functions log and swallow errors instead of propagating them.

#[cfg(target_os = "macos")]
// Unit tests exercise migration with a fake provider; they must not touch the real Keychain.
#[cfg_attr(test, allow(dead_code))]
mod macos;
#[cfg(all(not(test), target_os = "macos"))]
pub use macos::{clear, load, load_for_unlock, save};

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
pub fn save(username: &str, password: &str) {
    let Some(entry) = entry(username) else { return };
    if let Err(e) = entry.set_password(password) {
        log::warn!("failed to save stay-signed-in secret: {e}");
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
pub fn save(username: &str, password: &str) {
    TEST_SECRETS
        .lock()
        .unwrap()
        .insert(username.into(), password.into());
}
#[cfg(test)]
pub fn load(username: &str) -> Option<String> {
    TEST_SECRETS.lock().unwrap().get(username).cloned()
}
#[cfg(all(test, target_os = "macos"))]
pub fn load_for_unlock(username: &str) -> Option<String> {
    load(username)
}
#[cfg(test)]
pub fn clear(username: &str) {
    TEST_SECRETS.lock().unwrap().remove(username);
}

// Ownership tests use the explicit cfg(test) provider above. They prove auth and
// metadata sequencing, not a real platform keyring round trip: keyring's built-in
// mock is per Entry, and the real keyring can be locked or absent in CI. Production
// platform integration remains a separate end-to-end validation boundary.

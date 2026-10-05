//! "Stay signed in": persist the login password in the OS keychain so the app can
//! auto-unlock the encrypted stores on the next launch without re-prompting.
//!
//! SECURITY: storing the password in the OS keychain (macOS Keychain, Windows
//! Credential Manager, Linux Secret Service) means anyone with access to the
//! *already-unlocked* OS user account can open the app and read the user's
//! messages. This is the standard desktop-messenger tradeoff (Signal Desktop and
//! Slack persist a session secret the same way). The password is NEVER written to
//! a plaintext file or to localStorage — the keychain (which is itself encrypted
//! and unlocked alongside the OS login) is the only store. The whole feature is
//! gated behind a user-controllable "Stay signed in" toggle.
//!
//! Every operation here is best-effort: a keychain that's locked, unavailable, or
//! returns an error must degrade to manual login rather than break sign-in. So all
//! functions log and swallow errors instead of propagating them.

/// The keychain service name; the account is the mesh-talk username.
#[cfg(not(test))]
const SERVICE: &str = "mesh-talk";

/// Build the keychain entry for a username, or `None` if the platform keyring
/// can't be reached (e.g. no Secret Service on a headless Linux box).
#[cfg(not(test))]
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
#[cfg(not(test))]
pub fn save(username: &str, password: &str) {
    let Some(entry) = entry(username) else { return };
    if let Err(e) = entry.set_password(password) {
        log::warn!("failed to save stay-signed-in secret: {e}");
    }
}

/// Load the saved password for `username`, or `None` if there is no entry, the
/// keychain is unavailable, or the read failed (any failure → manual login).
#[cfg(not(test))]
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
#[cfg(not(test))]
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
#[cfg(test)]
pub fn clear(username: &str) {
    TEST_SECRETS.lock().unwrap().remove(username);
}

// Ownership tests use the explicit cfg(test) provider above. They prove auth and
// metadata sequencing, not a real platform keyring round trip: keyring's built-in
// mock is per Entry, and the real keyring can be locked or absent in CI. Production
// platform integration remains a separate end-to-end validation boundary.

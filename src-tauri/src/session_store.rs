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
const SERVICE: &str = "mesh-talk";

/// Build the keychain entry for a username, or `None` if the platform keyring
/// can't be reached (e.g. no Secret Service on a headless Linux box).
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
pub fn save(username: &str, password: &str) {
    let Some(entry) = entry(username) else { return };
    if let Err(e) = entry.set_password(password) {
        log::warn!("failed to save stay-signed-in secret: {e}");
    }
}

/// Load the saved password for `username`, or `None` if there is no entry, the
/// keychain is unavailable, or the read failed (any failure → manual login).
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
pub fn clear(username: &str) {
    let Some(entry) = entry(username) else { return };
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(e) => log::warn!("failed to clear stay-signed-in secret: {e}"),
    }
}

// NOTE (test seam): there is no clean unit test for the save→load→clear round trip. Each
// function opens a FRESH `keyring::Entry`, and keyring's `mock` backend is per-Entry and
// explicitly non-persistent ("no persistence between sessions"), so a mocked round trip
// can't share state across calls; a real-keychain test is flaky/unavailable in CI (locked
// or absent on headless runners). The logic here is a thin, error-swallowing wrapper; its
// behavior is exercised end-to-end by the app's auto-login flow.

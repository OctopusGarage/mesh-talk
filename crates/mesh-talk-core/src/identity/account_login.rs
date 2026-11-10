//! Account sign-in aliases unlock retained, device-local storage credentials.
//!
//! Only the envelope is replaced. Existing identity files and node stores retain
//! their original passwords and local-user namespace. Hosts must authenticate
//! the owning desktop session and certified mesh account before installation.

use super::{errors::IdentityError, manager::IdentityManager, user::User};
use crate::storage::encryption::{EncryptionKey, decrypt_data, encrypt_data, generate_salt};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
};
use zeroize::{Zeroize, Zeroizing};

const ENVELOPE_PATH: &str = "meta/account_login.bin";
const ENVELOPE_VERSION: u32 = 1;
const MAX_ENVELOPE_BYTES: u64 = 64 * 1024;

/// Resolved credentials for distinct identity and node-storage owners.
/// Passwords are never included in Debug output and are zeroized on drop.
pub struct ResolvedAccountLogin {
    pub user: User,
    pub login_name: String,
    pub identity_password: String,
    pub node_storage_password: String,
}

impl fmt::Debug for ResolvedAccountLogin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolvedAccountLogin")
            .field("user", &self.user)
            .field("login_name", &self.login_name)
            .field("identity_password", &"[REDACTED]")
            .field("node_storage_password", &"[REDACTED]")
            .finish()
    }
}

impl Drop for ResolvedAccountLogin {
    fn drop(&mut self) {
        self.identity_password.zeroize();
        self.node_storage_password.zeroize();
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Binding {
    version: u32,
    storage_username: String,
    local_user_id: String,
    account_id: String,
    login_name: String,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    binding: Binding,
    salt: [u8; 16],
    nonce: [u8; 12],
    ciphertext: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct Credentials {
    binding: Binding,
    identity_password: String,
    node_storage_password: String,
}

impl Drop for Credentials {
    fn drop(&mut self) {
        self.identity_password.zeroize();
        self.node_storage_password.zeroize();
    }
}

struct InstallCredentials<'a> {
    storage_username: &'a str,
    identity_password: &'a str,
    node_storage_password: &'a str,
    account_id: &'a str,
    canonical_login_name: &'a str,
    raw_shared_login_password: &'a str,
    expected_previous_account: Option<&'a str>,
}

fn invalid_envelope() -> IdentityError {
    IdentityError::StorageError("Invalid account login envelope; preserve it for recovery".into())
}

fn io_error(_: std::io::Error) -> IdentityError {
    IdentityError::StorageError("Account login storage operation failed".into())
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, IdentityError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_ENVELOPE_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|_| invalid_envelope())
}

fn validate_name(name: &str) -> Result<(), IdentityError> {
    if User::validate_username(name) {
        Ok(())
    } else {
        Err(IdentityError::InvalidUsername)
    }
}

impl IdentityManager {
    /// Publish a verified, staged local user while reserving the shared login
    /// namespace. The callback must not reenter this manager.
    pub fn publish_new_account_login<T>(
        &self,
        login_name: &str,
        publish: impl FnOnce() -> T,
    ) -> Result<T, IdentityError> {
        validate_name(login_name)?;
        let _lock = self.account_login_namespace_lock()?;
        self.ensure_account_login_available(login_name, login_name)?;
        match fs::symlink_metadata(self.file_manager.user_data_path(login_name)) {
            Ok(_) => return Err(IdentityError::UserAlreadyExists(login_name.into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(error)),
        }
        Ok(publish())
    }

    // Shared by registration and migration, including separate manager instances
    // and processes. Dropping the handle releases the OS-managed lock.
    pub(super) fn account_login_namespace_lock(&self) -> Result<File, IdentityError> {
        fs::create_dir_all(self.file_manager.base_path()).map_err(io_error)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.file_manager.base_path().join(".account-login.lock"))
            .map_err(io_error)?;
        lock.lock().map_err(io_error)?;
        Ok(lock)
    }

    fn read_account_login(&self, username: &str) -> Result<Option<Envelope>, IdentityError> {
        let path = self
            .file_manager
            .user_data_path(username)
            .join(ENVELOPE_PATH);
        // symlink_metadata also notices dangling symlinks: they cannot restore
        // legacy authentication by masquerading as an absent envelope.
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(io_error(e)),
            Ok(meta) if !meta.is_file() => return Err(invalid_envelope()),
            Ok(meta) if meta.len() > MAX_ENVELOPE_BYTES => return Err(invalid_envelope()),
            Ok(_) => {}
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(io_error)?
            .take(MAX_ENVELOPE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() as u64 > MAX_ENVELOPE_BYTES {
            return Err(invalid_envelope());
        }
        let envelope: Envelope = decode(&bytes)?;
        let binding = &envelope.binding;
        if binding.version != ENVELOPE_VERSION
            || binding.storage_username != username
            || !User::validate_username(&binding.login_name)
            || !User::validate_user_id(&binding.local_user_id)
            || binding.account_id.len() != 32
            || !binding.account_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid_envelope());
        }
        Ok(Some(envelope))
    }

    fn account_login_envelopes(&self) -> Result<Vec<Envelope>, IdentityError> {
        let users = self.file_manager.base_path().join("users");
        let entries = match fs::read_dir(users) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_error(e)),
        };
        let mut envelopes: Vec<Envelope> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(io_error)?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|name| User::validate_username(name)) else {
                continue;
            };
            if let Some(envelope) = self.read_account_login(name)? {
                if envelopes
                    .iter()
                    .any(|existing| existing.binding.login_name == envelope.binding.login_name)
                {
                    return Err(invalid_envelope());
                }
                // A public alias must never shadow another retained local user.
                if envelope.binding.login_name != name
                    && self
                        .file_manager
                        .user_data_path(&envelope.binding.login_name)
                        .try_exists()
                        .map_err(io_error)?
                {
                    return Err(invalid_envelope());
                }
                envelopes.push(envelope);
            }
        }
        Ok(envelopes)
    }

    pub(super) fn ensure_account_login_available(
        &self,
        storage_username: &str,
        login_name: &str,
    ) -> Result<(), IdentityError> {
        validate_name(storage_username)?;
        validate_name(login_name)?;
        if storage_username != login_name
            && self
                .file_manager
                .user_data_path(login_name)
                .try_exists()
                .map_err(io_error)?
        {
            return Err(IdentityError::UserAlreadyExists(login_name.into()));
        }
        if self.account_login_envelopes()?.iter().any(|envelope| {
            envelope.binding.login_name == login_name
                && envelope.binding.storage_username != storage_username
        }) {
            return Err(IdentityError::UserAlreadyExists(login_name.into()));
        }
        Ok(())
    }

    /// Preflight only: installation rechecks under the namespace lock. Hosts use
    /// this before adopting a paired account; it grants no session authority.
    pub fn validate_account_login_name(
        &self,
        storage_username: &str,
        canonical_login_name: &str,
    ) -> Result<(), IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        self.ensure_account_login_available(storage_username, canonical_login_name)
    }

    /// The public alias is a discovery hint, never proof of authenticated access.
    pub fn account_login_name(
        &self,
        storage_username: &str,
    ) -> Result<Option<String>, IdentityError> {
        validate_name(storage_username)?;
        let _lock = self.account_login_namespace_lock()?;
        Ok(self
            .read_account_login(storage_username)?
            .map(|envelope| envelope.binding.login_name))
    }

    /// Resolve user-facing credentials to the immutable local storage owner.
    /// Legacy sign-in trims only the identity password, retaining the original
    /// raw password for node stores. A published envelope disables that fallback.
    pub fn authenticate_account_login(
        &self,
        login_name: &str,
        raw_login_password: &str,
    ) -> Result<ResolvedAccountLogin, IdentityError> {
        validate_name(login_name)?;
        let _lock = self.account_login_namespace_lock()?;
        let envelopes = self.account_login_envelopes()?;
        if let Some(envelope) = envelopes
            .iter()
            .find(|envelope| envelope.binding.login_name == login_name)
        {
            let key = EncryptionKey::from_password(raw_login_password, &envelope.salt)
                .map_err(|_| IdentityError::InvalidPassword)?;
            let plaintext = Zeroizing::new(
                decrypt_data(&envelope.ciphertext, &envelope.nonce, &key)
                    .map_err(|_| IdentityError::InvalidPassword)?,
            );
            let credentials: Credentials = decode(&plaintext)?;
            if credentials.binding != envelope.binding {
                return Err(invalid_envelope());
            }
            let user = self.authenticate_user(
                &envelope.binding.storage_username,
                &credentials.identity_password,
            )?;
            if user.user_id != envelope.binding.local_user_id
                || user.username != envelope.binding.storage_username
            {
                return Err(invalid_envelope());
            }
            return Ok(ResolvedAccountLogin {
                user,
                login_name: login_name.into(),
                identity_password: credentials.identity_password.clone(),
                node_storage_password: credentials.node_storage_password.clone(),
            });
        }
        if envelopes
            .iter()
            .any(|envelope| envelope.binding.storage_username == login_name)
        {
            return Err(IdentityError::InvalidPassword);
        }
        let identity_password = raw_login_password.trim();
        let user = self.authenticate_user(login_name, identity_password)?;
        Ok(ResolvedAccountLogin {
            user,
            login_name: login_name.into(),
            identity_password: identity_password.into(),
            node_storage_password: raw_login_password.into(),
        })
    }

    /// Atomically install or replace this authenticated user's account login.
    /// The host verifies the active session and account owner; this layer checks
    /// retained identity credentials, UUID and any existing account binding.
    pub fn install_account_login(
        &self,
        storage_username: &str,
        identity_password: &str,
        node_storage_password: &str,
        account_id: &str,
        canonical_login_name: &str,
        raw_shared_login_password: &str,
    ) -> Result<(), IdentityError> {
        self.install_account_login_if(
            storage_username,
            identity_password,
            node_storage_password,
            account_id,
            canonical_login_name,
            raw_shared_login_password,
            |commit| commit(),
        )
    }

    /// Prepare encryption first, then publish only inside the host's exact-session
    /// guard. The callback must call commit once and propagate its result; it
    /// must not fail after a successful publication or reenter IdentityManager.
    #[allow(clippy::too_many_arguments)] // Distinct secret owners remain explicit at the host boundary.
    pub fn install_account_login_if<F>(
        &self,
        storage_username: &str,
        identity_password: &str,
        node_storage_password: &str,
        account_id: &str,
        canonical_login_name: &str,
        raw_shared_login_password: &str,
        authorize_commit: F,
    ) -> Result<(), IdentityError>
    where
        F: FnMut(&mut dyn FnMut() -> Result<(), IdentityError>) -> Result<(), IdentityError>,
    {
        self.install_account_login_guarded(
            InstallCredentials {
                storage_username,
                identity_password,
                node_storage_password,
                account_id,
                canonical_login_name,
                raw_shared_login_password,
                expected_previous_account: None,
            },
            authorize_commit,
        )
    }

    /// Explicit account adoption only. The host must durably authorize the old
    /// and new certified account/device bindings before changing node keys.
    /// Ongoing account refresh must use install_account_login_if instead.
    #[allow(clippy::too_many_arguments)] // Explicit previous-account consent extends the guarded boundary.
    pub fn install_account_login_for_adoption_if<F>(
        &self,
        storage_username: &str,
        identity_password: &str,
        node_storage_password: &str,
        account_id: &str,
        canonical_login_name: &str,
        raw_shared_login_password: &str,
        expected_previous_account: &str,
        authorize_commit: F,
    ) -> Result<(), IdentityError>
    where
        F: FnMut(&mut dyn FnMut() -> Result<(), IdentityError>) -> Result<(), IdentityError>,
    {
        self.install_account_login_guarded(
            InstallCredentials {
                storage_username,
                identity_password,
                node_storage_password,
                account_id,
                canonical_login_name,
                raw_shared_login_password,
                expected_previous_account: Some(expected_previous_account),
            },
            authorize_commit,
        )
    }

    fn install_account_login_guarded<F>(
        &self,
        request: InstallCredentials<'_>,
        mut authorize_commit: F,
    ) -> Result<(), IdentityError>
    where
        F: FnMut(&mut dyn FnMut() -> Result<(), IdentityError>) -> Result<(), IdentityError>,
    {
        let InstallCredentials {
            storage_username,
            identity_password,
            node_storage_password,
            account_id,
            canonical_login_name,
            raw_shared_login_password,
            expected_previous_account,
        } = request;
        validate_name(storage_username)?;
        validate_name(canonical_login_name)?;
        if raw_shared_login_password.trim().is_empty() {
            return Err(IdentityError::InvalidPassword);
        }
        if account_id.len() != 32 || !account_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(IdentityError::InvalidUserId);
        }
        let _lock = self.account_login_namespace_lock()?;
        let user = self.authenticate_user(storage_username, identity_password)?;
        if user.username != storage_username || !User::validate_user_id(&user.user_id) {
            return Err(invalid_envelope());
        }
        self.ensure_account_login_available(storage_username, canonical_login_name)?;
        match (
            self.read_account_login(storage_username)?,
            expected_previous_account,
        ) {
            (Some(existing), previous) => {
                let expected = previous.unwrap_or(account_id);
                if existing.binding.account_id != expected
                    || existing.binding.local_user_id != user.user_id
                {
                    return Err(invalid_envelope());
                }
            }
            (None, Some(_)) => return Err(invalid_envelope()),
            (None, None) => {}
        }
        let binding = Binding {
            version: ENVELOPE_VERSION,
            storage_username: storage_username.into(),
            local_user_id: user.user_id,
            account_id: account_id.into(),
            login_name: canonical_login_name.into(),
        };
        let credentials = Credentials {
            binding: binding.clone(),
            identity_password: identity_password.into(),
            node_storage_password: node_storage_password.into(),
        };
        let plaintext =
            Zeroizing::new(bincode::serialize(&credentials).map_err(|_| invalid_envelope())?);
        if plaintext.len() as u64 > MAX_ENVELOPE_BYTES / 2 {
            return Err(invalid_envelope());
        }
        let salt = generate_salt();
        let key = EncryptionKey::from_password(raw_shared_login_password, &salt)
            .map_err(|_| invalid_envelope())?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &key).map_err(|_| invalid_envelope())?;
        let bytes = bincode::serialize(&Envelope {
            binding,
            salt,
            nonce,
            ciphertext,
        })
        .map_err(|_| invalid_envelope())?;
        let destination = self
            .file_manager
            .user_data_path(storage_username)
            .join(ENVELOPE_PATH);
        let parent = destination.parent().ok_or_else(invalid_envelope)?;
        // Same-directory synchronized publication reuses the storage writer's
        // transaction primitive, including restrictive temporary-file modes.
        let mut staged = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        staged.write_all(&bytes).map_err(io_error)?;
        staged.as_file().sync_all().map_err(io_error)?;
        let mut staged = Some(staged);
        let mut published = false;
        authorize_commit(&mut || {
            staged
                .take()
                .ok_or_else(invalid_envelope)?
                .persist(&destination)
                .map_err(|e| io_error(e.error))?;
            published = true;
            Ok(())
        })?;
        if !published {
            return Err(invalid_envelope());
        }
        // Publication succeeded. A directory fsync failure must not be reported
        // as a pre-publication failure with old credentials supposedly active.
        #[cfg(unix)]
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::identity::manager::IdentityManager;
    use crate::storage::file_manager::FileManager;

    const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";

    fn manager() -> (IdentityManager, FileManager, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let files = FileManager::new(dir.path().to_path_buf());
        (IdentityManager::new(files.clone()), files, dir)
    }

    #[test]
    fn account_login_legacy_preserves_raw_node_and_trimmed_identity_passwords() {
        let (manager, _, _dir) = manager();
        let user = manager.register_user("local", "legacy").unwrap();
        let resolved = manager
            .authenticate_account_login("local", "  legacy  ")
            .unwrap();
        assert_eq!(resolved.user.user_id, user.user_id);
        assert_eq!(resolved.identity_password, "legacy");
        assert_eq!(resolved.node_storage_password, "  legacy  ");
        assert_eq!(resolved.login_name, "local");
    }

    #[test]
    fn account_login_migration_retains_local_identity_and_original_store_bytes() {
        let (manager, files, _dir) = manager();
        let user = manager.register_user("local", "legacy").unwrap();
        let key = manager.get_user_private_key("local", "legacy").unwrap();
        files
            .write_encrypted_file("local", "data/node.enc", &"history and media", "  legacy  ")
            .unwrap();
        let path = files.user_data_path("local").join("data/node.enc");
        let original = std::fs::read(&path).unwrap();
        manager
            .install_account_login(
                "local",
                "legacy",
                "  legacy  ",
                ACCOUNT,
                "Federer",
                " shared ",
            )
            .unwrap();
        let resolved = manager
            .authenticate_account_login("Federer", " shared ")
            .unwrap();
        assert_eq!(resolved.user.username, "local");
        assert_eq!(resolved.user.user_id, user.user_id);
        assert_eq!(resolved.user.public_key, user.public_key);
        assert_eq!(
            manager
                .get_user_private_key("local", &resolved.identity_password)
                .unwrap(),
            key
        );
        assert_eq!(
            files
                .read_encrypted_file::<String>(
                    "local",
                    "data/node.enc",
                    &resolved.node_storage_password
                )
                .unwrap(),
            "history and media"
        );
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(
            manager.account_login_name("local").unwrap().as_deref(),
            Some("Federer")
        );
        for (name, password) in [
            ("local", "legacy"),
            ("Federer", "legacy"),
            ("Federer", "shared"),
        ] {
            assert!(manager.authenticate_account_login(name, password).is_err());
        }
        let debug = format!("{resolved:?}");
        assert!(!debug.contains("legacy"));
        assert!(!debug.contains(" shared "));
    }

    #[test]
    fn account_login_collision_and_failed_auth_preserve_legacy_login() {
        let (manager, _, _dir) = manager();
        manager.register_user("local", "legacy").unwrap();
        manager.register_user("taken", "other").unwrap();
        assert!(
            manager
                .install_account_login("local", "legacy", "legacy", ACCOUNT, "taken", "common")
                .is_err()
        );
        assert!(
            manager
                .install_account_login("local", "wrong", "legacy", ACCOUNT, "Federer", "common")
                .is_err()
        );
        assert!(
            manager
                .authenticate_account_login("local", "legacy")
                .is_ok()
        );
        assert_eq!(manager.account_login_name("local").unwrap(), None);
        manager
            .install_account_login("local", "legacy", "legacy", ACCOUNT, "Federer", "common")
            .unwrap();
        assert!(manager.register_user("Federer", "new").is_err());
        assert!(
            manager
                .install_account_login("taken", "other", "other", ACCOUNT, "Federer", "common")
                .is_err()
        );
        assert!(manager.authenticate_account_login("taken", "other").is_ok());
        assert!(
            manager
                .authenticate_account_login("Federer", "common")
                .is_ok()
        );
    }

    #[test]
    fn account_login_corruption_never_falls_back_to_legacy_authentication() {
        let (manager, files, _dir) = manager();
        manager.register_user("local", "legacy").unwrap();
        manager
            .install_account_login("local", "legacy", "legacy", ACCOUNT, "local", "common")
            .unwrap();
        std::fs::write(
            files.user_data_path("local").join("meta/account_login.bin"),
            b"broken envelope",
        )
        .unwrap();
        assert!(
            manager
                .authenticate_account_login("local", "legacy")
                .is_err()
        );
        assert!(
            manager
                .authenticate_account_login("local", "common")
                .is_err()
        );
        assert!(manager.register_user("another", "pw").is_err());
    }
    #[test]
    fn account_login_swapped_envelopes_fail_closed_for_both_users() {
        let (manager, files, _dir) = manager();
        manager.register_user("alice", "alice-old").unwrap();
        manager.register_user("bob", "bob-old").unwrap();
        manager
            .install_account_login(
                "alice",
                "alice-old",
                "alice-node",
                ACCOUNT,
                "AliceLogin",
                "common",
            )
            .unwrap();
        manager
            .install_account_login("bob", "bob-old", "bob-node", ACCOUNT, "BobLogin", "common")
            .unwrap();
        let alice = files.user_data_path("alice").join("meta/account_login.bin");
        let bob = files.user_data_path("bob").join("meta/account_login.bin");
        let first = std::fs::read(&alice).unwrap();
        let second = std::fs::read(&bob).unwrap();
        std::fs::write(alice, second).unwrap();
        std::fs::write(bob, first).unwrap();
        for (name, password) in [
            ("AliceLogin", "common"),
            ("BobLogin", "common"),
            ("alice", "alice-old"),
            ("bob", "bob-old"),
        ] {
            assert!(manager.authenticate_account_login(name, password).is_err());
        }
    }

    #[test]
    fn account_login_replacement_retains_owner_and_reserves_partial_local_directories() {
        let (manager, files, _dir) = manager();
        manager.register_user("local", "legacy").unwrap();
        manager
            .install_account_login("local", "legacy", "node", ACCOUNT, "Federer", "first")
            .unwrap();
        let envelope = files.user_data_path("local").join("meta/account_login.bin");
        let original = std::fs::read(&envelope).unwrap();
        assert!(
            manager
                .install_account_login(
                    "local",
                    "legacy",
                    "node",
                    "ffffffffffffffffffffffffffffffff",
                    "Federer",
                    "second"
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&envelope).unwrap(), original);
        std::fs::create_dir_all(files.user_data_path("partial")).unwrap();
        assert!(
            manager
                .validate_account_login_name("local", "partial")
                .is_err()
        );
        assert!(
            manager
                .install_account_login("local", "legacy", "node", ACCOUNT, "partial", "second")
                .is_err()
        );
        assert_eq!(std::fs::read(&envelope).unwrap(), original);
        manager
            .install_account_login("local", "legacy", "node", ACCOUNT, "Federer", "second")
            .unwrap();
        assert!(
            manager
                .authenticate_account_login("Federer", "first")
                .is_err()
        );
        assert!(
            manager
                .authenticate_account_login("Federer", "second")
                .is_ok()
        );
    }

    #[test]
    fn account_login_public_binding_tampering_is_rejected_after_decryption() {
        let (manager, files, _dir) = manager();
        manager.register_user("local", "legacy").unwrap();
        manager
            .install_account_login("local", "legacy", "node", ACCOUNT, "Federer", "common")
            .unwrap();
        let path = files.user_data_path("local").join("meta/account_login.bin");
        let mut envelope: super::Envelope = super::decode(&std::fs::read(&path).unwrap()).unwrap();
        envelope.binding.login_name = "Tampered".into();
        std::fs::write(path, bincode::serialize(&envelope).unwrap()).unwrap();
        assert!(
            manager
                .authenticate_account_login("Tampered", "common")
                .is_err()
        );
        assert!(
            manager
                .authenticate_account_login("Federer", "common")
                .is_err()
        );
        assert!(
            manager
                .authenticate_account_login("local", "legacy")
                .is_err()
        );
    }

    #[test]
    fn account_login_concurrent_installation_reserves_exactly_one_alias_owner() {
        let (manager, _, _dir) = manager();
        manager.register_user("alice", "password").unwrap();
        manager.register_user("bob", "password").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = ["alice", "bob"]
            .into_iter()
            .map(|name| {
                let manager = manager.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager
                        .install_account_login(name, "password", name, ACCOUNT, "Shared", "common")
                        .is_ok()
                })
            })
            .collect();
        let successes = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        let resolved = manager
            .authenticate_account_login("Shared", "common")
            .unwrap();
        assert_eq!(resolved.node_storage_password, resolved.user.username);
    }

    #[test]
    fn account_login_guard_denial_preserves_existing_credential_envelope() {
        let (manager, files, _dir) = manager();
        manager.register_user("local", "legacy").unwrap();
        manager
            .install_account_login("local", "legacy", "node", ACCOUNT, "Federer", "first")
            .unwrap();
        let path = files.user_data_path("local").join("meta/account_login.bin");
        let original = std::fs::read(&path).unwrap();
        let mut reached_guard = false;
        let result = manager.install_account_login_if(
            "local",
            "legacy",
            "node",
            ACCOUNT,
            "Federer",
            "second",
            |_commit| {
                reached_guard = true;
                assert_eq!(std::fs::read(&path).unwrap(), original);
                Err(crate::identity::errors::IdentityError::InvalidPassword)
            },
        );
        assert!(reached_guard);
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(
            manager
                .authenticate_account_login("Federer", "first")
                .is_ok()
        );
        assert!(
            manager
                .authenticate_account_login("Federer", "second")
                .is_err()
        );
    }

    #[test]
    fn account_login_explicit_adoption_requires_matching_previous_account_owner() {
        let (manager, files, _dir) = manager();
        let user = manager.register_user("local", "legacy").unwrap();
        manager
            .install_account_login("local", "legacy", "node", ACCOUNT, "Federer", "first")
            .unwrap();
        let next = "ffffffffffffffffffffffffffffffff";
        let unrelated = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        let path = files.user_data_path("local").join("meta/account_login.bin");
        let original = std::fs::read(&path).unwrap();
        assert!(
            manager
                .install_account_login_for_adoption_if(
                    "local",
                    "legacy",
                    "node",
                    next,
                    "NewLogin",
                    "second",
                    unrelated,
                    |commit| commit()
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);
        manager
            .install_account_login_for_adoption_if(
                "local",
                "legacy",
                "node",
                next,
                "NewLogin",
                "second",
                ACCOUNT,
                |commit| commit(),
            )
            .unwrap();
        let resolved = manager
            .authenticate_account_login("NewLogin", "second")
            .unwrap();
        assert_eq!(resolved.user.user_id, user.user_id);
        assert_eq!(resolved.user.username, "local");
        assert_eq!(resolved.node_storage_password, "node");
        assert!(
            manager
                .authenticate_account_login("Federer", "first")
                .is_err()
        );
    }
}

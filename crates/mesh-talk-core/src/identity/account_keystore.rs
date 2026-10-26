//! Password-encrypted, on-disk keystore for an `Account` secret (the cross-device
//! Ed25519 account key). Separate file from the device keystore: the account key
//! is shared across a user's devices and is transferred at link time, so it has
//! its own lifecycle.
//!
//! Format: MTKS1 || salt(16) || nonce(12) || AES-256-GCM ciphertext of the
//! versioned secret and creation metadata. Legacy unversioned secrets remain
//! readable; older clients fail authentication on the new framing.

use crate::identity::account::Account;
use crate::storage::encryption::{
    EncryptionKey, NONCE_SIZE, SALT_SIZE, decrypt_data, encrypt_data, generate_salt,
};
use crate::storage::errors::StorageError;
use std::io::{Read, Write};
use std::path::Path;

const PAYLOAD_MAGIC: &[u8; 5] = b"MTAK1";
const FILE_MAGIC: &[u8; 5] = b"MTKS1";
const MAX_KEYSTORE_BYTES: u64 = 128;

/// Store the secret and immutable creation metadata in one authenticated payload.
pub fn save(path: &Path, password: &str, account: &Account) -> Result<(), StorageError> {
    save_with_mode(path, password, account, true)
}

fn save_with_mode(
    path: &Path,
    password: &str,
    account: &Account,
    replace: bool,
) -> Result<(), StorageError> {
    let mut plaintext = zeroize::Zeroizing::new(Vec::with_capacity(46));
    plaintext.extend_from_slice(PAYLOAD_MAGIC);
    plaintext.extend_from_slice(&account.secret_bytes());
    match account.created_at_ms() {
        Some(time) => {
            plaintext.push(1);
            plaintext.extend_from_slice(&time.to_be_bytes());
        }
        None => plaintext.push(0),
    }
    write_encrypted_with_mode(path, password, &plaintext, replace, true)
}

#[cfg(test)]
fn write_encrypted(path: &Path, password: &str, plaintext: &[u8]) -> Result<(), StorageError> {
    write_encrypted_with_mode(path, password, plaintext, true, false)
}

fn write_encrypted_with_mode(
    path: &Path,
    password: &str,
    plaintext: &[u8],
    replace: bool,
    versioned: bool,
) -> Result<(), StorageError> {
    let salt = generate_salt();
    let key = EncryptionKey::from_password(password, &salt)?;
    let (ciphertext, nonce) = encrypt_data(plaintext, &key)?;

    let mut out = Vec::with_capacity(FILE_MAGIC.len() + SALT_SIZE + NONCE_SIZE + ciphertext.len());
    if versioned {
        out.extend_from_slice(FILE_MAGIC);
    }
    out.extend_from_slice(&salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|_| StorageError::DirectoryCreationFailed(parent.to_path_buf()))?;
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(&out)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)
            .map_err(|error| StorageError::Io(error.error))?;
    } else {
        // Publish only if the account path is still absent. A simultaneous
        // initializer must adopt the winner rather than replacing its identity.
        file.persist_noclobber(path)
            .map_err(|error| StorageError::Io(error.error))?;
    }
    #[cfg(unix)]
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

/// Read and decrypt the account keystore at `path` with `password`.
pub fn load(path: &Path, password: &str) -> Result<Account, StorageError> {
    let mut content = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_KEYSTORE_BYTES + 1)
        .read_to_end(&mut content)?;
    if content.len() as u64 > MAX_KEYSTORE_BYTES {
        return Err(StorageError::Deserialization(
            "account keystore exceeds limit".into(),
        ));
    }
    let versioned = content.starts_with(FILE_MAGIC);
    let content = if versioned {
        &content[FILE_MAGIC.len()..]
    } else {
        content.as_slice()
    };
    if content.len() < SALT_SIZE + NONCE_SIZE {
        return Err(StorageError::Decryption(
            "account keystore file too short".to_string(),
        ));
    }
    let salt: [u8; SALT_SIZE] = content[0..SALT_SIZE]
        .try_into()
        .expect("length checked above");
    let nonce: [u8; NONCE_SIZE] = content[SALT_SIZE..SALT_SIZE + NONCE_SIZE]
        .try_into()
        .expect("length checked above");
    let ciphertext = &content[SALT_SIZE + NONCE_SIZE..];

    let key = EncryptionKey::from_password(password, &salt)?;
    let plaintext = zeroize::Zeroizing::new(decrypt_data(ciphertext, &nonce, &key)?);
    if !versioned && plaintext.len() == 32 {
        return Ok(Account::from_secret_bytes(
            plaintext.as_slice().try_into().expect("length checked"),
        ));
    }
    let invalid = || StorageError::Deserialization("invalid account metadata payload".into());
    if !versioned || !plaintext.starts_with(PAYLOAD_MAGIC) || plaintext.len() < 38 {
        return Err(invalid());
    }
    let secret = plaintext[5..37].try_into().expect("length checked");
    let created_at_ms = match (plaintext[37], plaintext.len()) {
        (0, 38) => None,
        (1, 46) => Some(u64::from_be_bytes(
            plaintext[38..46].try_into().expect("length checked"),
        )),
        _ => return Err(invalid()),
    };
    Account::from_secret_bytes_with_created_at(secret, created_at_ms).ok_or_else(invalid)
}

/// Load the account if present, otherwise generate a new one and save it.
pub fn load_or_create(path: &Path, password: &str) -> Result<Account, StorageError> {
    if path.exists() {
        load(path, password)
    } else {
        let account = Account::generate();
        match save_with_mode(path, password, &account, false) {
            Ok(()) => Ok(account),
            Err(StorageError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                load(path, password)
            }
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_clients_cannot_misread_versioned_metadata_as_an_account_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        // The legacy loader assumes an unversioned salt/nonce header and then
        // deserializes the first 32 plaintext bytes without rejecting trailing data.
        let salt: [u8; SALT_SIZE] = bytes[..SALT_SIZE].try_into().unwrap();
        let nonce: [u8; NONCE_SIZE] = bytes[SALT_SIZE..SALT_SIZE + NONCE_SIZE].try_into().unwrap();
        let key = EncryptionKey::from_password("pw", &salt).unwrap();
        assert!(
            decrypt_data(&bytes[SALT_SIZE + NONCE_SIZE..], &nonce, &key).is_err(),
            "old loaders must fail closed instead of loading the metadata prefix as a different secret"
        );
    }

    #[test]
    fn save_then_load_round_trips_the_account() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let original = Account::generate();
        save(&path, "correct horse battery staple", &original).expect("save");

        let loaded = load(&path, "correct horse battery staple").expect("load");
        assert_eq!(original.account_id(), loaded.account_id());
        assert!(original.created_at_ms().is_some());
        assert_eq!(original.created_at_ms(), loaded.created_at_ms());
    }

    #[test]
    fn legacy_secret_stays_unknown_after_resave() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.keystore");
        let account = Account::generate();
        write_encrypted(&path, "pw", &account.secret_bytes()).unwrap();
        let legacy = load(&path, "pw").unwrap();
        assert_eq!(legacy.account_id(), account.account_id());
        assert_eq!(legacy.created_at_ms(), None);
        save(&path, "pw", &legacy).unwrap();
        assert_eq!(load(&path, "pw").unwrap().created_at_ms(), None);
    }

    #[test]
    fn rejects_authenticated_malformed_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.keystore");
        for plaintext in [
            [b"MTAK1".as_slice(), &[7; 32], &[1], &0u64.to_be_bytes()].concat(),
            vec![7; 33],
            vec![7; 1024],
        ] {
            write_encrypted_with_mode(&path, "pw", &plaintext, true, true).unwrap();
            assert!(load(&path, "pw").is_err());
        }
    }

    #[test]
    fn concurrent_initialization_preserves_one_account_and_creation_timestamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account.keystore");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let results = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let barrier = barrier.clone();
                    let path = path.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        let account = load_or_create(&path, "pw").unwrap();
                        (account.account_id(), account.created_at_ms())
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        let loaded = load(&path, "pw").unwrap();
        let expected = (loaded.account_id(), loaded.created_at_ms());
        assert!(results.iter().all(|result| *result == expected));
    }

    #[test]
    fn wrong_password_fails_to_decrypt() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "right", &Account::generate()).expect("save");
        assert!(load(&path, "wrong").is_err());
    }

    #[test]
    fn load_or_create_is_stable_across_calls() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");

        let first = load_or_create(&path, "pw").expect("first");
        let second = load_or_create(&path, "pw").expect("second");
        assert_eq!(first.account_id(), second.account_id());
    }

    #[test]
    fn tampered_ciphertext_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let mut bytes = std::fs::read(&path).expect("read");
        bytes[FILE_MAGIC.len() + SALT_SIZE + NONCE_SIZE] ^= 0xFF;
        std::fs::write(&path, &bytes).expect("write tampered");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn truncated_file_fails_to_load() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        std::fs::write(&path, [0u8; 10]).expect("write short");
        assert!(load(&path, "pw").is_err());
        std::fs::write(&path, [0u8; 28]).expect("write 28-byte");
        assert!(load(&path, "pw").is_err());
    }

    #[test]
    fn loaded_account_can_certify() {
        use crate::identity::device::DeviceIdentity;
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("account.keystore");
        save(&path, "pw", &Account::generate()).expect("save");

        let loaded = load(&path, "pw").expect("load");
        let device = DeviceIdentity::generate();
        assert!(loaded.certify(&device.public().ed25519_pub).verify());
    }
}

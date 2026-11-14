//! Optional local automatic sign-in credentials. The random encryption key lives
//! with its ciphertext: isolation is the OS user's private files, not Keychain.
//! Callers serialize save/load/clear through desktop authentication ownership.

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"MTLOGIN1";
const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
const HEADER: usize = MAGIC.len() + KEY_SIZE + NONCE_SIZE;
const MAX_PASSWORD: usize = 16 * 1024;
const MAX_FILE: usize = HEADER + MAX_PASSWORD + 16;

fn path(root: &Path, username: &str) -> PathBuf {
    root.join(format!(
        "{}.session",
        hex::encode(Sha256::digest(username.as_bytes()))
    ))
}

fn error(error: impl std::fmt::Display) -> String {
    format!("Local sign-in storage: {error}")
}

/// Reject aliases and non-private roots before accessing secrets. A parent is
/// supplied by the trusted desktop data-directory owner (not a renderer path).
fn directory(root: &Path, create: bool) -> Result<bool, String> {
    if create {
        let builder = &mut fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(error(e)),
        }
    }
    let metadata = match fs::symlink_metadata(root) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(error(e)),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(error("credential directory must be a real directory"));
    }
    private(&metadata)?;
    Ok(true)
}

fn private(metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // Same-user processes are intentionally outside this mode's isolation.
        if metadata.mode() & 0o077 != 0 || metadata.uid() != unsafe { libc::geteuid() } {
            return Err(error("credential permissions or owner are unsafe"));
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

fn check_file(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(error("credential must be a regular file"));
            }
            private(&metadata)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(error(e)),
    }
}

fn associated_data(username: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(MAGIC.len() + username.len());
    aad.extend_from_slice(MAGIC);
    aad.extend_from_slice(username.as_bytes());
    aad
}

pub(super) fn save(root: &Path, username: &str, password: &str) -> Result<(), String> {
    if password.len() > MAX_PASSWORD || username.len() > 1024 {
        return Err(error("credential exceeds size limit"));
    }
    directory(root, true)?;
    let destination = path(root, username);
    check_file(&destination)?;
    let mut key = Zeroizing::new([0; KEY_SIZE]);
    OsRng.try_fill_bytes(&mut *key).map_err(error)?;
    let mut nonce = [0; NONCE_SIZE];
    OsRng.try_fill_bytes(&mut nonce).map_err(error)?;
    let cipher = Aes256Gcm::new((&*key).into());
    let ciphertext = cipher
        .encrypt(
            &Nonce::try_from(nonce.as_slice()).map_err(error)?,
            Payload {
                msg: password.as_bytes(),
                aad: &associated_data(username),
            },
        )
        .map_err(|_| error("credential encryption failed"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(root).map_err(error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(error)?;
    }
    temporary.write_all(MAGIC).map_err(error)?;
    temporary.write_all(&*key).map_err(error)?;
    temporary.write_all(&nonce).map_err(error)?;
    temporary.write_all(&ciphertext).map_err(error)?;
    temporary.flush().map_err(error)?;
    temporary.as_file().sync_all().map_err(error)?;
    // Recheck after staging. Renaming never follows a destination symlink.
    directory(root, false)?;
    check_file(&destination)?;
    temporary.persist(&destination).map_err(error)?;
    sync_directory(root)?;
    Ok(())
}

pub(super) fn load(root: &Path, username: &str) -> Result<Option<String>, String> {
    if username.len() > 1024 {
        return Err(error("account exceeds size limit"));
    }
    if !directory(root, false)? {
        return Ok(None);
    }
    let location = path(root, username);
    if !check_file(&location)? {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(location).map_err(error)?;
    let metadata = file.metadata().map_err(error)?;
    if !metadata.is_file() {
        return Err(error("credential must be a regular file"));
    }
    private(&metadata)?;
    if metadata.len() > MAX_FILE as u64 {
        return Err(error("credential exceeds size limit"));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take((MAX_FILE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    if bytes.len() < HEADER + 16 || bytes.len() > MAX_FILE || &bytes[..MAGIC.len()] != MAGIC {
        return Err(error("invalid credential format"));
    }
    let cipher =
        Aes256Gcm::new_from_slice(&bytes[MAGIC.len()..MAGIC.len() + KEY_SIZE]).map_err(error)?;
    let nonce = Nonce::try_from(&bytes[MAGIC.len() + KEY_SIZE..HEADER]).map_err(error)?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &bytes[HEADER..],
                    aad: &associated_data(username),
                },
            )
            .map_err(|_| error("credential authentication failed"))?,
    );
    let password =
        std::str::from_utf8(&plaintext).map_err(|_| error("invalid credential encoding"))?;
    Ok(Some(password.to_owned()))
}

pub(super) fn clear(root: &Path, username: &str) -> Result<(), String> {
    if !directory(root, false)? {
        return Ok(());
    }
    let location = path(root, username);
    if check_file(&location)? {
        fs::remove_file(location).map_err(error)?;
        sync_directory(root)?;
    }
    Ok(())
}

fn sync_directory(root: &Path) -> Result<(), String> {
    #[cfg(unix)]
    fs::File::open(root)
        .and_then(|file| file.sync_all())
        .map_err(error)?;
    #[cfg(not(unix))]
    let _ = root;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_roundtrip_owner_binding_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        assert_eq!(load(&root, "alice").unwrap(), None);
        save(&root, "alice", " password 🐙 ").unwrap();
        assert_eq!(
            load(&root, "alice").unwrap().as_deref(),
            Some(" password 🐙 ")
        );
        assert_eq!(load(&root, "bob").unwrap(), None);
        std::fs::copy(path(&root, "alice"), path(&root, "bob")).unwrap();
        assert!(load(&root, "bob").is_err());
        clear(&root, "alice").unwrap();
        assert_eq!(load(&root, "alice").unwrap(), None);
        clear(&root, "alice").unwrap();
    }

    #[test]
    fn local_corruption_and_oversized_input_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        save(&root, "alice", "old").unwrap();
        assert!(save(&root, "alice", &"a".repeat(MAX_PASSWORD + 1)).is_err());
        assert_eq!(load(&root, "alice").unwrap().as_deref(), Some("old"));
        let location = path(&root, "alice");
        let mut data = std::fs::read(&location).unwrap();
        *data.last_mut().unwrap() ^= 1;
        std::fs::write(&location, data).unwrap();
        assert!(load(&root, "alice").is_err());
        std::fs::write(&location, vec![0; MAX_FILE + 1]).unwrap();
        assert!(load(&root, "alice").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn local_permissions_and_symlinks_are_enforced() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("sessions");
        save(&root, "alice", "secret").unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let location = path(&root, "alice");
        assert_eq!(
            std::fs::metadata(&location).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::set_permissions(&location, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&root, "alice").is_err());
        assert!(save(&root, "alice", "replacement").is_err());
        std::fs::set_permissions(&location, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(load(&root, "alice").is_err());
        assert!(save(&root, "alice", "replacement").is_err());
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let alias = temp.path().join("alias");
        symlink(&root, &alias).unwrap();
        assert!(load(&alias, "alice").is_err());
        assert!(save(&alias, "alice", "replacement").is_err());
        assert!(clear(&alias, "alice").is_err());
        std::fs::remove_file(&location).unwrap();
        let outside = temp.path().join("outside");
        std::fs::write(&outside, "untouched").unwrap();
        symlink(&outside, &location).unwrap();
        assert!(load(&root, "alice").is_err());
        assert!(save(&root, "alice", "replacement").is_err());
        assert!(clear(&root, "alice").is_err());
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
    }
}

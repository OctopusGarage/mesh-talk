//! Durable local disclosure permissions. Hosts serialize access to this store;
//! public admission for a verified account ID is
//! `!snapshot.invisible || policy.allows(account_id)`.
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 6] = b"MTPRV1";
const MAX_BYTES: u64 = 1_048_576;
const HEADER: usize = MAGIC.len() + SALT_SIZE + NONCE_SIZE;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid privacy policy")
}
fn crypto_error(_: impl std::fmt::Display) -> io::Error {
    invalid()
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES)
        .reject_trailing_bytes()
}
fn check_path(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_file() => Ok(true),
        Ok(_) => Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionSource {
    Manual,
    Initiated,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowedAccount {
    pub id: String,
    pub name: String,
    pub source: PermissionSource,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacySnapshot {
    pub version: u32,
    pub invisible: bool,
    pub allowed_accounts: Vec<AllowedAccount>,
}
/// One encrypted policy file; mutations commit to disk before changing memory.
///
/// The host must maintain one live owner per file and serialize access: this
/// store has no cross-process locking. Its parent directory must be trusted;
/// path checks reject existing symlinks but do not defend against filesystem
/// changes racing those checks. Admission decisions require account IDs verified
/// by the host's authentication layer.
pub struct PrivacyPolicy {
    path: PathBuf,
    salt: [u8; SALT_SIZE],
    key: EncryptionKey,
    snapshot: PrivacySnapshot,
}
impl PrivacyPolicy {
    pub fn open(path: &Path, password: &str) -> io::Result<Self> {
        if !check_path(path)? {
            let salt = generate_salt();
            let p = Self {
                path: path.into(),
                salt,
                key: EncryptionKey::from_password(password, &salt).map_err(crypto_error)?,
                snapshot: PrivacySnapshot {
                    version: 1,
                    invisible: false,
                    allowed_accounts: Vec::new(),
                },
            };
            p.save(&p.snapshot)?;
            return Ok(p);
        }
        let mut bytes = Vec::new();
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() < HEADER + 16
            || bytes.len() as u64 > MAX_BYTES
            || &bytes[..MAGIC.len()] != MAGIC
        {
            return Err(invalid());
        }
        let salt = bytes[MAGIC.len()..MAGIC.len() + SALT_SIZE]
            .try_into()
            .map_err(crypto_error)?;
        let nonce = bytes[MAGIC.len() + SALT_SIZE..HEADER]
            .try_into()
            .map_err(crypto_error)?;
        let key = EncryptionKey::from_password(password, &salt).map_err(crypto_error)?;
        let plaintext = decrypt_data(&bytes[HEADER..], &nonce, &key).map_err(crypto_error)?;
        let snapshot: PrivacySnapshot = codec().deserialize(&plaintext).map_err(crypto_error)?;
        if snapshot.version != 1 || snapshot.allowed_accounts.len() > 1024 {
            return Err(invalid());
        }
        let mut previous: Option<&str> = None;
        for account in &snapshot.allowed_accounts {
            if !valid_id(&account.id)
                || previous.is_some_and(|p| p >= account.id.as_str())
                || account.name.chars().count() > 256
                || account.name.chars().any(char::is_control)
            {
                return Err(invalid());
            }
            previous = Some(&account.id);
        }
        Ok(Self {
            path: path.into(),
            salt,
            key,
            snapshot,
        })
    }
    pub fn snapshot(&self) -> PrivacySnapshot {
        self.snapshot.clone()
    }
    /// Returns allowlist membership independently of the current visibility mode.
    pub fn allows(&self, id: &str) -> bool {
        valid_id(id)
            && self
                .snapshot
                .allowed_accounts
                .binary_search_by(|a| a.id.as_str().cmp(id))
                .is_ok()
    }
    pub fn set_invisible(&mut self, invisible: bool) -> io::Result<()> {
        let mut next = self.snapshot.clone();
        next.invisible = invisible;
        self.commit(next)
    }
    pub fn grant(&mut self, id: &str, name: &str, source: PermissionSource) -> io::Result<()> {
        if !valid_id(id) {
            return Err(invalid());
        }
        let name: String = name.chars().filter(|c| !c.is_control()).take(256).collect();
        let mut next = self.snapshot.clone();
        match next
            .allowed_accounts
            .binary_search_by(|a| a.id.as_str().cmp(id))
        {
            Ok(i) => {
                let a = &mut next.allowed_accounts[i];
                if !name.is_empty() {
                    a.name = name;
                }
                if source == PermissionSource::Manual {
                    a.source = source;
                }
            }
            Err(i) => {
                if next.allowed_accounts.len() == 1024 {
                    return Err(invalid());
                }
                next.allowed_accounts.insert(
                    i,
                    AllowedAccount {
                        id: id.into(),
                        name,
                        source,
                    },
                );
            }
        }
        self.commit(next)
    }
    pub fn revoke(&mut self, id: &str) -> io::Result<()> {
        if !valid_id(id) {
            return Err(invalid());
        }
        let mut next = self.snapshot.clone();
        if let Ok(i) = next
            .allowed_accounts
            .binary_search_by(|a| a.id.as_str().cmp(id))
        {
            next.allowed_accounts.remove(i);
        }
        self.commit(next)
    }
    fn commit(&mut self, next: PrivacySnapshot) -> io::Result<()> {
        if next == self.snapshot {
            return Ok(());
        }
        self.save(&next)?;
        self.snapshot = next;
        Ok(())
    }
    fn save(&self, snapshot: &PrivacySnapshot) -> io::Result<()> {
        let plaintext = codec().serialize(snapshot).map_err(crypto_error)?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key).map_err(crypto_error)?;
        if HEADER + ciphertext.len() > MAX_BYTES as usize {
            return Err(invalid());
        }
        check_path(&self.path)?;
        let directory = parent(&self.path);
        let mut tmp = tempfile::NamedTempFile::new_in(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(MAGIC)?;
        tmp.write_all(&self.salt)?;
        tmp.write_all(&nonce)?;
        tmp.write_all(&ciphertext)?;
        tmp.as_file().sync_all()?;
        check_path(&self.path)?;
        tmp.persist(&self.path).map_err(|e| e.error)?;
        // Replacement is the commit point. A directory-sync failure must not
        // report rollback after the new file has already become visible.
        if let Ok(directory) = File::open(directory) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grant_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let id = "a".repeat(32);
        let mut policy = PrivacyPolicy::open(&path, "pw").unwrap();
        policy
            .grant(&id, "Alice", PermissionSource::Manual)
            .unwrap();
        let reopened = PrivacyPolicy::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.snapshot().allowed_accounts,
            vec![AllowedAccount {
                id,
                name: "Alice".into(),
                source: PermissionSource::Manual
            }]
        );
    }

    #[test]
    fn mode_revoke_sources_names_and_noops() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let mut p = PrivacyPolicy::open(&path, "pw").unwrap();
        let id = "a".repeat(32);
        assert!(!p.allows(&id));
        p.set_invisible(true).unwrap();
        p.grant(&id, "A\n\0lice", PermissionSource::Manual).unwrap();
        p.grant(&id, "", PermissionSource::Initiated).unwrap();
        assert!(p.allows(&id));
        assert_eq!(p.snapshot().allowed_accounts[0].name, "Alice");
        assert_eq!(
            p.snapshot().allowed_accounts[0].source,
            PermissionSource::Manual
        );
        let bytes = std::fs::read(&path).unwrap();
        p.set_invisible(true).unwrap();
        p.grant(&id, "Alice", PermissionSource::Manual).unwrap();
        p.revoke(&"b".repeat(32)).unwrap();
        assert_eq!(bytes, std::fs::read(&path).unwrap());
        assert!(
            PrivacyPolicy::open(&path, "pw")
                .unwrap()
                .snapshot()
                .invisible
        );
        p.revoke(&id).unwrap();
        assert!(!PrivacyPolicy::open(&path, "pw").unwrap().allows(&id));
        p.grant(&id, &"界".repeat(300), PermissionSource::Initiated)
            .unwrap();
        assert_eq!(p.snapshot().allowed_accounts[0].name.chars().count(), 256);
        p.grant(&id, "", PermissionSource::Manual).unwrap();
        assert_eq!(
            p.snapshot().allowed_accounts[0].source,
            PermissionSource::Manual
        );
    }

    #[test]
    fn validates_ids_order_and_capacity() {
        let root = tempfile::tempdir().unwrap();
        let mut p = PrivacyPolicy::open(&root.path().join("privacy"), "pw").unwrap();
        for id in [
            "",
            "abc",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "gggggggggggggggggggggggggggggggg",
        ] {
            assert!(p.grant(id, "name", PermissionSource::Manual).is_err());
            assert!(p.revoke(id).is_err());
            assert!(!p.allows(id));
        }
        for n in (0..3).rev() {
            p.grant(&format!("{n:032x}"), "name", PermissionSource::Initiated)
                .unwrap();
        }
        assert_eq!(p.snapshot().allowed_accounts[0].id, "0".repeat(32));
        let mut full = p.snapshot();
        full.allowed_accounts = (0..1024)
            .map(|n| AllowedAccount {
                id: format!("{n:032x}"),
                name: "name".into(),
                source: PermissionSource::Initiated,
            })
            .collect();
        p.commit(full).unwrap();
        assert!(p
            .grant(&format!("{:032x}", 1024), "", PermissionSource::Manual)
            .is_err());
        p.grant(&"0".repeat(32), "updated", PermissionSource::Manual)
            .unwrap();
    }

    #[test]
    fn roots_are_isolated_and_wrong_password_tamper_fail() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        let mut p = PrivacyPolicy::open(&a, "pw").unwrap();
        p.set_invisible(true).unwrap();
        assert!(!PrivacyPolicy::open(&b, "pw").unwrap().snapshot().invisible);
        assert!(PrivacyPolicy::open(&a, "wrong").is_err());
        let mut bytes = std::fs::read(&a).unwrap();
        assert!(bytes.starts_with(b"MTPRV1"));
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        std::fs::write(&a, bytes).unwrap();
        assert!(PrivacyPolicy::open(&a, "pw").is_err());
        for bytes in [
            vec![],
            b"MTPRV1short".to_vec(),
            vec![0; 40],
            vec![0; 1_048_577],
        ] {
            std::fs::write(&a, bytes).unwrap();
            assert!(PrivacyPolicy::open(&a, "pw").is_err());
        }
        assert!(PrivacyPolicy::open(root.path(), "pw").is_err());
    }

    #[test]
    fn authenticated_invalid_snapshots_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let p = PrivacyPolicy::open(&path, "pw").unwrap();
        let a = AllowedAccount {
            id: "a".repeat(32),
            name: "Alice".into(),
            source: PermissionSource::Manual,
        };
        let write_plain = |plain: &[u8]| {
            let (cipher, nonce) = encrypt_data(plain, &p.key).unwrap();
            let mut bytes = MAGIC.to_vec();
            bytes.extend(p.salt);
            bytes.extend(nonce);
            bytes.extend(cipher);
            std::fs::write(&path, bytes).unwrap();
        };
        for snapshot in [
            PrivacySnapshot {
                version: 2,
                invisible: false,
                allowed_accounts: vec![],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![a.clone(), a.clone()],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![AllowedAccount {
                    id: "invalid".into(),
                    ..a.clone()
                }],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![a.clone(); 1025],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![AllowedAccount {
                    name: "bad\n".into(),
                    ..a.clone()
                }],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![AllowedAccount {
                    name: "界".repeat(257),
                    ..a.clone()
                }],
            },
            PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: vec![
                    AllowedAccount {
                        id: "b".repeat(32),
                        ..a.clone()
                    },
                    a.clone(),
                ],
            },
        ] {
            write_plain(&bincode::serialize(&snapshot).unwrap());
            assert!(PrivacyPolicy::open(&path, "pw").is_err());
        }
        let mut plain = bincode::serialize(&p.snapshot()).unwrap();
        plain.push(0);
        write_plain(&plain);
        assert!(PrivacyPolicy::open(&path, "pw").is_err());
        let mut plain = bincode::serialize(&PrivacySnapshot {
            version: 1,
            invisible: false,
            allowed_accounts: vec![a],
        })
        .unwrap();
        let end = plain.len();
        plain[end - 4..].copy_from_slice(&2_u32.to_le_bytes());
        write_plain(&plain);
        assert!(PrivacyPolicy::open(&path, "pw").is_err());
    }

    #[test]
    fn invalid_destination_keeps_memory_and_previous_file() {
        let root = tempfile::tempdir().unwrap();
        let original = root.path().join("original");
        let mut p = PrivacyPolicy::open(&original, "pw").unwrap();
        let before = p.snapshot();
        let bytes = std::fs::read(&original).unwrap();
        // A nonempty directory cannot be atomically replaced by a regular file.
        let blocker = root.path().join("blocker");
        std::fs::create_dir(&blocker).unwrap();
        std::fs::write(blocker.join("child"), "keep").unwrap();
        p.path = blocker;
        assert!(p.set_invisible(true).is_err());
        assert_eq!(p.snapshot(), before);
        assert_eq!(std::fs::read(&original).unwrap(), bytes);
        assert_eq!(
            PrivacyPolicy::open(&original, "pw").unwrap().snapshot(),
            before
        );
    }

    #[test]
    fn encryption_hides_contacts_and_successful_changes_refresh_nonce() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let mut p = PrivacyPolicy::open(&path, "pw").unwrap();
        let id = "123456789abcdef0123456789abcdef0";
        let name = "distinctive private contact name";
        p.grant(id, name, PermissionSource::Manual).unwrap();
        let before = fs::read(&path).unwrap();
        for secret in [id, name] {
            assert!(!before.windows(secret.len()).any(|w| w == secret.as_bytes()));
        }
        p.set_invisible(true).unwrap();
        let after = fs::read(&path).unwrap();
        assert_eq!(&before[6..22], &after[6..22]);
        assert_ne!(&before[22..34], &after[22..34]);
    }

    #[test]
    fn oversized_update_keeps_memory_and_committed_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let mut p = PrivacyPolicy::open(&path, "pw").unwrap();
        let mut snapshot = p.snapshot();
        snapshot.allowed_accounts = (0..1024)
            .map(|n| AllowedAccount {
                id: format!("{n:032x}"),
                name: if n < 971 {
                    "😀".repeat(256)
                } else {
                    String::new()
                },
                source: PermissionSource::Manual,
            })
            .collect();
        p.commit(snapshot).unwrap();
        let before = p.snapshot();
        let bytes = fs::read(&path).unwrap();
        let mut candidate = before.clone();
        candidate.allowed_accounts[971].name = "😀".repeat(241);
        let encoded = codec().serialize(&candidate).unwrap();
        assert!(encoded.len() as u64 <= MAX_BYTES);
        assert!((HEADER + encoded.len() + 16) as u64 > MAX_BYTES);
        assert!(p
            .grant(
                &format!("{:032x}", 971),
                &"😀".repeat(241),
                PermissionSource::Manual
            )
            .is_err());
        assert_eq!(p.snapshot(), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(PrivacyPolicy::open(&path, "pw").unwrap().snapshot(), before);
    }

    #[cfg(unix)]
    #[test]
    fn failed_save_in_readonly_parent_keeps_committed_file() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        struct Restore(PathBuf);
        impl Drop for Restore {
            fn drop(&mut self) {
                fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let mut p = PrivacyPolicy::open(&path, "pw").unwrap();
        let before = p.snapshot();
        let bytes = fs::read(&path).unwrap();
        let restore = Restore(root.path().into());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o500)).unwrap();
        assert!(p.set_invisible(true).is_err());
        assert_eq!(p.snapshot(), before);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(PrivacyPolicy::open(&path, "pw").unwrap().snapshot(), before);
        drop(restore);
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_and_symlinks_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("privacy");
        let mut p = PrivacyPolicy::open(&path, "pw").unwrap();
        p.set_invisible(true).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(PrivacyPolicy::open(&link, "pw").is_err());
        let dangling = root.path().join("dangling");
        symlink(root.path().join("missing"), &dangling).unwrap();
        assert!(PrivacyPolicy::open(&dangling, "pw").is_err());
    }
}

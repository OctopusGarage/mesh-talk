//! Verified historical device/account proofs, without IP addresses or endpoint freshness.
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use crate::{discovery::Announce, identity::device::PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 6] = b"MTPDR1";
const MAX_BYTES: u64 = 1_048_576;
const MAX_PEERS: usize = 1024;
const HEADER: usize = MAGIC.len() + SALT_SIZE + NONCE_SIZE;
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid peer directory")
}
fn invalid_error(_: impl std::fmt::Display) -> io::Error {
    invalid()
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
fn valid(announce: &Announce) -> bool {
    announce.name.len() <= 1024
        && announce.tcp_port != 0
        && announce.account_cert.is_some()
        && announce.verify()
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    records: Vec<Announce>,
}

/// One serialized owner per file, in a trusted parent directory, as with PrivacyPolicy.
/// Records establish historical author/account bindings, never a fresh dial endpoint.
/// Existing symlinks are rejected; filesystem races in an untrusted parent are out of scope.
pub(crate) struct PeerDirectory {
    path: PathBuf,
    salt: [u8; SALT_SIZE],
    key: EncryptionKey,
    records: Vec<Announce>,
}
impl PeerDirectory {
    pub(crate) fn open(path: &Path, password: &str) -> io::Result<Self> {
        if !check_path(path)? {
            let salt = generate_salt();
            let directory = Self {
                path: path.into(),
                salt,
                key: EncryptionKey::from_password(password, &salt).map_err(invalid_error)?,
                records: Vec::new(),
            };
            directory.save(&[])?;
            return Ok(directory);
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() < HEADER + 16
            || bytes.len() as u64 > MAX_BYTES
            || &bytes[..MAGIC.len()] != MAGIC
        {
            return Err(invalid());
        }
        let salt = bytes[MAGIC.len()..MAGIC.len() + SALT_SIZE]
            .try_into()
            .map_err(invalid_error)?;
        let nonce = bytes[MAGIC.len() + SALT_SIZE..HEADER]
            .try_into()
            .map_err(invalid_error)?;
        let key = EncryptionKey::from_password(password, &salt).map_err(invalid_error)?;
        let plaintext = decrypt_data(&bytes[HEADER..], &nonce, &key).map_err(invalid_error)?;
        let snapshot: Snapshot = codec().deserialize(&plaintext).map_err(invalid_error)?;
        if snapshot.version != 1 || snapshot.records.len() > MAX_PEERS {
            return Err(invalid());
        }
        let mut previous: Option<&str> = None;
        for record in &snapshot.records {
            if !valid(record) || previous.is_some_and(|p| p >= record.user_id.as_str()) {
                return Err(invalid());
            }
            previous = Some(&record.user_id);
        }
        Ok(Self {
            path: path.into(),
            salt,
            key,
            records: snapshot.records,
        })
    }
    pub(crate) fn record(&mut self, announce: &Announce) -> io::Result<()> {
        self.record_bound(announce, false)
    }
    /// Trusted local keystore adoption only. The Node supplies its own exact
    /// public identity; remote announcements never use this migration path.
    pub(in crate::node) fn record_own(
        &mut self,
        announce: &Announce,
        own: &PublicIdentity,
    ) -> io::Result<()> {
        if announce.public() != *own {
            return Err(invalid());
        }
        self.record_bound(announce, true)
    }
    /// Explicit user acceptance of a verified account binding. Never used by
    /// discovery, inbound authentication, relay ingestion or background probes.
    /// A device rehome is permitted, but its complete Ed/X identity stays pinned.
    pub(in crate::node) fn record_explicit_account(
        &mut self,
        announce: &Announce,
    ) -> io::Result<()> {
        self.record_bound(announce, true)
    }
    fn record_bound(&mut self, announce: &Announce, accept_account_change: bool) -> io::Result<()> {
        if !valid(announce) {
            return Err(invalid());
        }
        let mut next = self.records.clone();
        match next.binary_search_by(|a| a.user_id.cmp(&announce.user_id)) {
            Ok(i) => {
                let existing = &next[i];
                if existing.ed25519_pub != announce.ed25519_pub
                    || existing.x25519_pub != announce.x25519_pub
                    || (!accept_account_change
                        && existing
                            .account_cert
                            .as_ref()
                            .map(|c| c.account_ed25519_pub)
                            != announce
                                .account_cert
                                .as_ref()
                                .map(|c| c.account_ed25519_pub))
                {
                    return Err(invalid());
                }
                if existing == announce {
                    return Ok(());
                }
                next[i] = announce.clone();
            }
            Err(i) => {
                if next.len() == MAX_PEERS {
                    return Err(invalid());
                }
                next.insert(i, announce.clone());
            }
        }
        self.save(&next)?;
        self.records = next;
        Ok(())
    }
    pub(crate) fn account_for(&self, public: &PublicIdentity) -> Option<String> {
        let i = self
            .records
            .binary_search_by(|a| a.user_id.cmp(&public.user_id()))
            .ok()?;
        let record = &self.records[i];
        (record.public() == *public)
            .then(|| record.account_id())
            .flatten()
    }
    pub(crate) fn by_author(&self, author: &[u8; 32]) -> Option<Announce> {
        let id = PublicIdentity::user_id_from(author);
        let i = self.records.binary_search_by(|a| a.user_id.cmp(&id)).ok()?;
        let record = &self.records[i];
        (record.ed25519_pub == *author).then(|| record.clone())
    }
    pub(crate) fn announcements(&self) -> Vec<Announce> {
        self.records.clone()
    }
    pub(crate) fn next_after(&self, cursor: Option<&str>) -> Option<Announce> {
        let index = cursor.map_or(0, |id| {
            self.records.partition_point(|a| a.user_id.as_str() <= id)
        });
        self.records
            .get(index)
            .or_else(|| self.records.first())
            .cloned()
    }
    fn save(&self, records: &[Announce]) -> io::Result<()> {
        let plaintext = codec().serialize(&(1u32, records)).map_err(invalid_error)?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key).map_err(invalid_error)?;
        if HEADER + ciphertext.len() > MAX_BYTES as usize {
            return Err(invalid());
        }
        check_path(&self.path)?;
        let directory = self
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
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
        // Rename is the commit point; post-commit directory sync is best effort.
        if let Ok(directory) = File::open(directory) {
            let _ = directory.sync_all();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[test]
    fn explicit_account_rehome_preserves_full_identity_and_signature_checks() {
        let dir = tempfile::tempdir().unwrap();
        let mut proofs = PeerDirectory::open(&dir.path().join("proofs"), "pw").unwrap();
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let new = Account::generate();
        let first = Announce::new_with_account(&device, &old, "Device", 1234);
        let updated = Announce::new_with_account(&device, &new, "Device", 1234);
        proofs.record(&first).unwrap();
        assert!(proofs.record(&updated).is_err());
        let mut tampered = updated.clone();
        tampered.tcp_port += 1;
        assert!(proofs.record_explicit_account(&tampered).is_err());
        let keys = device.secret_bytes();
        let different_x =
            DeviceIdentity::from_secret_bytes(keys.0, DeviceIdentity::generate().secret_bytes().1);
        assert!(proofs
            .record_explicit_account(&Announce::new_with_account(
                &different_x,
                &new,
                "Device",
                1234
            ))
            .is_err());
        proofs.record_explicit_account(&updated).unwrap();
        assert_eq!(proofs.account_for(&device.public()), Some(new.account_id()));
        assert!(
            proofs.record(&first).is_err(),
            "passive old binding must stay rejected after explicit rehome"
        );
    }

    fn write_snapshot(path: &Path, records: &[Announce], trailing: bool) {
        use crate::storage::encryption::{encrypt_data, generate_salt, EncryptionKey};
        use bincode::Options;
        let mut plaintext = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&(1u32, records))
            .unwrap();
        if trailing {
            plaintext.push(0);
        }
        let salt = generate_salt();
        let key = EncryptionKey::from_password("pw", &salt).unwrap();
        let (ciphertext, nonce) = encrypt_data(&plaintext, &key).unwrap();
        let mut bytes = b"MTPDR1".to_vec();
        bytes.extend(salt);
        bytes.extend(nonce);
        bytes.extend(ciphertext);
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn reopen_reverifies_every_record_and_rejects_order_duplicates_and_trailing_data() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let account = Account::generate();
        let mut records: Vec<_> = (0..2)
            .map(|_| {
                Announce::new_with_account(&DeviceIdentity::generate(), &account, "Alice", 1234)
            })
            .collect();
        records.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        write_snapshot(&path, &records, false);
        assert!(PeerDirectory::open(&path, "pw").is_ok());
        write_snapshot(&path, &records, true);
        assert!(PeerDirectory::open(&path, "pw").is_err());
        let mut invalid = records.clone();
        invalid[1].name.push('!');
        write_snapshot(&path, &invalid, false);
        assert!(PeerDirectory::open(&path, "pw").is_err());
        write_snapshot(&path, &[records[0].clone(), records[0].clone()], false);
        assert!(PeerDirectory::open(&path, "pw").is_err());
        records.reverse();
        write_snapshot(&path, &records, false);
        assert!(PeerDirectory::open(&path, "pw").is_err());
    }

    #[test]
    fn record_and_snapshot_bounds_preserve_last_committed_state() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let account = Account::generate();
        let mut records: Vec<_> = (0..1024)
            .map(|_| {
                Announce::new_with_account(&DeviceIdentity::generate(), &account, "Alice", 1234)
            })
            .collect();
        records.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        write_snapshot(&path, &records, false);
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        let original = std::fs::read(&path).unwrap();
        let extra =
            Announce::new_with_account(&DeviceIdentity::generate(), &account, "Alice", 1234);
        assert!(directory.record(&extra).is_err());
        assert_eq!(directory.announcements().len(), 1024);
        assert_eq!(original, std::fs::read(&path).unwrap());
        records.push(extra);
        records.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        write_snapshot(&path, &records, false);
        assert!(PeerDirectory::open(&path, "pw").is_err());
        std::fs::write(&path, vec![0; 1_048_577]).unwrap();
        assert!(PeerDirectory::open(&path, "pw").is_err());
    }

    #[test]
    fn encrypted_output_limit_rejects_update_without_changing_disk_or_memory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let account = Account::generate();
        let mut fixtures: Vec<_> = (0..1024)
            .map(|_| {
                let device = DeviceIdentity::generate();
                let announce = Announce::new_with_account(&device, &account, "", 1234);
                (device, announce)
            })
            .collect();
        fixtures.sort_by(|a, b| a.1.user_id.cmp(&b.1.user_id));
        let base: Vec<_> = fixtures.iter().map(|(_, a)| a.clone()).collect();
        let base_size = codec().serialize(&(1u32, &base)).unwrap().len();
        // Fill signed names to leave exactly one byte of encrypted-file slack.
        let mut budget = MAX_BYTES as usize - HEADER - 16 - base_size - 1;
        let mut records = Vec::new();
        for (device, _) in &fixtures {
            let length = budget.min(1024);
            budget -= length;
            records.push(Announce::new_with_account(
                device,
                &account,
                "a".repeat(length),
                1234,
            ));
        }
        assert_eq!(budget, 0);
        write_snapshot(&path, &records, false);
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        let original = std::fs::read(&path).unwrap();
        assert_eq!(original.len(), MAX_BYTES as usize - 1);
        let index = records.iter().position(|a| a.name.len() < 1022).unwrap();
        let update = Announce::new_with_account(
            &fixtures[index].0,
            &account,
            "a".repeat(records[index].name.len() + 2),
            1234,
        );
        assert!(directory.record(&update).is_err());
        assert_eq!(original, std::fs::read(&path).unwrap());
        assert_eq!(directory.announcements(), records);
    }

    #[test]
    fn rejects_forgery_tamper_missing_certificate_zero_port_and_oversized_name() {
        let root = tempfile::tempdir().unwrap();
        let mut directory = PeerDirectory::open(&root.path().join("peers"), "pw").unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let valid = Announce::new_with_account(&device, &account, "Alice", 1234);
        let mut forged = valid.clone();
        forged.account_cert = Some(Account::generate().certify(&valid.ed25519_pub));
        let mut tampered = valid.clone();
        tampered.x25519_pub[0] ^= 1;
        for invalid in [
            forged,
            tampered,
            Announce::new(&device, "Alice", 1234),
            Announce::new_with_account(&device, &account, "Alice", 0),
            Announce::new_with_account(&device, &account, "界".repeat(342), 1234),
        ] {
            assert!(directory.record(&invalid).is_err());
        }
        assert!(directory.announcements().is_empty());
        directory
            .record(&Announce::new_with_account(
                &device,
                &account,
                "a".repeat(1024),
                1234,
            ))
            .unwrap();
    }

    #[test]
    fn sticky_account_updates_noops_and_failed_save_preserve_memory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let original = Announce::new_with_account(&device, &account, "Alice", 1234);
        directory.record(&original).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        directory.record(&original).unwrap();
        assert_eq!(bytes, std::fs::read(&path).unwrap());
        let conflict = Announce::new_with_account(&device, &Account::generate(), "Alice", 1234);
        assert!(directory.record(&conflict).is_err());
        assert_eq!(bytes, std::fs::read(&path).unwrap());
        let update = Announce::new_with_account(&device, &account, "Alice\nnew", 4321);
        directory.record(&update).unwrap();
        assert_eq!(
            directory.by_author(&original.ed25519_pub),
            Some(update.clone())
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(directory.record(&original).is_err());
        assert_eq!(directory.by_author(&original.ed25519_pub), Some(update));
    }

    #[test]
    fn wrong_password_corruption_nonregular_and_encryption() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        let announce = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "unique private display name",
            1234,
        );
        directory.record(&announce).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        assert!(!bytes
            .windows(announce.name.len())
            .any(|s| s == announce.name.as_bytes()));
        assert!(PeerDirectory::open(&path, "wrong").is_err());
        *bytes.last_mut().unwrap() ^= 1;
        std::fs::write(&path, bytes).unwrap();
        assert!(PeerDirectory::open(&path, "pw").is_err());
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(PeerDirectory::open(&path, "pw").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_and_symlink_rejection() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let target = root.path().join("target");
        std::fs::rename(&path, &target).unwrap();
        symlink(&target, &path).unwrap();
        assert!(PeerDirectory::open(&path, "pw").is_err());
        assert!(directory
            .record(&Announce::new_with_account(
                &DeviceIdentity::generate(),
                &Account::generate(),
                "Alice",
                1234
            ))
            .is_err());
        assert!(directory.announcements().is_empty());
    }

    #[test]
    fn verified_binding_survives_reopen_and_requires_exact_keys() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("peers");
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let announce = Announce::new_with_account(&device, &account, "Alice", 1234);
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        directory.record(&announce).unwrap();
        let reopened = PeerDirectory::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.account_for(&device.public()),
            Some(account.account_id())
        );
        let mut wrong = device.public();
        wrong.x25519_pub[0] ^= 1;
        assert_eq!(reopened.account_for(&wrong), None);
        assert_eq!(
            reopened.by_author(&announce.ed25519_pub),
            Some(announce.clone())
        );
        assert_eq!(reopened.announcements(), vec![announce]);
    }
}

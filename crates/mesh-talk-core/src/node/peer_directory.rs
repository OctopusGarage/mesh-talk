//! Verified historical device/account proofs, without IP addresses or endpoint freshness.
use crate::identity::account_transition::AccountBindingTransition;
use crate::storage::encryption::{
    EncryptionKey, NONCE_SIZE, SALT_SIZE, decrypt_data, encrypt_data, generate_salt,
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
    announce.valid_presence() && announce.account_cert.is_some()
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    records: Vec<Announce>,
}

#[derive(Clone, Serialize, Deserialize)]
struct TransitionHead {
    public: PublicIdentity,
    id: [u8; 32],
    transitions: Vec<AccountBindingTransition>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct AccountBindingPatch {
    pub(super) announce: Announce,
    pub(super) transitions: Vec<AccountBindingTransition>,
}
#[derive(Serialize, Deserialize)]
struct SnapshotV2 {
    version: u32,
    records: Vec<Announce>,
    heads: Vec<TransitionHead>,
}

/// One serialized owner per file, in a trusted parent directory, as with PrivacyPolicy.
/// Records establish historical author/account bindings, never a fresh dial endpoint.
/// Existing symlinks are rejected; filesystem races in an untrusted parent are out of scope.
pub(crate) struct PeerDirectory {
    path: PathBuf,
    salt: [u8; SALT_SIZE],
    key: EncryptionKey,
    records: Vec<Announce>,
    heads: Vec<TransitionHead>,
    // Reconstructed from the durable local account-device registry at startup.
    linked_bindings: Vec<super::account_devices::AccountDevice>,
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
                heads: Vec::new(),
                linked_bindings: Vec::new(),
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
        let (snapshot, heads) = match plaintext.get(..4) {
            Some([1, 0, 0, 0]) => {
                let snapshot: Snapshot = codec().deserialize(&plaintext).map_err(invalid_error)?;
                (snapshot, Vec::new())
            }
            Some([2, 0, 0, 0]) => {
                let snapshot: SnapshotV2 =
                    codec().deserialize(&plaintext).map_err(invalid_error)?;
                (
                    Snapshot {
                        version: 1,
                        records: snapshot.records,
                    },
                    snapshot.heads,
                )
            }
            _ => return Err(invalid()),
        };
        if heads.len() > MAX_PEERS
            || heads
                .windows(2)
                .any(|w| w[0].public.user_id() >= w[1].public.user_id())
            || heads.iter().any(|h| {
                h.transitions.is_empty()
                    || h.transitions.len() > 8
                    || h.transitions
                        .first()
                        .is_some_and(|t| t.previous_transition.is_some())
                    || h.transitions.last().is_none_or(|t| t.id() != h.id)
                    || h.transitions
                        .iter()
                        .any(|t| t.public != h.public || !t.verify())
                    || h.transitions.windows(2).any(|w| {
                        w[1].previous_transition != Some(w[0].id())
                            || w[1].previous_certificate != w[0].current_certificate
                    })
                    || !snapshot.records.iter().any(|r| r.public() == h.public)
            })
        {
            return Err(invalid());
        }
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
            heads,
            linked_bindings: Vec::new(),
        })
    }
    pub(super) fn transition_patches(&self) -> Vec<AccountBindingPatch> {
        self.heads
            .iter()
            .filter_map(|h| {
                let announce = self.by_author(&h.public.ed25519_pub)?;
                (announce.account_cert.as_ref()
                    == h.transitions.last().map(|t| &t.current_certificate))
                .then(|| AccountBindingPatch {
                    announce,
                    transitions: h.transitions.clone(),
                })
            })
            .collect()
    }
    pub(in crate::node) fn transition_candidate(
        &self,
        announce: &Announce,
        chain: &[AccountBindingTransition],
    ) -> Option<String> {
        if !valid(announce) || announce.post_office || chain.is_empty() || chain.len() > 8 {
            return None;
        }
        let old = self.by_author(&announce.ed25519_pub)?;
        if old.post_office
            || old.public() != announce.public()
            || old.account_id() == announce.account_id()
        {
            return None;
        }
        if self.linked_binding(&announce.user_id).is_some_and(|d| {
            d.public != announce.public() || Some(&d.certificate) != announce.account_cert.as_ref()
        }) {
            return None;
        }
        let mut roots = Vec::new();
        for (i, t) in chain.iter().enumerate() {
            if !t.verify() || t.public != announce.public() {
                return None;
            }
            if i == 0 {
                roots.push(t.previous_certificate.account_ed25519_pub);
            } else if t.previous_transition != Some(chain[i - 1].id())
                || t.previous_certificate != chain[i - 1].current_certificate
            {
                return None;
            }
            if roots.contains(&t.current_certificate.account_ed25519_pub) {
                return None;
            }
            roots.push(t.current_certificate.account_ed25519_pub);
        }
        if Some(&chain.last()?.current_certificate) != announce.account_cert.as_ref() {
            return None;
        }
        let start = chain
            .iter()
            .position(|t| Some(&t.previous_certificate) == old.account_cert.as_ref())?;
        match self.heads.iter().find(|h| h.public == announce.public()) {
            Some(head) if chain[start].previous_transition != Some(head.id) => return None,
            None if start == 0 && chain[0].previous_transition.is_some() => return None,
            _ => {}
        }
        old.account_id()
    }
    pub(in crate::node) fn record_transition(
        &mut self,
        announce: &Announce,
        chain: &[AccountBindingTransition],
    ) -> io::Result<()> {
        if self.transition_candidate(announce, chain).is_none() {
            return if self
                .by_author(&announce.ed25519_pub)
                .as_ref()
                .is_some_and(|old| {
                    old.public() == announce.public() && old.account_cert == announce.account_cert
                })
                && chain.last().is_some_and(|t| {
                    self.heads
                        .iter()
                        .any(|h| h.public == announce.public() && h.id == t.id())
                }) {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        let mut records = self.records.clone();
        let index = records
            .binary_search_by(|r| r.user_id.cmp(&announce.user_id))
            .map_err(invalid_error)?;
        records[index] = announce.clone();
        let mut heads = self.heads.clone();
        heads.retain(|h| h.public != announce.public());
        let transitions = if chain[0].previous_transition.is_none() {
            chain.to_vec()
        } else {
            let mut prefix = self
                .heads
                .iter()
                .find(|h| h.public == announce.public())
                .ok_or_else(invalid)?
                .transitions
                .clone();
            prefix.extend_from_slice(chain);
            if prefix.len() > 8 {
                return Err(invalid());
            }
            prefix
        };
        heads.push(TransitionHead {
            public: announce.public(),
            id: chain.last().ok_or_else(invalid)?.id(),
            transitions,
        });
        heads.sort_by_key(|h| h.public.user_id());
        self.save_with_heads(&records, &heads)?;
        self.records = records;
        self.heads = heads;
        Ok(())
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
    /// A completed, code-authorized pairing may move a known device into our
    /// account. Keep its full Ed/X identity pinned, and reject the retired account
    /// immediately. The certificate registry is durable before this is called.
    pub(in crate::node) fn authorize_linked_device(
        &mut self,
        device: super::account_devices::AccountDevice,
        own_account: &str,
    ) -> io::Result<()> {
        if !device.certificate.verify()
            || device.certificate.device_ed25519_pub != device.public.ed25519_pub
            || device.certificate.account_id() != own_account
            || self
                .by_author(&device.public.ed25519_pub)
                .is_some_and(|old| old.public() != device.public)
        {
            return Err(invalid());
        }
        if let Some(existing) = self
            .linked_bindings
            .iter()
            .find(|d| d.public.user_id() == device.public.user_id())
        {
            return if existing == &device {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        self.linked_bindings.push(device);
        Ok(())
    }

    fn linked_binding(&self, user_id: &str) -> Option<&super::account_devices::AccountDevice> {
        self.linked_bindings
            .iter()
            .find(|d| d.public.user_id() == user_id)
    }

    /// Shared by passive persistence and authenticated inbound/outbound admission.
    pub(in crate::node) fn accepts_binding(&self, announce: &Announce) -> bool {
        if let Some(linked) = self.linked_binding(&announce.user_id) {
            return announce.public() == linked.public
                && announce.account_cert.as_ref() == Some(&linked.certificate);
        }
        self.by_author(&announce.ed25519_pub).is_none_or(|old| {
            old.public() == announce.public() && old.account_id() == announce.account_id()
        })
    }
    /// A signed device rehome awaiting explicit local acceptance. Read-only;
    /// certified siblings already bound by pairing cannot be rehomed here.
    pub(in crate::node) fn account_binding_change(
        &self,
        announce: &Announce,
    ) -> Option<super::privacy_runtime::AccountBindingChange> {
        if !announce.valid_presence()
            || announce.post_office
            || self.linked_binding(&announce.user_id).is_some()
        {
            return None;
        }
        let old = self.by_author(&announce.ed25519_pub)?;
        if old.public() != announce.public() || old.post_office {
            return None;
        }
        let previous_account_id = old.account_id()?;
        let current_account_id = announce.account_id()?;
        (previous_account_id != current_account_id).then(|| {
            super::privacy_runtime::AccountBindingChange {
                device_id: announce.user_id.clone(),
                previous_account_id,
                current_account_id,
            }
        })
    }

    /// Only the exact prior durable binding of an already enrolled device can
    /// enter the inbound pairing-retry handler. Normal admission stays denied.
    pub(in crate::node) fn retired_link_binding(&self, announce: &Announce) -> bool {
        self.linked_binding(&announce.user_id)
            .is_some_and(|linked| {
                linked.public == announce.public() && !self.accepts_binding(announce)
            })
            && self.by_author(&announce.ed25519_pub).is_some_and(|old| {
                old.public() == announce.public() && old.account_id() == announce.account_id()
            })
    }

    fn record_bound(&mut self, announce: &Announce, accept_account_change: bool) -> io::Result<()> {
        if !valid(announce)
            || (self.linked_binding(&announce.user_id).is_some() && !self.accepts_binding(announce))
        {
            return Err(invalid());
        }
        let mut next = self.records.clone();
        match next.binary_search_by(|a| a.user_id.cmp(&announce.user_id)) {
            Ok(i) => {
                let existing = &next[i];
                if existing.ed25519_pub != announce.ed25519_pub
                    || existing.x25519_pub != announce.x25519_pub
                    || (!accept_account_change
                        && self.linked_binding(&announce.user_id).is_none()
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
        // Storage validation failures (for example a replaced cache path) must
        // remain distinguishable from a rejected device/account binding.
        let mut heads = self.heads.clone();
        if accept_account_change
            && self
                .by_author(&announce.ed25519_pub)
                .is_some_and(|old| old.account_cert != announce.account_cert)
        {
            heads.retain(|h| h.public != announce.public());
        }
        self.save_with_heads(&next, &heads)
            .map_err(io::Error::other)?;
        self.records = next;
        self.heads = heads;
        Ok(())
    }
    pub(crate) fn account_for(&self, public: &PublicIdentity) -> Option<String> {
        if let Some(linked) = self.linked_binding(&public.user_id()) {
            return (linked.public == *public).then(|| linked.certificate.account_id());
        }
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
        self.save_with_heads(records, &self.heads)
    }
    fn save_with_heads(&self, records: &[Announce], heads: &[TransitionHead]) -> io::Result<()> {
        let plaintext = if heads.is_empty() {
            codec().serialize(&(1u32, records))
        } else {
            codec().serialize(&(2u32, records, heads))
        }
        .map_err(invalid_error)?;
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
    fn transition_requires_existing_full_identity_pin_and_atomic_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proofs");
        let a = Account::generate();
        let b = Account::generate();
        let device = DeviceIdentity::generate();
        let before = Announce::new_with_account(&device, &a, "A", 1234);
        let after = Announce::new_with_account(&device, &b, "B", 1234);
        let transition = AccountBindingTransition::create(&a, &b, &device, None);
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        assert!(
            directory
                .transition_candidate(&after, std::slice::from_ref(&transition))
                .is_none()
        );
        directory.record(&before).unwrap();
        let changed = DeviceIdentity::from_secret_bytes(
            device.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        let changed_after = Announce::new_with_account(&changed, &b, "B", 1234);
        let changed_transition = AccountBindingTransition::create(&a, &b, &changed, None);
        assert!(
            directory
                .record_transition(&changed_after, &[changed_transition])
                .is_err()
        );
        let saved = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(
            directory
                .record_transition(&after, std::slice::from_ref(&transition))
                .is_err()
        );
        assert!(directory.accepts_binding(&before));
        assert!(directory.transition_patches().is_empty());
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, saved).unwrap();
        directory.record_transition(&after, &[transition]).unwrap();
        let c = Account::generate();
        let explicit = Announce::new_with_account(&device, &c, "C", 1234);
        directory.record_explicit_account(&explicit).unwrap();
        assert!(directory.transition_patches().is_empty());
        drop(directory);
        assert!(
            PeerDirectory::open(&path, "pw")
                .unwrap()
                .transition_patches()
                .is_empty()
        );
    }
    #[test]
    fn authenticated_transition_pins_chain_and_relays_it_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proofs");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let device = DeviceIdentity::generate();
        let aa = Announce::new_with_account(&device, &a, "A", 1234);
        let bb = Announce::new_with_account(&device, &b, "B", 1234);
        let cc = Announce::new_with_account(&device, &c, "C", 1234);
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        let bc = AccountBindingTransition::create(&b, &c, &device, Some(ab.id()));
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        directory.record(&aa).unwrap();
        assert_eq!(
            directory.transition_candidate(&cc, &[ab.clone(), bc.clone()]),
            Some(a.account_id())
        );
        let mut forged = ab.clone();
        forged.old_signature[0] ^= 1;
        assert!(directory.record_transition(&bb, &[forged]).is_err());
        directory
            .record_transition(&bb, std::slice::from_ref(&ab))
            .unwrap();
        drop(directory);
        let mut directory = PeerDirectory::open(&path, "pw").unwrap();
        let fork = AccountBindingTransition::create(&a, &b, &device, None);
        let fork_bc = AccountBindingTransition::create(&b, &c, &device, Some(fork.id()));
        assert!(directory.record_transition(&cc, &[fork, fork_bc]).is_err());
        assert_eq!(
            directory.transition_candidate(&cc, &[ab.clone(), bc.clone()]),
            Some(b.account_id())
        );
        directory
            .record_transition(&cc, std::slice::from_ref(&bc))
            .unwrap();
        assert!(
            directory
                .record_transition(&bb, std::slice::from_ref(&ab))
                .is_err()
        );
        assert!(directory.record(&aa).is_err());
        let before = std::fs::read(&path).unwrap();
        directory
            .record_transition(&cc, &[ab.clone(), bc.clone()])
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        drop(directory);
        let directory = PeerDirectory::open(&path, "pw").unwrap();
        let patches = directory.transition_patches();
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].announce, cc);
        assert_eq!(patches[0].transitions, vec![ab, bc]);
    }
    #[test]
    fn completed_pairing_permits_only_exact_certified_own_account_transition() {
        use crate::node::account_devices::AccountDevice;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proofs");
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let own = Account::generate();
        let outsider = Account::generate();
        let prior = Announce::new_with_account(&device, &old, "Before", 1234);
        let adopted = Announce::new_with_account(&device, &own, "After", 1235);
        let unrelated = Announce::new_with_account(&device, &outsider, "Other", 1236);
        let mut proofs = PeerDirectory::open(&path, "pw").unwrap();
        proofs.record(&prior).unwrap();
        assert!(!proofs.accepts_binding(&adopted));
        assert!(!proofs.retired_link_binding(&prior));
        assert!(proofs.record(&adopted).is_err());
        let enrollment = AccountDevice {
            public: device.public(),
            certificate: own.certify(&device.public().ed25519_pub),
        };
        let mut forged = enrollment.clone();
        forged.certificate.signature[0] ^= 1;
        assert!(
            proofs
                .authorize_linked_device(forged, &own.account_id())
                .is_err()
        );
        assert!(
            proofs
                .authorize_linked_device(enrollment.clone(), &old.account_id())
                .is_err()
        );
        proofs
            .authorize_linked_device(enrollment.clone(), &own.account_id())
            .unwrap();
        assert_eq!(proofs.account_for(&device.public()), Some(own.account_id()));
        assert!(!proofs.accepts_binding(&prior));
        assert!(proofs.retired_link_binding(&prior));
        assert!(!proofs.retired_link_binding(&unrelated));
        assert!(proofs.record(&prior).is_err());
        assert!(proofs.record_explicit_account(&unrelated).is_err());
        let (ed, _) = device.secret_bytes();
        let replaced_x =
            DeviceIdentity::from_secret_bytes(ed, DeviceIdentity::generate().secret_bytes().1);
        let altered = Announce::new_with_account(&replaced_x, &own, "Wrong X", 1234);
        assert!(!proofs.accepts_binding(&altered));
        assert!(proofs.record(&altered).is_err());
        // Simulate a crash after durable enrollment but before the first new announce.
        drop(proofs);
        let mut proofs = PeerDirectory::open(&path, "pw").unwrap();
        proofs
            .authorize_linked_device(enrollment, &own.account_id())
            .unwrap();
        assert!(proofs.accepts_binding(&adopted));
        proofs.record(&adopted).unwrap();
        assert!(
            !proofs.retired_link_binding(&prior),
            "new own-account presence permanently retires the retry exception"
        );
        drop(proofs);
        let mut proofs = PeerDirectory::open(&path, "pw").unwrap();
        assert!(proofs.record(&prior).is_err());
        assert_eq!(proofs.account_for(&device.public()), Some(own.account_id()));
    }

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
        assert!(
            proofs
                .record_explicit_account(&Announce::new_with_account(
                    &different_x,
                    &new,
                    "Device",
                    1234
                ))
                .is_err()
        );
        proofs.record_explicit_account(&updated).unwrap();
        assert_eq!(proofs.account_for(&device.public()), Some(new.account_id()));
        assert!(
            proofs.record(&first).is_err(),
            "passive old binding must stay rejected after explicit rehome"
        );
    }

    fn write_snapshot(path: &Path, records: &[Announce], trailing: bool) {
        use crate::storage::encryption::{EncryptionKey, encrypt_data, generate_salt};
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
        assert!(
            !bytes
                .windows(announce.name.len())
                .any(|s| s == announce.name.as_bytes())
        );
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
        use std::os::unix::fs::{PermissionsExt, symlink};
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
        assert!(
            directory
                .record(&Announce::new_with_account(
                    &DeviceIdentity::generate(),
                    &Account::generate(),
                    "Alice",
                    1234
                ))
                .is_err()
        );
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

#[cfg(test)]
mod account_change_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    #[test]
    fn account_change_diagnostics_require_verified_unchanged_full_device_identity() {
        let directory = tempfile::tempdir().unwrap();
        let mut proofs = PeerDirectory::open(&directory.path().join("proofs"), "pw").unwrap();
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let new = Account::generate();
        proofs
            .record(&Announce::new_with_account(&device, &old, "Before", 1))
            .unwrap();
        let claim = Announce::new_with_account(&device, &new, "After", 1);
        assert!(proofs.account_binding_change(&claim).is_some());
        assert!(
            !proofs.accepts_binding(&claim),
            "inspection must not authorize passive rehoming"
        );
        let mut forged = claim.clone();
        forged.sig[0] ^= 1;
        assert!(proofs.account_binding_change(&forged).is_none());
        let alternate_x = DeviceIdentity::generate().secret_bytes().1;
        let changed = DeviceIdentity::from_secret_bytes(device.secret_bytes().0, alternate_x);
        let changed = Announce::new_with_account(&changed, &new, "Changed X key", 1);
        assert!(changed.verify());
        assert!(proofs.account_binding_change(&changed).is_none());
        proofs
            .authorize_linked_device(
                super::super::account_devices::AccountDevice {
                    public: device.public(),
                    certificate: old.certify(&device.public().ed25519_pub),
                },
                &old.account_id(),
            )
            .unwrap();
        assert!(
            proofs.account_binding_change(&claim).is_none(),
            "paired own-account certificates are not overridden by recovery"
        );
    }
}

//! Account-signed, self-declared profile claims and independent signed receipts.
use super::profile::MAX_AVATAR_BYTES;
use crate::eventlog::LogError;
use crate::identity::{
    account::{Account, AccountPublic},
    device::DeviceIdentity,
};
use crate::storage::record_log::EncryptedRecordLog;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

const MAX_PROOFS: usize = 16;
const MAX_TEXT: usize = 2048;
const MAX_WIRE: usize = MAX_AVATAR_BYTES + 96 * 1024;
mod indexed;
pub(super) use indexed::IndexedTrustCardStore;

const MAGIC: &[u8; 6] = b"MTTRST";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Introduction {
    pub issuer_ed25519_pub: [u8; 32],
    pub subject_account_id: String,
    pub issued_at_ms: u64,
    pub note: String,
    #[serde(with = "super::profile::sig_bytes")]
    pub signature: [u8; 64],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimestampWitness {
    pub issuer_ed25519_pub: [u8; 32],
    pub subject_account_id: String,
    pub created_at_ms: u64,
    pub observed_at_ms: u64,
    #[serde(with = "super::profile::sig_bytes")]
    pub signature: [u8; 64],
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustCard {
    pub account_ed25519_pub: [u8; 32],
    pub revision: u64,
    pub display_name: String,
    pub bio: String,
    pub created_at_ms: Option<u64>,
    pub avatar: Option<Vec<u8>>,
    pub introductions: Vec<Introduction>,
    pub witnesses: Vec<TimestampWitness>,
    #[serde(with = "super::profile::sig_bytes")]
    pub signature: [u8; 64],
}

fn signed_input(domain: &[u8], value: &impl Serialize) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    bytes.extend(bincode::serialize(value).expect("fixed trust fields serialize"));
    bytes
}
fn account_id(key: [u8; 32]) -> String {
    AccountPublic { ed25519_pub: key }.account_id()
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
}
impl Introduction {
    pub fn new(
        account: &Account,
        subject_account_id: String,
        note: String,
        issued_at_ms: u64,
    ) -> Self {
        let mut proof = Self {
            issuer_ed25519_pub: account.public().ed25519_pub,
            subject_account_id,
            issued_at_ms,
            note,
            signature: [0; 64],
        };
        proof.signature = account.sign(&proof.input());
        proof
    }
    fn input(&self) -> Vec<u8> {
        signed_input(
            b"mesh-talk-introduction-v1",
            &(
                self.issuer_ed25519_pub,
                &self.subject_account_id,
                self.issued_at_ms,
                &self.note,
            ),
        )
    }
    pub fn verify(&self) -> bool {
        valid_id(&self.subject_account_id)
            && self.note.len() <= MAX_TEXT
            && account_id(self.issuer_ed25519_pub) != self.subject_account_id
            && DeviceIdentity::verify(&self.issuer_ed25519_pub, &self.input(), &self.signature)
    }
}
impl TimestampWitness {
    pub fn new(
        account: &Account,
        subject_account_id: String,
        created_at_ms: u64,
        observed_at_ms: u64,
    ) -> Self {
        let mut proof = Self {
            issuer_ed25519_pub: account.public().ed25519_pub,
            subject_account_id,
            created_at_ms,
            observed_at_ms,
            signature: [0; 64],
        };
        proof.signature = account.sign(&proof.input());
        proof
    }
    fn input(&self) -> Vec<u8> {
        signed_input(
            b"mesh-talk-creation-witness-v1",
            &(
                self.issuer_ed25519_pub,
                &self.subject_account_id,
                self.created_at_ms,
                self.observed_at_ms,
            ),
        )
    }
    /// This authenticates an independent account's statement, never trusted wall time.
    pub fn verify(&self) -> bool {
        valid_id(&self.subject_account_id)
            && self.observed_at_ms >= self.created_at_ms
            && account_id(self.issuer_ed25519_pub) != self.subject_account_id
            && DeviceIdentity::verify(&self.issuer_ed25519_pub, &self.input(), &self.signature)
    }
}
impl TrustCard {
    pub fn new(
        account: &Account,
        revision: u64,
        display_name: String,
        bio: String,
        created_at_ms: Option<u64>,
        avatar: Option<Vec<u8>>,
    ) -> Self {
        let mut card = Self {
            account_ed25519_pub: account.public().ed25519_pub,
            revision,
            display_name,
            bio,
            created_at_ms,
            avatar,
            introductions: vec![],
            witnesses: vec![],
            signature: [0; 64],
        };
        card.resign(account);
        card
    }
    fn input(&self) -> Vec<u8> {
        signed_input(
            b"mesh-talk-trust-card-v1",
            &(
                self.account_ed25519_pub,
                self.revision,
                &self.display_name,
                &self.bio,
                self.created_at_ms,
                &self.avatar,
                &self.introductions,
                &self.witnesses,
            ),
        )
    }
    pub(super) fn resign(&mut self, account: &Account) {
        self.signature = account.sign(&self.input());
    }
    pub fn account_id(&self) -> String {
        account_id(self.account_ed25519_pub)
    }
    pub fn verify(&self) -> bool {
        if self.revision == 0
            || self.display_name.len() > 256
            || self.bio.len() > MAX_TEXT
            || self
                .avatar
                .as_ref()
                .is_some_and(|a| a.len() > MAX_AVATAR_BYTES)
            || self.introductions.len() > MAX_PROOFS
            || self.witnesses.len() > MAX_PROOFS
        {
            return false;
        }
        let id = self.account_id();
        let mut issuers = std::collections::HashSet::new();
        if !self.introductions.iter().all(|p| {
            p.subject_account_id == id && p.verify() && issuers.insert(p.issuer_ed25519_pub)
        }) {
            return false;
        }
        issuers.clear();
        if !self.witnesses.iter().all(|p| {
            p.subject_account_id == id
                && Some(p.created_at_ms) == self.created_at_ms
                && p.verify()
                && issuers.insert(p.issuer_ed25519_pub)
        }) {
            return false;
        }
        DeviceIdentity::verify(&self.account_ed25519_pub, &self.input(), &self.signature)
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut out = b"MTC1".to_vec();
        out.extend(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("trust card serializes"),
        );
        out
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_WIRE {
            return None;
        }
        let card: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_WIRE as u64)
            .reject_trailing_bytes()
            .deserialize(bytes.strip_prefix(b"MTC1")?)
            .ok()?;
        card.verify().then_some(card)
    }
}
/// Latest signed card per account; invalid/stale records never become current.
pub(super) struct TrustCardStore {
    file: EncryptedRecordLog<TrustCard>,
    cards: HashMap<String, TrustCard>,
    records: usize,
}
impl TrustCardStore {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, cards) = EncryptedRecordLog::<TrustCard>::open(path, password, MAGIC)?;
        let mut store = Self {
            file,
            cards: HashMap::new(),
            records: cards.len(),
        };
        for card in cards {
            if store.accepts(&card) {
                store.cards.insert(card.account_id(), card);
            }
        }
        if store.records > store.cards.len() {
            store.compact()?;
        }
        Ok(store)
    }
    pub fn get(&self, id: &str) -> Option<TrustCard> {
        self.cards.get(id).cloned()
    }
    pub fn all(&self) -> Vec<TrustCard> {
        self.cards.values().cloned().collect()
    }
    fn accepts(&self, card: &TrustCard) -> bool {
        if !card.verify() {
            return false;
        }
        let id = card.account_id();
        if let Some(old) = self.cards.get(&id) {
            if old.revision >= card.revision
                || (old.created_at_ms.is_some() && old.created_at_ms != card.created_at_ms)
            {
                return false;
            }
        } else if self.cards.len() >= 1024 {
            return false;
        }
        let size: usize = self
            .cards
            .iter()
            .filter(|(key, _)| **key != id)
            .map(|(_, c)| c.encode().len())
            .sum();
        size.saturating_add(card.encode().len()) <= 32 * 1024 * 1024
    }
    pub fn record(&mut self, card: TrustCard) -> Result<bool, LogError> {
        if !self.accepts(&card) {
            return Ok(false);
        }
        if self.records >= self.cards.len().saturating_mul(2).saturating_add(32) {
            self.compact()?;
        }
        self.file.append(&card)?;
        self.records += 1;
        self.cards.insert(card.account_id(), card);
        Ok(true)
    }
    fn compact(&mut self) -> Result<(), LogError> {
        let mut cards = self.all();
        cards.sort_by_key(TrustCard::account_id);
        self.file.rewrite(&cards)?;
        self.records = cards.len();
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    pub(super) fn profile(
        root: &std::path::Path,
        identity: &crate::identity::device::DeviceIdentity,
        account: &Account,
    ) -> std::sync::Arc<crate::storage::profile::context::ProfileStorage> {
        let profile = crate::storage::profile::context::ProfileStorage::open_participant(
            crate::storage::profile::ProfileStorageLease::acquire(root).unwrap(),
            &identity.public(),
            &account.public(),
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        profile
    }
    #[test]
    fn warm_open_reads_no_avatar_payloads() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("cards");
        let mut store = TrustCardStore::open(&path, "pw").unwrap();
        for seed in 1..=32 {
            let account = crate::identity::account::Account::from_secret_bytes([seed; 32]);
            let card = TrustCard::new(
                &account,
                1,
                "Person".into(),
                String::new(),
                None,
                Some(vec![7; 8192]),
            );
            assert!(store.record(card).unwrap());
        }
        drop(store);
        let identity = crate::identity::device::DeviceIdentity::generate();
        let account = Account::generate();
        let owner = profile(root.path(), &identity, &account);
        let imported = IndexedTrustCardStore::from_profile(owner.clone(), &path, "pw").unwrap();
        drop(imported);
        drop(owner);
        indexed::reset_body_reads();
        crate::storage::record_log::frame_observations::reset();
        let owner = profile(root.path(), &identity, &account);
        let store = IndexedTrustCardStore::from_profile(owner, &path, "pw").unwrap();
        assert_eq!(indexed::body_reads(), 0);
        let page = store.page(None).unwrap();
        assert_eq!(page.rows.len(), 16);
        assert_eq!(indexed::body_reads(), 16);
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0,
            "warm trust-card open must not decrypt every account payload"
        );
    }

    use super::*;
    use crate::identity::account::Account;
    #[test]
    fn trust_card_signature_bounds_and_stable_witness() {
        let account = Account::generate();
        let issuer = Account::generate();
        let mut card = TrustCard::new(&account, 1, "Alice".into(), "Hello".into(), Some(42), None);
        assert!(card.verify());
        assert_eq!(TrustCard::decode(&card.encode()), Some(card.clone()));
        let intro = Introduction::new(&issuer, card.account_id(), "Met in person".into(), 100);
        let witness = TimestampWitness::new(&issuer, card.account_id(), 42, 101);
        card.introductions.push(intro);
        card.witnesses.push(witness);
        card.resign(&account);
        assert!(card.verify());
        card.created_at_ms = Some(43);
        card.resign(&account);
        assert!(!card.verify());
        card.witnesses.clear();
        card.bio = "x".repeat(2049);
        card.resign(&account);
        assert!(!card.verify());
    }
    #[test]
    fn trust_card_store_rejects_stale_tampering_and_creation_changes_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.log");
        let account = Account::generate();
        let card = TrustCard::new(&account, 1, "Alice".into(), "".into(), Some(42), None);
        let mut store = TrustCardStore::open(&path, "pw").unwrap();
        assert!(store.record(card.clone()).unwrap());
        assert!(!store.record(card.clone()).unwrap());
        let mut changed = card.clone();
        changed.revision = 2;
        changed.created_at_ms = Some(43);
        changed.resign(&account);
        assert!(!store.record(changed).unwrap());
        let mut bad = card.clone();
        bad.bio = "forged".into();
        assert!(!store.record(bad).unwrap());
        for revision in 2..150 {
            let mut next = card.clone();
            next.revision = revision;
            next.resign(&account);
            assert!(store.record(next).unwrap());
        }
        drop(store);
        let reopened = TrustCardStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.get(&account.account_id()).unwrap().revision, 149);
        let (_, records) = crate::storage::record_log::EncryptedRecordLog::<TrustCard>::open(
            &path, "pw", b"MTTRST",
        )
        .unwrap();
        assert_eq!(records.len(), 1);
    }
    #[test]
    fn trust_card_load_verifies_signed_records_and_decode_rejects_trailing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trust.log");
        let account = Account::generate();
        let mut bad = TrustCard::new(&account, 99, "Alice".into(), "bio".into(), None, None);
        let mut bytes = bad.encode();
        bytes.push(0);
        assert!(TrustCard::decode(&bytes).is_none());
        bad.bio = "forged".into();
        let (mut file, _) = EncryptedRecordLog::<TrustCard>::open(&path, "pw", MAGIC).unwrap();
        file.append(&bad).unwrap();
        drop(file);
        assert!(
            TrustCardStore::open(&path, "pw")
                .unwrap()
                .get(&account.account_id())
                .is_none()
        );
    }

    #[test]
    fn trust_card_rejects_wrong_subject_self_witness_and_unbounded_proofs() {
        let account = Account::generate();
        let issuer = Account::generate();
        let mut card = TrustCard::new(&account, 1, "Alice".into(), "".into(), Some(42), None);
        card.witnesses.push(TimestampWitness::new(
            &account,
            account.account_id(),
            42,
            50,
        ));
        card.resign(&account);
        assert!(!card.verify());
        card.witnesses.clear();
        card.introductions.push(Introduction::new(
            &issuer,
            issuer.account_id(),
            "wrong subject".into(),
            50,
        ));
        card.resign(&account);
        assert!(!card.verify());
        card.introductions.clear();
        for _ in 0..17 {
            let other = Account::generate();
            card.introductions.push(Introduction::new(
                &other,
                account.account_id(),
                String::new(),
                50,
            ));
        }
        card.resign(&account);
        assert!(!card.verify());
    }
}

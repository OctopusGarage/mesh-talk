//! Ordinary account profiles; signing and replication are implementation details.

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

const MAGIC: &[u8; 6] = b"MTAPRF";
const WIRE_MAGIC: &[u8; 4] = b"MTP3";
const MAX_WIRE: usize = MAX_AVATAR_BYTES + 4096;
const MAX_ACCOUNTS: usize = 1024;
const MAX_STORE_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_BIO_BYTES: usize = 2048;

/// Authenticated presentation data. Creation time comes from account metadata,
/// never from a profile-edit intent. It still uses the originating device's clock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountProfile {
    pub account_ed25519_pub: [u8; 32],
    pub revision: u64,
    pub display_name: String,
    pub bio: String,
    pub created_at_ms: Option<u64>,
    pub avatar: Option<Vec<u8>>,
    #[serde(with = "super::profile::sig_bytes")]
    pub signature: [u8; 64],
}

impl AccountProfile {
    pub(super) fn new(
        account: &Account,
        revision: u64,
        display_name: String,
        bio: String,
        avatar: Option<Vec<u8>>,
    ) -> Self {
        let mut profile = Self {
            account_ed25519_pub: account.public().ed25519_pub,
            revision,
            display_name,
            bio,
            created_at_ms: account.created_at_ms(),
            avatar,
            signature: [0; 64],
        };
        profile.resign(account);
        profile
    }
    fn input(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-account-profile-v1".to_vec();
        bytes.extend(
            bincode::serialize(&(
                self.account_ed25519_pub,
                self.revision,
                &self.display_name,
                &self.bio,
                self.created_at_ms,
                &self.avatar,
            ))
            .expect("profile fields serialize"),
        );
        bytes
    }
    pub(super) fn resign(&mut self, account: &Account) {
        self.signature = account.sign(&self.input());
    }
    pub fn account_id(&self) -> String {
        AccountPublic {
            ed25519_pub: self.account_ed25519_pub,
        }
        .account_id()
    }
    pub fn verify(&self) -> bool {
        self.revision > 0
            && self.display_name.len() <= 256
            && self.bio.len() <= MAX_BIO_BYTES
            && self
                .avatar
                .as_ref()
                .is_none_or(|value| value.len() <= MAX_AVATAR_BYTES)
            && self
                .created_at_ms
                .is_none_or(|time| (1..=253_402_300_799_999).contains(&time))
            && DeviceIdentity::verify(&self.account_ed25519_pub, &self.input(), &self.signature)
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = WIRE_MAGIC.to_vec();
        bytes.extend(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("profile serializes"),
        );
        bytes
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_WIRE {
            return None;
        }
        let value: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_WIRE as u64)
            .reject_trailing_bytes()
            .deserialize(bytes.strip_prefix(WIRE_MAGIC)?)
            .ok()?;
        value.verify().then_some(value)
    }
}

/// One bounded current profile per account. Equal revisions converge by signature
/// ordering; an established creation date cannot be replaced by another device.
pub(super) struct AccountProfileStore {
    file: EncryptedRecordLog<AccountProfile>,
    profiles: HashMap<String, AccountProfile>,
    records: usize,
}
impl AccountProfileStore {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let mut store = Self {
            file,
            profiles: HashMap::new(),
            records: records.len(),
        };
        for record in records {
            if store.accepts(&record) {
                store.profiles.insert(record.account_id(), record);
            }
        }
        if store.records > store.profiles.len() {
            store.compact()?;
        }
        Ok(store)
    }
    pub fn get(&self, id: &str) -> Option<AccountProfile> {
        self.profiles.get(id).cloned()
    }
    pub fn all(&self) -> Vec<AccountProfile> {
        self.profiles.values().cloned().collect()
    }
    fn order(profile: &AccountProfile) -> (u64, bool, Option<&[u8]>, &[u8; 64]) {
        // Keep avatar tie-breaking identical to the legacy avatar projection;
        // signature ordering resolves metadata-only ties after that shared order.
        (
            profile.revision,
            profile.avatar.is_none(),
            profile.avatar.as_deref(),
            &profile.signature,
        )
    }
    pub(super) fn accepts(&self, profile: &AccountProfile) -> bool {
        if !profile.verify() {
            return false;
        }
        let id = profile.account_id();
        if let Some(old) = self.profiles.get(&id) {
            if old.created_at_ms.is_some() && old.created_at_ms != profile.created_at_ms {
                return false;
            }
            if Self::order(profile) <= Self::order(old) {
                return false;
            }
        } else if self.profiles.len() >= MAX_ACCOUNTS {
            return false;
        }
        self.profiles
            .iter()
            .filter(|(key, _)| **key != id)
            .map(|(_, value)| value.encode().len())
            .sum::<usize>()
            .saturating_add(profile.encode().len())
            <= MAX_STORE_BYTES
    }
    pub fn record(&mut self, profile: AccountProfile) -> Result<bool, LogError> {
        if !self.accepts(&profile) {
            return Ok(false);
        }
        if self.records >= self.profiles.len().saturating_mul(2).saturating_add(32) {
            self.compact()?;
        }
        self.file.append(&profile)?;
        self.records += 1;
        self.profiles.insert(profile.account_id(), profile);
        Ok(true)
    }
    fn compact(&mut self) -> Result<(), LogError> {
        let mut values = self.all();
        values.sort_by_key(AccountProfile::account_id);
        self.file.rewrite(&values)?;
        self.records = values.len();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;

    #[test]
    fn profile_edits_preserve_system_creation_metadata_and_reject_tampering() {
        let account = Account::generate();
        let profile = AccountProfile::new(&account, 1, "Alice".into(), "About".into(), None);
        assert_eq!(profile.created_at_ms, account.created_at_ms());
        assert!(profile.verify());
        assert_eq!(
            AccountProfile::decode(&profile.encode()),
            Some(profile.clone())
        );
        let mut tampered = profile.clone();
        tampered.created_at_ms = Some(42);
        assert!(!tampered.verify());
        let mut trailing = profile.encode();
        trailing.push(0);
        assert!(AccountProfile::decode(&trailing).is_none());
    }

    #[test]
    fn legacy_keys_have_no_invented_creation_date() {
        let account = Account::from_secret_bytes([7; 32]);
        let profile = AccountProfile::new(&account, 1, "Legacy".into(), String::new(), None);
        assert_eq!(profile.created_at_ms, None);
        assert!(profile.verify());
    }

    #[test]
    fn equal_revision_avatar_clears_win_in_both_profile_and_compatibility_projection() {
        // Choose fixed signed inputs whose signature order disagrees with the
        // established avatar clear order. This catches comparing signatures first.
        let (photo, clear) = (1..=32)
            .find_map(|seed| {
                let account = Account::from_secret_bytes([seed; 32]);
                let photo = AccountProfile::new(
                    &account,
                    1,
                    "Alice".into(),
                    String::new(),
                    Some(b"photo".to_vec()),
                );
                let clear = AccountProfile::new(&account, 1, "Alice".into(), String::new(), None);
                (photo.signature > clear.signature).then_some((photo, clear))
            })
            .expect("fixture has opposing signature and clear orders");
        let dir = tempfile::tempdir().unwrap();
        let mut store = AccountProfileStore::open(&dir.path().join("profiles"), "pw").unwrap();
        store.record(clear.clone()).unwrap();
        store.record(photo).unwrap();
        assert_eq!(store.get(&clear.account_id()), Some(clear));
    }

    #[test]
    fn profile_store_rejects_conflicting_dates_and_converges_equal_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let first = AccountProfile::new(&account, 1, "Alice".into(), "a".into(), None);
        let second = AccountProfile::new(&account, 1, "Alice".into(), "b".into(), None);
        let mut left = AccountProfileStore::open(&dir.path().join("left"), "pw").unwrap();
        let mut right = AccountProfileStore::open(&dir.path().join("right"), "pw").unwrap();
        left.record(first.clone()).unwrap();
        left.record(second.clone()).unwrap();
        right.record(second).unwrap();
        right.record(first).unwrap();
        assert_eq!(
            left.get(&account.account_id()),
            right.get(&account.account_id())
        );
        let saved = left.get(&account.account_id()).unwrap();
        let mut conflicting = saved.clone();
        conflicting.revision += 1;
        conflicting.created_at_ms = Some(42);
        conflicting.resign(&account);
        assert!(!left.record(conflicting).unwrap());
        drop(left);
        let reopened = AccountProfileStore::open(&dir.path().join("left"), "pw").unwrap();
        assert_eq!(reopened.get(&account.account_id()), Some(saved));
    }
}

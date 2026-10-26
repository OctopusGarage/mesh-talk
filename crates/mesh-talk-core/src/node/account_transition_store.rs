//! Durable preparation and publication of this profile's exact-device transitions.
use crate::{
    eventlog::LogError,
    identity::{account_transition::AccountBindingTransition, device::PublicIdentity},
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
struct TransitionRecord {
    version: u8,
    transition: AccountBindingTransition,
    committed: bool,
}
pub(super) struct AccountTransitionStore {
    file: EncryptedRecordLog<TransitionRecord>,
    records: Vec<TransitionRecord>,
}
fn invalid() -> LogError {
    LogError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "invalid account transition journal",
    ))
}
fn valid_extension(records: &[TransitionRecord], transition: &AccountBindingTransition) -> bool {
    if records.len() >= 256 || !transition.verify() {
        return false;
    }
    let related: Vec<_> = records
        .iter()
        .filter(|r| r.transition.public.ed25519_pub == transition.public.ed25519_pub)
        .collect();
    if related.len() >= 8
        || related
            .iter()
            .any(|r| r.transition.public != transition.public)
    {
        return false;
    }
    match related.last() {
        None => transition.previous_transition.is_none(),
        Some(tail) => {
            let unseen = |account| {
                !related.iter().any(|r| {
                    r.transition.previous_certificate.account_ed25519_pub == account
                        || r.transition.current_certificate.account_ed25519_pub == account
                })
            };
            tail.committed
                && unseen(transition.current_certificate.account_ed25519_pub)
                && if transition.previous_transition.is_none() {
                    // A local security reset may start a separate segment. It conveys
                    // no authority to recipients pinned to an earlier segment.
                    unseen(transition.previous_certificate.account_ed25519_pub)
                } else {
                    transition.previous_transition == Some(tail.transition.id())
                        && transition.previous_certificate == tail.transition.current_certificate
                }
        }
    }
}
impl AccountTransitionStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() || meta.len() > 512 * 1024 => return Err(invalid()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LogError::Io(e)),
        }
        let (file, records): (_, Vec<TransitionRecord>) =
            EncryptedRecordLog::open(path, password, b"MTATR1")?;
        for (i, record) in records.iter().enumerate() {
            if record.version != 1 || !valid_extension(&records[..i], &record.transition) {
                return Err(invalid());
            }
        }
        Ok(Self { file, records })
    }
    pub(super) fn prepare(&mut self, transition: AccountBindingTransition) -> Result<(), LogError> {
        if self.records.iter().any(|r| r.transition == transition) {
            return Ok(());
        }
        if !valid_extension(&self.records, &transition) {
            return Err(invalid());
        }
        let record = TransitionRecord {
            version: 1,
            transition,
            committed: false,
        };
        self.file.append_durable(&record)?;
        self.records.push(record);
        Ok(())
    }
    pub(super) fn commit_own(
        &mut self,
        public: &PublicIdentity,
        current_account: &[u8; 32],
    ) -> Result<(), LogError> {
        let mut next = self.records.clone();
        let mut changed = false;
        for record in &mut next {
            if record.transition.public == *public
                && !record.committed
                && record.transition.current_certificate.account_ed25519_pub == *current_account
            {
                record.committed = true;
                changed = true;
            }
        }
        if changed {
            self.file.rewrite(&next)?;
            self.records = next;
        }
        Ok(())
    }
    pub(super) fn discard_pending(
        &mut self,
        public: &PublicIdentity,
        loaded_account: &[u8; 32],
    ) -> Result<(), LogError> {
        let next: Vec<_> = self
            .records
            .iter()
            .filter(|r| {
                r.committed
                    || r.transition.public != *public
                    || r.transition.previous_certificate.account_ed25519_pub != *loaded_account
            })
            .cloned()
            .collect();
        if next.len() != self.records.len() {
            self.file.rewrite(&next)?;
            self.records = next;
        }
        Ok(())
    }
    pub(super) fn own_chain(
        &self,
        public: &PublicIdentity,
        current_account: &[u8; 32],
    ) -> Vec<AccountBindingTransition> {
        let chain: Vec<_> = self
            .records
            .iter()
            .filter(|r| r.committed && r.transition.public == *public)
            .map(|r| r.transition.clone())
            .collect();
        let chain = match chain.iter().rposition(|t| t.previous_transition.is_none()) {
            Some(start) => chain[start..].to_vec(),
            None => chain,
        };
        if chain
            .last()
            .is_some_and(|t| t.current_certificate.account_ed25519_pub == *current_account)
        {
            chain
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    #[test]
    fn isolated_security_reset_segment_never_claims_old_account_continuity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let d = Account::generate();
        let device = DeviceIdentity::generate();
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        store.prepare(ab).unwrap();
        store
            .commit_own(&device.public(), &b.public().ed25519_pub)
            .unwrap();
        store
            .commit_own(&device.public(), &c.public().ed25519_pub)
            .unwrap();
        assert!(
            store
                .own_chain(&device.public(), &c.public().ed25519_pub)
                .is_empty()
        );
        let cd = AccountBindingTransition::create(&c, &d, &device, None);
        store.prepare(cd.clone()).unwrap();
        store
            .commit_own(&device.public(), &d.public().ed25519_pub)
            .unwrap();
        drop(store);
        let store = AccountTransitionStore::open(&path, "pw").unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &d.public().ed25519_pub),
            vec![cd]
        );
        assert!(
            store
                .own_chain(&device.public(), &b.public().ed25519_pub)
                .is_empty()
        );
    }
    #[test]
    fn loaded_old_root_discards_only_unpublished_tail_and_allows_new_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let device = DeviceIdentity::generate();
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        store.prepare(ab).unwrap();
        assert!(
            store
                .prepare(AccountBindingTransition::create(&a, &c, &device, None))
                .is_err()
        );
        store
            .discard_pending(&device.public(), &a.public().ed25519_pub)
            .unwrap();
        let ac = AccountBindingTransition::create(&a, &c, &device, None);
        store.prepare(ac.clone()).unwrap();
        store
            .commit_own(&device.public(), &c.public().ed25519_pub)
            .unwrap();
        store
            .discard_pending(&device.public(), &c.public().ed25519_pub)
            .unwrap();
        drop(store);
        let store = AccountTransitionStore::open(&path, "pw").unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &c.public().ed25519_pub),
            vec![ac]
        );
    }
    #[test]
    fn invalid_persisted_proofs_and_record_budget_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        let mut forged =
            AccountBindingTransition::create(&a, &b, &DeviceIdentity::generate(), None);
        forged.old_signature[0] ^= 1;
        assert!(store.prepare(forged.clone()).is_err());
        store
            .file
            .append_durable(&TransitionRecord {
                version: 1,
                transition: forged,
                committed: false,
            })
            .unwrap();
        drop(store);
        assert!(AccountTransitionStore::open(&path, "pw").is_err());
        let path = dir.path().join("bounded");
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        for _ in 0..256 {
            store
                .prepare(AccountBindingTransition::create(
                    &a,
                    &b,
                    &DeviceIdentity::generate(),
                    None,
                ))
                .unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        assert!(
            store
                .prepare(AccountBindingTransition::create(
                    &a,
                    &b,
                    &DeviceIdentity::generate(),
                    None
                ))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }
    #[test]
    fn pending_is_private_until_loaded_account_commit_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let device = DeviceIdentity::generate();
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        store.prepare(ab.clone()).unwrap();
        assert!(
            store
                .own_chain(&device.public(), &b.public().ed25519_pub)
                .is_empty()
        );
        store
            .commit_own(&device.public(), &a.public().ed25519_pub)
            .unwrap();
        assert!(
            store
                .own_chain(&device.public(), &a.public().ed25519_pub)
                .is_empty()
        );
        drop(store);
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        store
            .commit_own(&device.public(), &b.public().ed25519_pub)
            .unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &b.public().ed25519_pub),
            vec![ab.clone()]
        );
        let before = std::fs::read(&path).unwrap();
        store.prepare(ab.clone()).unwrap();
        store
            .commit_own(&device.public(), &b.public().ed25519_pub)
            .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "replay must not create another journal record"
        );
        let bc = AccountBindingTransition::create(&b, &c, &device, Some(ab.id()));
        store.prepare(bc.clone()).unwrap();
        drop(store);
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &b.public().ed25519_pub),
            vec![ab.clone()]
        );
        assert!(
            store
                .own_chain(&device.public(), &c.public().ed25519_pub)
                .is_empty()
        );
        store
            .commit_own(&device.public(), &c.public().ed25519_pub)
            .unwrap();
        drop(store);
        let store = AccountTransitionStore::open(&path, "pw").unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &c.public().ed25519_pub),
            vec![ab, bc]
        );
        assert!(
            store
                .own_chain(&device.public(), &b.public().ed25519_pub)
                .is_empty()
        );
        assert!(AccountTransitionStore::open(&path, "wrong").is_err());
    }
    #[test]
    fn forks_cycles_swapped_full_identity_and_chain_limit_fail_without_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let device = DeviceIdentity::generate();
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        store.prepare(ab.clone()).unwrap();
        store
            .commit_own(&device.public(), &b.public().ed25519_pub)
            .unwrap();
        let changed = DeviceIdentity::from_secret_bytes(
            device.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        let before = std::fs::read(&path).unwrap();
        for invalid in [
            AccountBindingTransition::create(&a, &c, &device, None),
            AccountBindingTransition::create(&b, &c, &device, None),
            AccountBindingTransition::create(&b, &c, &device, Some([7; 32])),
            AccountBindingTransition::create(&b, &a, &device, Some(ab.id())),
            AccountBindingTransition::create(&b, &c, &changed, Some(ab.id())),
        ] {
            assert!(store.prepare(invalid).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let mut tail = ab;
        let mut old = b;
        for _ in 1..8 {
            let next = Account::generate();
            let transition =
                AccountBindingTransition::create(&old, &next, &device, Some(tail.id()));
            store.prepare(transition.clone()).unwrap();
            store
                .commit_own(&device.public(), &next.public().ed25519_pub)
                .unwrap();
            tail = transition;
            old = next;
        }
        assert!(
            store
                .prepare(AccountBindingTransition::create(
                    &old,
                    &Account::generate(),
                    &device,
                    Some(tail.id())
                ))
                .is_err()
        );
        assert_eq!(
            store
                .own_chain(&device.public(), &old.public().ed25519_pub)
                .len(),
            8
        );
    }
    #[test]
    fn torn_pending_tail_recovers_confirmed_chain_and_allows_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transitions");
        let a = Account::generate();
        let b = Account::generate();
        let c = Account::generate();
        let device = DeviceIdentity::generate();
        let ab = AccountBindingTransition::create(&a, &b, &device, None);
        let bc = AccountBindingTransition::create(&b, &c, &device, Some(ab.id()));
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        store.prepare(ab.clone()).unwrap();
        store
            .commit_own(&device.public(), &b.public().ed25519_pub)
            .unwrap();
        store.prepare(bc.clone()).unwrap();
        drop(store);
        let size = std::fs::metadata(&path).unwrap().len();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(size - 3)
            .unwrap();
        let mut store = AccountTransitionStore::open(&path, "pw").unwrap();
        assert_eq!(
            store.own_chain(&device.public(), &b.public().ed25519_pub),
            vec![ab]
        );
        store.prepare(bc.clone()).unwrap();
        store
            .commit_own(&device.public(), &c.public().ed25519_pub)
            .unwrap();
        drop(store);
        assert_eq!(
            AccountTransitionStore::open(&path, "pw")
                .unwrap()
                .own_chain(&device.public(), &c.public().ed25519_pub)
                .last(),
            Some(&bc)
        );
    }
}

//! Owns avatar persistence, signed publication and coordinated account transitions.
//! Callers authenticate device/account bindings and authorize local edits; this
//! module owns revision allocation and serializes received profiles with edits.
use super::{
    NodeError,
    profile::{MAX_AVATAR_BYTES, ProfilePayload},
    profile_store::ProfileStore,
};
use crate::{eventlog::LogError, identity::account::Account};
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard},
};

pub(super) struct AccountProfiles {
    account: Account,
    state: Mutex<State>,
}
struct State {
    avatars: ProfileStore,
    current: Option<ProfilePayload>,
}
impl AccountProfiles {
    pub fn new(account: &Account, avatars: ProfileStore) -> Self {
        let current = Self::reconstruct(account, &avatars);
        Self {
            account: Account::from_secret_bytes(account.secret_bytes()),
            state: Mutex::new(State { avatars, current }),
        }
    }
    fn reconstruct(account: &Account, avatars: &ProfileStore) -> Option<ProfilePayload> {
        avatars.updated_at(&account.account_id()).map(|revision| {
            ProfilePayload::new(account, avatars.avatar(&account.account_id()), revision)
        })
    }
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("account profile mutex not poisoned")
    }
    #[cfg(test)]
    pub(super) fn fail_appends_for_test(&self, enabled: bool) {
        self.state().avatars.fail_appends_for_test(enabled);
    }
    pub fn current(&self) -> Option<ProfilePayload> {
        self.state().current.clone()
    }
    pub fn avatar(&self, account: &str) -> Option<Vec<u8>> {
        self.state().avatars.avatar(account)
    }
    pub fn avatars(&self) -> HashMap<String, Vec<u8>> {
        self.state().avatars.all()
    }
    pub fn avatar_states(&self) -> HashMap<String, Option<Vec<u8>>> {
        self.snapshot()
            .into_iter()
            .map(|(id, _, avatar)| (id, avatar))
            .collect()
    }
    /// An identical avatar is a no-op, including an existing clear tombstone.
    pub fn set_avatar(&self, avatar: Option<Vec<u8>>) -> Result<Option<ProfilePayload>, NodeError> {
        if let Some(bytes) = &avatar
            && bytes.len() > MAX_AVATAR_BYTES
        {
            return Err(NodeError::Channel(format!(
                "avatar too large: {} bytes (max {MAX_AVATAR_BYTES})",
                bytes.len()
            )));
        }
        let mut state = self.state();
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.avatar == avatar)
        {
            return Ok(None);
        }
        let account = self.account.account_id();
        let revision = self.next_avatar_revision(&state)?;
        let payload = ProfilePayload::new(&self.account, avatar.clone(), revision);
        let applied = state
            .avatars
            .record(&account, revision, avatar)
            .map_err(NodeError::Log)?;
        if !applied {
            return Err(NodeError::InvalidInput(
                "avatar update did not advance the durable profile revision".into(),
            ));
        }
        state.current = Some(payload.clone());
        Ok(Some(payload))
    }
    fn next_avatar_revision(&self, state: &State) -> Result<u64, NodeError> {
        let next = state
            .avatars
            .updated_at(&self.account.account_id())
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| NodeError::InvalidInput("account profile revision exhausted".into()))?;
        Ok(super::node::now_millis().max(next))
    }
    /// Preflight additional revisions needed by a coordinated signed-card edit.
    pub fn validate_avatar_revision_headroom(&self, extra_revisions: u64) -> Result<(), NodeError> {
        self.next_avatar_revision(&self.state())?
            .checked_add(extra_revisions)
            .ok_or_else(|| NodeError::InvalidInput("account profile revision exhausted".into()))?;
        Ok(())
    }
    /// Recheck signatures after caller authentication. Retired metadata is ignored
    /// when reconstructing our own publication, including older MTPF2 profiles.
    pub fn receive(&self, payload: &ProfilePayload) -> Result<bool, LogError> {
        if !payload.verify() {
            return Ok(false);
        }
        let mut state = self.state();
        let account = payload.account_id();
        let applied = state
            .avatars
            .record(&account, payload.updated_at, payload.avatar.clone())?;
        if account == self.account.account_id() {
            state.current = Self::reconstruct(&self.account, &state.avatars);
        }
        Ok(applied)
    }
    pub fn receive_trust_card(&self, card: &super::TrustCard) -> Result<bool, LogError> {
        if !card.verify() {
            return Ok(false);
        }
        let mut state = self.state();
        let account = card.account_id();
        let applied = state
            .avatars
            .record(&account, card.revision, card.avatar.clone())?;
        if account == self.account.account_id() {
            state.current = Self::reconstruct(&self.account, &state.avatars);
        }
        Ok(applied)
    }
    pub fn snapshot(&self) -> Vec<(String, u64, Option<Vec<u8>>)> {
        self.state().avatars.snapshot_entries()
    }
    /// Trusted snapshot installation preserves revisions and clear tombstones;
    /// reopening reconstructs publication after installation completes.
    pub fn restore_avatar(
        &self,
        account: &str,
        revision: u64,
        avatar: Option<Vec<u8>>,
    ) -> Result<bool, LogError> {
        self.state().avatars.record(account, revision, avatar)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn open(dir: &std::path::Path, account: &Account) -> AccountProfiles {
        AccountProfiles::new(
            account,
            ProfileStore::open(&dir.join("profiles.log"), "pw").unwrap(),
        )
    }
    #[test]
    fn local_edits_and_clear_survive_restart_without_retired_preferences() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let first = profiles
            .set_avatar(Some(b"avatar".to_vec()))
            .unwrap()
            .unwrap();
        assert!(first.verify());
        assert!(profiles.set_avatar(first.avatar.clone()).unwrap().is_none());
        let clear = profiles.set_avatar(None).unwrap().unwrap();
        assert!(clear.updated_at > first.updated_at);
        drop(profiles);
        let reopened = open(dir.path(), &account);
        assert_eq!(reopened.current(), Some(clear));
        assert!(reopened.avatar_states().contains_key(&account.account_id()));
        assert!(reopened.avatars().is_empty());
    }
    #[test]
    fn older_signed_metadata_does_not_return_in_own_publication() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let old = ProfilePayload::new_with_language(
            &account,
            Some(b"sibling".to_vec()),
            100,
            Some("en".into()),
        );
        assert!(profiles.receive(&old).unwrap());
        let current = profiles.current().unwrap();
        assert_eq!(current.avatar, old.avatar);
        assert!(current.verify());
        assert!(!current.language_supported);
        assert!(current.language.is_none());
        let clear = profiles.set_avatar(None).unwrap().unwrap();
        assert!(clear.updated_at > old.updated_at);
        assert!(!clear.language_supported);
    }
    #[test]
    fn invalid_and_stale_profiles_cannot_replace_a_newer_avatar() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let newest = ProfilePayload::new(&account, Some(b"new".to_vec()), 100);
        assert!(profiles.receive(&newest).unwrap());
        let stale = ProfilePayload::new(&account, None, 99);
        assert!(!profiles.receive(&stale).unwrap());
        let mut tampered = ProfilePayload::new(&account, None, 101);
        tampered.avatar = Some(b"forged".to_vec());
        assert!(!profiles.receive(&tampered).unwrap());
        assert_eq!(profiles.current(), Some(newest));
    }
    #[test]
    fn restoring_a_clear_preserves_its_revision_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        profiles
            .restore_avatar(&account.account_id(), 50, None)
            .unwrap();
        drop(profiles);
        let reopened = open(dir.path(), &account);
        let current = reopened.current().unwrap();
        assert_eq!(current.updated_at, 50);
        assert_eq!(current.avatar, None);
        assert!(current.verify());
    }
    #[test]
    fn exhausted_profile_revision_rejects_edits_without_changing_current_or_disk() {
        for avatar in [Some(b"sibling".to_vec()), None] {
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let profiles = open(dir.path(), &account);
            let exhausted = ProfilePayload::new(&account, avatar.clone(), u64::MAX);
            assert!(profiles.receive(&exhausted).unwrap());
            assert!(profiles.set_avatar(avatar.clone()).unwrap().is_none());
            let path = dir.path().join("profiles.log");
            let before = std::fs::read(&path).unwrap();
            let changed = if avatar.is_some() {
                None
            } else {
                Some(b"edit".to_vec())
            };
            assert!(
                profiles.set_avatar(changed).is_err(),
                "a tied revision cannot be acknowledged as a saved edit"
            );
            assert_eq!(profiles.current(), Some(exhausted.clone()));
            assert_eq!(profiles.avatar(&account.account_id()), avatar);
            assert_eq!(std::fs::read(&path).unwrap(), before);
            drop(profiles);
            assert_eq!(open(dir.path(), &account).current(), Some(exhausted));
        }
    }

    #[test]
    fn last_profile_revision_is_saved_once_then_further_edits_fail() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        assert!(
            profiles
                .receive(&ProfilePayload::new(
                    &account,
                    Some(b"prior".to_vec()),
                    u64::MAX - 1
                ))
                .unwrap()
        );
        let clear = profiles.set_avatar(None).unwrap().unwrap();
        assert_eq!(clear.updated_at, u64::MAX);
        assert!(clear.avatar.is_none());
        assert!(profiles.set_avatar(Some(b"next".to_vec())).is_err());
        assert_eq!(profiles.current(), Some(clear.clone()));
        drop(profiles);
        assert_eq!(open(dir.path(), &account).current(), Some(clear));
    }
}

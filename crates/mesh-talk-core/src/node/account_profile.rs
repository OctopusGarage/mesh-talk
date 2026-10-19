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
        let revision = super::node::now_millis().max(
            state
                .avatars
                .updated_at(&account)
                .unwrap_or(0)
                .saturating_add(1),
        );
        let payload = ProfilePayload::new(&self.account, avatar.clone(), revision);
        state
            .avatars
            .record(&account, revision, avatar)
            .map_err(NodeError::Log)?;
        state.current = Some(payload.clone());
        Ok(Some(payload))
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
}

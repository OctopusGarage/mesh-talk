//! Owns account profile transitions and their two compatible encrypted journals.
//!
//! Callers supply host authorization around edits and authenticate device/account
//! bindings before reception. They never coordinate profile locks or journal order.
use super::{
    NodeError, language_policy,
    language_state::LanguageState,
    profile::{MAX_AVATAR_BYTES, ProfilePayload},
    profile_store::ProfileStore,
};
use crate::{
    eventlog::{EventId, LogError},
    identity::account::Account,
};
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
    language: LanguageState,
    current: Option<ProfilePayload>,
}

pub(super) struct ProfileSnapshot {
    pub avatars: Vec<(String, u64, Option<Vec<u8>>)>,
    pub language_profiles: Vec<ProfilePayload>,
    pub language_decisions: Vec<(EventId, bool)>,
}

impl AccountProfiles {
    pub fn new(account: &Account, avatars: ProfileStore, language: LanguageState) -> Self {
        let current = Self::reconstruct(account, &avatars, &language);
        Self {
            account: Account::from_secret_bytes(account.secret_bytes()),
            state: Mutex::new(State {
                avatars,
                language,
                current,
            }),
        }
    }

    /// With no avatar record, retain the signed policy's avatar (including on
    /// recovery from a first-journal-only write). A recorded clear remains authoritative.
    fn reconstruct(
        account: &Account,
        avatars: &ProfileStore,
        language: &LanguageState,
    ) -> Option<ProfilePayload> {
        let avatar = avatars.updated_at(&account.account_id()).map(|revision| {
            ProfilePayload::new(account, avatars.avatar(&account.account_id()), revision)
        });
        match (avatar, language.profile(&account.account_id())) {
            (Some(avatar), Some(policy)) => Some(ProfilePayload::new_with_language(
                account,
                avatar.avatar,
                avatar.updated_at.max(policy.updated_at),
                policy.language,
            )),
            (avatar, policy) => policy.or(avatar),
        }
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
        self.state()
            .avatars
            .snapshot_entries()
            .into_iter()
            .map(|(id, _, avatar)| (id, avatar))
            .collect()
    }
    pub fn language(&self, account: &str) -> Option<String> {
        self.state()
            .language
            .profile(account)
            .and_then(|profile| profile.language)
    }

    /// Atomically keep the other local fields and allocate the next revision.
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
        self.prepare(&mut state, avatar, None).map(Some)
    }

    pub fn set_language(&self, language: Option<String>) -> Result<ProfilePayload, NodeError> {
        Self::validate_language(language.as_deref())?;
        let mut state = self.state();
        let avatar = state.avatars.avatar(&self.account.account_id());
        self.prepare(&mut state, avatar, Some(language))
    }

    pub fn validate_language(language: Option<&str>) -> Result<(), NodeError> {
        if language.is_some_and(|code| !language_policy::is_supported(code)) {
            return Err(NodeError::InvalidInput(
                "unsupported message language".into(),
            ));
        }
        Ok(())
    }

    fn prepare(
        &self,
        state: &mut State,
        avatar: Option<Vec<u8>>,
        language: Option<Option<String>>,
    ) -> Result<ProfilePayload, NodeError> {
        let account = self.account.account_id();
        let previous = state.language.profile(&account);
        let modern = language.is_some() || previous.is_some();
        let language = language.unwrap_or_else(|| {
            previous
                .as_ref()
                .and_then(|profile| profile.language.clone())
        });
        let revision = super::node::now_millis().max(
            state
                .avatars
                .updated_at(&account)
                .unwrap_or(0)
                .max(previous.as_ref().map_or(0, |profile| profile.updated_at))
                .saturating_add(1),
        );
        let payload = if modern {
            ProfilePayload::new_with_language(&self.account, avatar.clone(), revision, language)
        } else {
            ProfilePayload::new(&self.account, avatar.clone(), revision)
        };
        // Preserve the established partial-failure contract: language is durable
        // first; current publication changes only after both journal writes succeed.
        if modern {
            state
                .language
                .record_profile(&payload)
                .map_err(NodeError::Log)?;
        }
        state
            .avatars
            .record(&account, revision, avatar)
            .map_err(NodeError::Log)?;
        state.current = Some(payload.clone());
        Ok(payload)
    }

    /// Apply a signed payload after the caller authenticates its device/account
    /// binding. Recheck the account signature here; a raw payload cannot bypass it.
    /// Returns whether the avatar advanced (language may advance independently).
    pub fn receive(&self, payload: &ProfilePayload) -> Result<bool, LogError> {
        if !payload.verify() {
            return Ok(false);
        }
        let mut state = self.state();
        let account = payload.account_id();
        if payload.language_supported {
            state.language.record_profile(payload)?;
        }
        let applied = state
            .avatars
            .record(&account, payload.updated_at, payload.avatar.clone())?;
        if account == self.account.account_id() {
            // Successful reception has already installed an avatar entry. Startup
            // additionally permits a policy-only journal after an interrupted edit.
            let merged = Self::reconstruct(&self.account, &state.avatars, &state.language);
            if let Some(profile) = merged.as_ref().filter(|profile| profile.language_supported) {
                state.language.record_profile(profile)?;
            }
            state.current = merged;
        }
        Ok(applied)
    }

    /// The internal synchronous WAL accept must not reenter this module. Host
    /// authorization runs outside this guard, as before; policy cannot change
    /// between evaluation and acceptance. No async work runs under the guard.
    pub fn authorize_language(
        &self,
        account: &str,
        text: &[u8],
        accept: &mut dyn FnMut() -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let state = self.state();
        if let Some(language) = state
            .language
            .profile(account)
            .and_then(|profile| profile.language)
        {
            let text = std::str::from_utf8(text)
                .map_err(|_| NodeError::InvalidInput("message is not UTF-8".into()))?;
            if !language_policy::evaluate(&language, text).allowed {
                return Err(NodeError::InvalidInput(format!(
                    "Recipient requires at least {}% {language} text. Please revise your message.",
                    language_policy::MIN_TARGET_PERCENT
                )));
            }
        }
        accept()
    }

    // Receive decisions remain immutable event observations, not profile revisions.
    pub fn hidden(&self, id: EventId) -> bool {
        self.state().language.hidden(id)
    }
    pub fn decide(&self, id: EventId, hidden: bool) -> Result<(), LogError> {
        self.state().language.decide(id, hidden)
    }

    pub fn snapshot(&self) -> ProfileSnapshot {
        let state = self.state();
        ProfileSnapshot {
            avatars: state.avatars.snapshot_entries(),
            language_profiles: state.language.snapshot_profiles(),
            language_decisions: state.language.snapshot_decisions(),
        }
    }

    /// Trusted snapshot installation preserves individual legacy records and
    /// tombstones. Reopening reconstructs publication after installation completes;
    /// importing does not synthesize new revisions or signed profile records.
    pub fn restore_avatar(
        &self,
        account: &str,
        revision: u64,
        avatar: Option<Vec<u8>>,
    ) -> Result<bool, LogError> {
        self.state().avatars.record(account, revision, avatar)
    }
    pub fn restore_language(&self, profile: &ProfilePayload) -> Result<bool, LogError> {
        self.state().language.record_profile(profile)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::EventId;
    use crate::identity::account::Account;
    use crate::node::{
        language_state::LanguageState, profile::ProfilePayload, profile_store::ProfileStore,
    };

    fn open(dir: &std::path::Path, account: &Account) -> AccountProfiles {
        AccountProfiles::new(
            account,
            ProfileStore::open(&dir.join("profiles.log"), "pw").unwrap(),
            LanguageState::open(&dir.join("language.log"), "pw").unwrap(),
        )
    }

    #[test]
    fn account_profile_legacy_merge_and_local_edits_keep_modern_policy() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let modern = profiles.set_language(Some("en".into())).unwrap();
        let legacy =
            ProfilePayload::new(&account, Some(b"sibling".to_vec()), modern.updated_at + 10);
        assert!(profiles.receive(&legacy).unwrap());
        let merged = profiles.current().unwrap();
        assert!(merged.verify());
        assert!(merged.language_supported);
        assert_eq!(merged.language.as_deref(), Some("en"));
        assert_eq!(merged.avatar, legacy.avatar);
        assert_eq!(merged.updated_at, legacy.updated_at);
        assert_eq!(
            profiles.snapshot().language_profiles[0].updated_at,
            merged.updated_at,
            "republished and effective policy revisions must agree"
        );
        assert!(!profiles.receive(&modern).unwrap());
        let edited = profiles.set_avatar(None).unwrap().unwrap();
        assert!(edited.updated_at > legacy.updated_at);
        assert_eq!(edited.language.as_deref(), Some("en"));
        assert!(profiles.set_avatar(None).unwrap().is_none());
        drop(profiles);
        let profiles = open(dir.path(), &account);
        let restored = profiles.current().unwrap();
        assert_eq!(restored.avatar, None);
        assert_eq!(restored.updated_at, edited.updated_at);
        assert_eq!(restored.language.as_deref(), Some("en"));
    }

    #[test]
    fn account_profile_startup_preserves_newer_avatar_and_policy_revisions() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut avatars = ProfileStore::open(&dir.path().join("profiles.log"), "pw").unwrap();
        avatars
            .record(&account.account_id(), 300, Some(b"avatar".to_vec()))
            .unwrap();
        let mut language = LanguageState::open(&dir.path().join("language.log"), "pw").unwrap();
        language
            .record_profile(&ProfilePayload::new_with_language(
                &account,
                None,
                200,
                Some("en".into()),
            ))
            .unwrap();
        let profiles = AccountProfiles::new(&account, avatars, language);
        let current = profiles.current().unwrap();
        assert_eq!(current.updated_at, 300);
        assert_eq!(current.avatar, Some(b"avatar".to_vec()));
        assert_eq!(current.language.as_deref(), Some("en"));
        assert!(current.verify());
    }

    #[test]
    fn account_profile_policy_only_startup_retains_avatar_until_recorded_clear() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let policy = ProfilePayload::new_with_language(
            &account,
            Some(b"policy avatar".to_vec()),
            200,
            Some("en".into()),
        );
        profiles.restore_language(&policy).unwrap();
        drop(profiles);
        let profiles = open(dir.path(), &account);
        assert_eq!(profiles.current().unwrap().encode(), policy.encode());
        profiles
            .restore_avatar(&account.account_id(), 300, None)
            .unwrap();
        drop(profiles);
        let profiles = open(dir.path(), &account);
        let current = profiles.current().unwrap();
        assert_eq!(current.avatar, None);
        assert_eq!(current.updated_at, 300);
        assert_eq!(current.language.as_deref(), Some("en"));
        assert!(current.verify());
    }

    #[test]
    fn account_profile_failed_avatar_persistence_keeps_publication_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        for revision in 1..=66 {
            profiles
                .restore_avatar(&account.account_id(), revision, None)
                .unwrap();
        }
        std::fs::create_dir(dir.path().join("profiles.compact-tmp")).unwrap();
        assert!(profiles.set_language(Some("en".into())).is_err());
        assert!(profiles.current().is_none());
        // The first durable journal still advances, as in the original two-log path.
        assert_eq!(
            profiles.language(&account.account_id()).as_deref(),
            Some("en")
        );
        std::fs::remove_dir(dir.path().join("profiles.compact-tmp")).unwrap();
        let retried = profiles.set_language(Some("en".into())).unwrap();
        assert!(retried.verify());
        assert_eq!(profiles.current().unwrap().updated_at, retried.updated_at);
    }
    #[test]
    fn account_profile_failed_language_persistence_keeps_previous_complete_state() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        for _ in 0..66 {
            profiles.set_language(Some("en".into())).unwrap();
        }
        let before = profiles.current().unwrap();
        std::fs::create_dir(dir.path().join("language.compact-tmp")).unwrap();
        assert!(profiles.set_language(Some("zh".into())).is_err());
        assert_eq!(profiles.current().unwrap().updated_at, before.updated_at);
        assert_eq!(
            profiles.language(&account.account_id()).as_deref(),
            Some("en")
        );
        std::fs::remove_dir(dir.path().join("language.compact-tmp")).unwrap();
        assert_eq!(profiles.set_language(None).unwrap().language, None);
    }

    #[test]
    fn account_profile_receive_decisions_survive_policy_changes_and_restart() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let profiles = open(dir.path(), &account);
        let id = EventId::new([7; 32]);
        profiles.decide(id, true).unwrap();
        profiles.set_language(Some("en".into())).unwrap();
        profiles.decide(id, false).unwrap();
        profiles.set_language(None).unwrap();
        assert!(profiles.hidden(id));
        drop(profiles);
        let reopened = open(dir.path(), &account);
        assert!(reopened.hidden(id));
        assert_eq!(reopened.language(&account.account_id()), None);
    }
}

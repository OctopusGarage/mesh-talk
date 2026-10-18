//! Local encrypted preferences and immutable receive decisions. Signed events stay intact.
use super::profile::ProfilePayload;
use crate::{
    eventlog::{EventId, LogError},
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};

#[derive(Clone, Serialize, Deserialize)]
enum Record {
    Profile(ProfilePayload),
    Decision { id: EventId, hidden: bool },
}

pub(super) struct LanguageState {
    file: EncryptedRecordLog<Record>,
    profiles: HashMap<String, ProfilePayload>,
    decisions: HashMap<EventId, bool>,
}
impl LanguageState {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::open(path, password, b"MTLANG")?;
        let mut state = Self {
            file,
            profiles: HashMap::new(),
            decisions: HashMap::new(),
        };
        for record in records {
            state.install(record);
        }
        Ok(state)
    }
    fn install(&mut self, record: Record) {
        match record {
            Record::Profile(profile) => {
                let account = profile.account_id();
                if profile.verify()
                    && profile.language_supported
                    && self.profiles.get(&account).is_none_or(|current| {
                        (profile.updated_at, &profile.language)
                            > (current.updated_at, &current.language)
                    })
                {
                    self.profiles.insert(account, profile);
                }
            }
            Record::Decision { id, hidden } => {
                self.decisions.entry(id).or_insert(hidden);
            }
        }
    }
    pub fn record_profile(&mut self, profile: &ProfilePayload) -> Result<bool, LogError> {
        if !profile.verify()
            || !profile.language_supported
            || self
                .profiles
                .get(&profile.account_id())
                .is_some_and(|current| {
                    (current.updated_at, &current.language)
                        >= (profile.updated_at, &profile.language)
                })
        {
            return Ok(false);
        }
        let record = Record::Profile(profile.clone());
        self.file.append_durable(&record)?;
        self.install(record);
        Ok(true)
    }
    pub fn profile(&self, account: &str) -> Option<ProfilePayload> {
        self.profiles.get(account).cloned()
    }
    pub fn snapshot_profiles(&self) -> Vec<ProfilePayload> {
        let mut profiles: Vec<_> = self.profiles.values().cloned().collect();
        profiles.sort_by_key(ProfilePayload::account_id);
        profiles
    }
    pub fn snapshot_decisions(&self) -> Vec<(EventId, bool)> {
        let mut decisions: Vec<_> = self
            .decisions
            .iter()
            .map(|(id, hidden)| (*id, *hidden))
            .collect();
        decisions.sort_by_key(|(id, _)| *id);
        decisions
    }
    pub fn hidden(&self, id: EventId) -> bool {
        self.decisions.get(&id).copied().unwrap_or(false)
    }
    pub fn decide(&mut self, id: EventId, hidden: bool) -> Result<(), LogError> {
        if self.decisions.contains_key(&id) {
            return Ok(());
        }
        let record = Record::Decision { id, hidden };
        self.file.append_durable(&record)?;
        self.install(record);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    #[test]
    fn preference_is_durable_and_rejects_stale_updates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("language.log");
        let account = Account::generate();
        let mut state = LanguageState::open(&path, "pw").unwrap();
        let current = ProfilePayload::new_with_language(&account, None, 200, Some("en".into()));
        assert!(state.record_profile(&current).unwrap());
        assert!(
            !state
                .record_profile(&ProfilePayload::new_with_language(
                    &account,
                    None,
                    100,
                    Some("zh".into())
                ))
                .unwrap()
        );
        drop(state);
        let state = LanguageState::open(&path, "pw").unwrap();
        assert_eq!(
            state
                .profile(&account.account_id())
                .unwrap()
                .language
                .as_deref(),
            Some("en")
        );
    }
}

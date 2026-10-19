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
    // Count profile records separately: immutable decisions must not grant an
    // arbitrarily large budget for obsolete avatar-bearing profile versions.
    profile_records: usize,
}
impl LanguageState {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::open(path, password, b"MTLANG")?;
        let mut state = Self {
            file,
            profiles: HashMap::new(),
            decisions: HashMap::new(),
            profile_records: records
                .iter()
                .filter(|record| matches!(record, Record::Profile(_)))
                .count(),
        };
        for record in records {
            state.install(record);
        }
        if state.profile_records > state.profiles.len() {
            state.compact_profiles()?;
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
        if self.profile_records >= self.profiles.len().saturating_mul(2).saturating_add(64) {
            self.compact_profiles()?;
        }
        let record = Record::Profile(profile.clone());
        self.file.append_durable(&record)?;
        self.profile_records += 1;
        self.install(record);
        Ok(true)
    }
    fn compact_profiles(&mut self) -> Result<(), LogError> {
        let mut records: Vec<_> = self
            .snapshot_profiles()
            .into_iter()
            .map(Record::Profile)
            .collect();
        records.extend(
            self.snapshot_decisions()
                .into_iter()
                .map(|(id, hidden)| Record::Decision { id, hidden }),
        );
        self.file.rewrite(&records)?;
        self.profile_records = self.profiles.len();
        Ok(())
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
    fn profile_compaction_preserves_immutable_receive_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("language.log");
        let account = Account::generate();
        let mut state = LanguageState::open(&path, "pw").unwrap();
        for index in 0u32..200 {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&index.to_le_bytes());
            state.decide(EventId::new(bytes), index % 2 == 0).unwrap();
        }
        for revision in 1..=200 {
            state
                .record_profile(&ProfilePayload::new_with_language(
                    &account,
                    None,
                    revision,
                    Some("en".into()),
                ))
                .unwrap();
        }
        drop(state);
        let (_, records) = EncryptedRecordLog::<Record>::open(&path, "pw", b"MTLANG").unwrap();
        let profile_count = records
            .iter()
            .filter(|record| matches!(record, Record::Profile(_)))
            .count();
        assert!(
            profile_count < 100,
            "receive decisions must not inflate the obsolete-profile budget"
        );
        let reopened = LanguageState::open(&path, "pw").unwrap();
        assert_eq!(reopened.snapshot_decisions().len(), 200);
        for index in 0u32..200 {
            let mut bytes = [0; 32];
            bytes[..4].copy_from_slice(&index.to_le_bytes());
            assert_eq!(reopened.hidden(EventId::new(bytes)), index % 2 == 0);
        }
        assert_eq!(
            reopened.profile(&account.account_id()).unwrap().updated_at,
            200
        );
        drop(reopened);
        let (_, records) = EncryptedRecordLog::<Record>::open(&path, "pw", b"MTLANG").unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|r| matches!(r, Record::Profile(_)))
                .count(),
            1
        );
    }

    #[test]
    fn failed_language_compaction_preserves_profile_and_decisions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("language.log");
        let account = Account::generate();
        let mut state = LanguageState::open(&path, "pw").unwrap();
        let id = EventId::new([7; 32]);
        state.decide(id, true).unwrap();
        for revision in 1..=66 {
            state
                .record_profile(&ProfilePayload::new_with_language(
                    &account,
                    None,
                    revision,
                    Some("en".into()),
                ))
                .unwrap();
        }
        std::fs::create_dir(path.with_extension("compact-tmp")).unwrap();
        assert!(
            state
                .record_profile(&ProfilePayload::new_with_language(
                    &account,
                    None,
                    67,
                    Some("zh".into())
                ))
                .is_err()
        );
        assert_eq!(state.profile(&account.account_id()).unwrap().updated_at, 66);
        assert!(state.hidden(id));
        std::fs::remove_dir(path.with_extension("compact-tmp")).unwrap();
        state
            .record_profile(&ProfilePayload::new_with_language(
                &account,
                None,
                67,
                Some("zh".into()),
            ))
            .unwrap();
    }

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

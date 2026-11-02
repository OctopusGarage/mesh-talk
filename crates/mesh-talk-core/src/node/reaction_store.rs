//! Durable locally authored DM and explicitly imported channel reactions.
//! Legacy MTREAC files migrate atomically to the versioned MTREA2 record family.

use super::reaction::ReactionPayload;
#[cfg(test)]
use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::{account::DeviceCertificate, device::PublicIdentity};
#[cfg(test)]
use crate::storage::record_log::EncryptedRecordLog;
#[cfg(test)]
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::{collections::HashMap, io::Read, path::Path};

#[cfg(test)]
type ReactionKey = (ConversationId, EventId, String);
#[cfg(test)]
type ReactionValue = (u64, ReactionPayload);
#[cfg(test)]
type Entries = HashMap<ReactionKey, ReactionValue>;
pub(in crate::node) type RetainedChannelReaction = (
    PublicIdentity,
    EventId,
    u64,
    ReactionPayload,
    Option<[u8; 32]>,
);
#[cfg(test)]
type ChannelKey = (ConversationId, EventId, String, [u8; 32], Option<[u8; 32]>);
const LEGACY_MAGIC: &[u8; 6] = b"MTREAC";
const MAGIC: &[u8; 6] = b"MTREA2";
// Match the existing local delivery journal byte budget and request-index bound.
// These limits apply only to the new explicit channel projection family.
const MAX_CHANNEL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHANNEL_ENTRIES: usize = 100_000;

#[derive(Clone, Serialize, Deserialize)]
pub(in crate::node) struct Record {
    pub conversation: ConversationId,
    pub wall: u64,
    pub payload: ReactionPayload,
}
#[derive(Clone, Serialize, Deserialize)]
struct ChannelRecord {
    conversation: ConversationId,
    source: PublicIdentity,
    effect_id: EventId,
    wall: u64,
    payload: ReactionPayload,
    #[serde(skip)]
    certificate: Option<DeviceCertificate>,
}
// Append-only positional tags within the explicit MTREA2 family.
#[derive(Clone, Serialize, Deserialize)]
enum StoreRecord {
    OwnDm(Record),
    Channel(ChannelRecord),
    ChannelBound {
        record: ChannelRecord,
        source_certificate: DeviceCertificate,
    },
}

#[cfg(test)]
pub(super) struct ReactionStore {
    file: EncryptedRecordLog<StoreRecord>,
    entries: Entries,
    channels: HashMap<ChannelKey, ChannelRecord>,
    channel_bytes: u64,
    records: usize,
}

#[cfg(test)]
fn channel_key(record: &ChannelRecord) -> ChannelKey {
    (
        record.conversation,
        record.payload.target,
        record.payload.emoji.clone(),
        record.source.ed25519_pub,
        record
            .certificate
            .as_ref()
            .map(|certificate| certificate.account_ed25519_pub),
    )
}
fn channel_record(record: ChannelRecord) -> StoreRecord {
    match record.certificate.clone() {
        Some(source_certificate) => StoreRecord::ChannelBound {
            record,
            source_certificate,
        },
        None => StoreRecord::Channel(record),
    }
}
#[cfg(test)]
fn channel_size(record: &ChannelRecord) -> Result<u64, LogError> {
    bincode::serialized_size(&channel_record(record.clone()))
        .map(|size| size.saturating_add(32))
        .map_err(|error| LogError::Serialization(error.to_string()))
}
#[cfg(test)]
fn replace_action(
    wall: u64,
    payload: &ReactionPayload,
    old_wall: u64,
    old: &ReactionPayload,
) -> bool {
    wall > old_wall || (wall == old_wall && payload.remove && !old.remove)
}

#[cfg(test)]
impl ReactionStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        Self::open_with_migration_hook(path, password, || Ok(()))
    }
    fn open_with_migration_hook(
        path: &Path,
        password: &str,
        before_publish: impl FnOnce() -> Result<(), LogError>,
    ) -> Result<Self, LogError> {
        let legacy = match std::fs::File::open(path) {
            Ok(mut file) => {
                let mut magic = [0; 6];
                file.read_exact(&mut magic)?;
                &magic == LEGACY_MAGIC
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        if legacy {
            let (old, records): (EncryptedRecordLog<Record>, _) =
                EncryptedRecordLog::open(path, password, LEGACY_MAGIC)?;
            drop(old);
            let mut nonce = [0; 16];
            rand_core::OsRng.fill_bytes(&mut nonce);
            let temporary = path.with_extension(format!("reaction-migrate-{}", hex::encode(nonce)));
            let result = (|| {
                let (mut replacement, _): (EncryptedRecordLog<StoreRecord>, _) =
                    EncryptedRecordLog::open(&temporary, password, MAGIC)?;
                for record in records {
                    replacement.append(&StoreRecord::OwnDm(record))?;
                }
                replacement.sync()?;
                drop(replacement);
                before_publish()?;
                std::fs::rename(&temporary, path)?;
                // Publication committed: the complete old logical state is now
                // in the new format. Match record_log::rewrite after rename.
                if super::media_store::sync_parent(path).is_err() {
                    log::warn!("reaction migration directory sync failed");
                }
                Ok::<_, LogError>(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            result?;
        }
        let (file, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let mut store = Self {
            file,
            entries: HashMap::new(),
            channels: HashMap::new(),
            channel_bytes: 0,
            records: records.len(),
        };
        for record in records {
            store.merge(record)?;
        }
        if store.channels.len() > MAX_CHANNEL_ENTRIES || store.channel_bytes > MAX_CHANNEL_BYTES {
            return Err(LogError::CorruptFile(
                "channel reaction projection exceeds capacity".into(),
            ));
        }
        Ok(store)
    }
    fn validate_channel_record(&self, record: &ChannelRecord) -> Result<(), LogError> {
        if self.channels.values().any(|old| {
            old.effect_id == record.effect_id
                && (old.conversation != record.conversation
                    || old.source != record.source
                    || old.wall != record.wall
                    || old.payload != record.payload
                    || old
                        .certificate
                        .as_ref()
                        .zip(record.certificate.as_ref())
                        .is_some_and(|(old, new)| {
                            old.account_ed25519_pub != new.account_ed25519_pub
                        }))
        }) {
            return Err(LogError::CorruptFile(
                "conflicting channel reaction provenance".into(),
            ));
        }
        Ok(())
    }
    fn merge(&mut self, record: StoreRecord) -> Result<(), LogError> {
        let record = match record {
            StoreRecord::ChannelBound {
                mut record,
                source_certificate,
            } => {
                if !source_certificate.verify()
                    || source_certificate.device_ed25519_pub != record.source.ed25519_pub
                {
                    return Err(LogError::CorruptFile(
                        "invalid bound channel reaction certificate".into(),
                    ));
                }
                record.certificate = Some(source_certificate);
                StoreRecord::Channel(record)
            }
            other => other,
        };
        match record {
            StoreRecord::ChannelBound { .. } => unreachable!(),
            StoreRecord::OwnDm(record) => {
                let key = (
                    record.conversation,
                    record.payload.target,
                    record.payload.emoji.clone(),
                );
                if self.entries.get(&key).is_none_or(|(wall, payload)| {
                    replace_action(record.wall, &record.payload, *wall, payload)
                }) {
                    self.entries.insert(key, (record.wall, record.payload));
                }
            }
            StoreRecord::Channel(record) => {
                self.validate_channel_record(&record)?;
                let key = channel_key(&record);
                if self.channels.get(&key).is_none_or(|old| {
                    replace_action(record.wall, &record.payload, old.wall, &old.payload)
                }) {
                    let old_size = self
                        .channels
                        .get(&key)
                        .map(channel_size)
                        .transpose()?
                        .unwrap_or(0);
                    self.channel_bytes = self
                        .channel_bytes
                        .saturating_sub(old_size)
                        .saturating_add(channel_size(&record)?);
                    self.channels.insert(key, record);
                }
            }
        }
        Ok(())
    }
    fn compact_before_append(&mut self) -> Result<(), LogError> {
        if self.records
            >= (self.entries.len() + self.channels.len())
                .saturating_mul(2)
                .saturating_add(64)
        {
            let current = self.records_kept(|_, _, _| false);
            self.file.rewrite(&current)?;
            self.records = current.len();
        }
        Ok(())
    }
    pub(super) fn record(
        &mut self,
        conversation: ConversationId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), LogError> {
        let key = (conversation, payload.target, payload.emoji.clone());
        if self
            .entries
            .get(&key)
            .is_some_and(|(old_wall, old)| !replace_action(wall, &payload, *old_wall, old))
        {
            return Ok(());
        }
        self.compact_before_append()?;
        let record = StoreRecord::OwnDm(Record {
            conversation,
            wall,
            payload,
        });
        self.file.append_durable(&record)?;
        self.records += 1;
        self.merge(record)
    }
    /// The caller validates the signed committed source result and exact intent
    /// before this private projection boundary. Preserve the original author.
    #[cfg(test)]
    pub(in crate::node) fn record_channel(
        &mut self,
        conversation: ConversationId,
        source: PublicIdentity,
        effect_id: EventId,
        wall: u64,
        payload: ReactionPayload,
    ) -> Result<(), LogError> {
        if effect_id == EventId::new([0; 32]) {
            return Err(LogError::CorruptFile(
                "invalid channel reaction effect".into(),
            ));
        }
        self.record_channel_with_certificate(conversation, source, effect_id, wall, payload, None)
    }
    fn record_channel_with_certificate(
        &mut self,
        conversation: ConversationId,
        source: PublicIdentity,
        effect_id: EventId,
        wall: u64,
        payload: ReactionPayload,
        certificate: Option<DeviceCertificate>,
    ) -> Result<(), LogError> {
        if effect_id == EventId::new([0; 32]) {
            return Err(LogError::CorruptFile(
                "invalid channel reaction effect".into(),
            ));
        }
        let record = ChannelRecord {
            conversation,
            source,
            effect_id,
            wall,
            payload,
            certificate,
        };
        self.validate_channel_record(&record)?;
        let key = channel_key(&record);
        let old = self.channels.get(&key);
        if old.is_some_and(|old| {
            !replace_action(record.wall, &record.payload, old.wall, &old.payload)
        }) {
            return Ok(());
        }
        let next_bytes = self
            .channel_bytes
            .saturating_sub(old.map(channel_size).transpose()?.unwrap_or(0))
            .saturating_add(channel_size(&record)?);
        if next_bytes > MAX_CHANNEL_BYTES
            || (old.is_none() && self.channels.len() >= MAX_CHANNEL_ENTRIES)
        {
            return Err(LogError::CorruptFile(
                "channel reaction projection capacity exhausted".into(),
            ));
        }
        self.compact_before_append()?;
        let record = channel_record(record);
        self.file.append_durable(&record)?;
        self.records += 1;
        self.merge(record)
    }
    #[cfg(test)]
    pub(in crate::node) fn channel_entries(
        &self,
        conversation: ConversationId,
    ) -> Vec<(PublicIdentity, EventId, u64, ReactionPayload)> {
        self.channels
            .values()
            .filter(|row| row.conversation == conversation)
            .map(|row| {
                (
                    row.source.clone(),
                    row.effect_id,
                    row.wall,
                    row.payload.clone(),
                )
            })
            .collect()
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = (&ReactionKey, &ReactionValue)> {
        self.entries.iter()
    }
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
    pub(super) fn remove_where(
        &mut self,
        remove: impl Fn(ConversationId, u64, &ReactionPayload) -> bool,
    ) -> Result<(), LogError> {
        let kept = self.records_kept(remove);
        if kept.len() != self.entries.len() + self.channels.len() {
            self.file.rewrite(&kept)?;
            self.records = kept.len();
            self.entries.clear();
            self.channels.clear();
            self.channel_bytes = 0;
            for record in kept {
                self.merge(record)?;
            }
        }
        Ok(())
    }
    fn records_kept(
        &self,
        remove: impl Fn(ConversationId, u64, &ReactionPayload) -> bool,
    ) -> Vec<StoreRecord> {
        self.entries
            .iter()
            .filter(|((conv, _, _), (wall, payload))| !remove(*conv, *wall, payload))
            .map(|((conv, _, _), (wall, payload))| {
                StoreRecord::OwnDm(Record {
                    conversation: *conv,
                    wall: *wall,
                    payload: payload.clone(),
                })
            })
            .chain(
                self.channels
                    .values()
                    .filter(|row| !remove(row.conversation, row.wall, &row.payload))
                    .cloned()
                    .map(channel_record),
            )
            .collect()
    }
    #[cfg(test)]
    fn open_fail_migration(path: &Path, password: &str) -> Result<Self, LogError> {
        Self::open_with_migration_hook(path, password, || {
            Err(LogError::CorruptFile(
                "injected migration publication failure".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_projection_compacts_and_preserves_remove_ties() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reactions.log");
        let conv = ConversationId::new([1; 32]);
        let payload = |remove| ReactionPayload {
            target: EventId::new([2; 32]),
            emoji: "👍".into(),
            remove,
        };
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        for wall in 0..200 {
            store.record(conv, wall, payload(wall % 2 == 1)).unwrap();
        }
        store.record(conv, 199, payload(false)).unwrap();
        drop(store);
        let (_, records) = EncryptedRecordLog::<StoreRecord>::open(&path, "pw", MAGIC).unwrap();
        assert!(records.len() < 70, "toggle history is compacted");
        let reopened = ReactionStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.len(), 1);
        assert!(
            reopened.iter().next().unwrap().1.1.remove,
            "equal-time add cannot resurrect remove"
        );
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn payload(remove: bool) -> ReactionPayload {
        ReactionPayload {
            target: EventId::new([31; 32]),
            emoji: "👍".into(),
            remove,
        }
    }
    fn legacy(path: &Path, conversation: ConversationId) {
        let (mut log, _): (EncryptedRecordLog<Record>, _) =
            EncryptedRecordLog::open(path, "pw", b"MTREAC").unwrap();
        log.append_durable(&Record {
            conversation,
            wall: 1,
            payload: payload(false),
        })
        .unwrap();
    }
    #[test]
    fn legacy_reaction_file_migrates_explicitly_and_reopens_without_losing_dm_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        let conv = ConversationId::new([30; 32]);
        legacy(&path, conv);
        let store = ReactionStore::open(&path, "pw").unwrap();
        assert_eq!(store.iter().count(), 1);
        drop(store);
        assert_eq!(&std::fs::read(&path).unwrap()[..6], b"MTREA2");
        let reopened = ReactionStore::open(&path, "pw").unwrap();
        let ((conversation, target, emoji), (wall, value)) = reopened.iter().next().unwrap();
        assert_eq!(
            (*conversation, *target, emoji.as_str(), *wall, value.remove),
            (conv, payload(false).target, "👍", 1, false)
        );
    }
    #[test]
    fn channel_reactions_retain_two_original_authors_across_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        let conv = ConversationId::new([30; 32]);
        let first = DeviceIdentity::generate().public();
        let second = DeviceIdentity::generate().public();
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        store
            .record_channel(
                conv,
                first.clone(),
                EventId::new([32; 32]),
                10,
                payload(false),
            )
            .unwrap();
        store
            .record_channel(
                conv,
                second.clone(),
                EventId::new([33; 32]),
                11,
                payload(false),
            )
            .unwrap();
        drop(store);
        let store = ReactionStore::open(&path, "pw").unwrap();
        let rows = store.channel_entries(conv);
        let views = super::super::reaction::aggregate(
            &rows
                .iter()
                .map(|(source, _, wall, payload)| (source.user_id(), *wall, payload.clone()))
                .collect::<Vec<_>>(),
        );
        assert_eq!(views.len(), 1);
        let mut authors = vec![first.user_id(), second.user_id()];
        authors.sort();
        assert_eq!(views[0].who, authors);
        assert!(
            store.iter().next().is_none(),
            "channel rows must not become local DM reactions"
        );
    }
    #[test]
    fn channel_remove_wins_timestamp_tie_without_changing_source() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConversationId::new([30; 32]);
        let source = DeviceIdentity::generate().public();
        let mut store = ReactionStore::open(&dir.path().join("reaction"), "pw").unwrap();
        for (effect, remove) in [(32, false), (33, true), (34, false)] {
            store
                .record_channel(
                    conv,
                    source.clone(),
                    EventId::new([effect; 32]),
                    10,
                    payload(remove),
                )
                .unwrap();
        }
        let rows = store.channel_entries(conv);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, source);
        assert_eq!(rows[0].1, EventId::new([33; 32]));
        assert!(rows[0].3.remove);
    }
    #[test]
    fn public_and_private_reaction_copies_reduce_to_one_original_author() {
        let dir = tempfile::tempdir().unwrap();
        let conv = ConversationId::new([30; 32]);
        let source = DeviceIdentity::generate().public();
        let mut store = ReactionStore::open(&dir.path().join("reaction"), "pw").unwrap();
        store
            .record_channel(
                conv,
                source.clone(),
                EventId::new([32; 32]),
                10,
                payload(false),
            )
            .unwrap();
        let mut decoded = vec![(source.user_id(), 10, payload(false))];
        decoded.extend(
            store
                .channel_entries(conv)
                .into_iter()
                .map(|(source, _, wall, payload)| (source.user_id(), wall, payload)),
        );
        assert_eq!(
            super::super::reaction::aggregate(&decoded)[0].who,
            vec![source.user_id()]
        );
    }
    #[test]
    fn failed_channel_append_preserves_existing_projection_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        let conv = ConversationId::new([30; 32]);
        let source = DeviceIdentity::generate().public();
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        store
            .record_channel(
                conv,
                source.clone(),
                EventId::new([32; 32]),
                10,
                payload(false),
            )
            .unwrap();
        store.file.fail_appends_for_test(true).unwrap();
        assert!(
            store
                .record_channel(conv, source, EventId::new([33; 32]), 11, payload(true))
                .is_err()
        );
        assert!(!store.channel_entries(conv)[0].3.remove);
        drop(store);
        assert!(
            !ReactionStore::open(&path, "pw")
                .unwrap()
                .channel_entries(conv)[0]
                .3
                .remove
        );
    }
    #[test]
    fn failed_migration_leaves_the_legacy_file_readable_and_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        legacy(&path, ConversationId::new([30; 32]));
        let before = std::fs::read(&path).unwrap();
        assert!(ReactionStore::open_fail_migration(&path, "pw").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let (_, rows): (EncryptedRecordLog<Record>, _) =
            EncryptedRecordLog::open(&path, "pw", b"MTREAC").unwrap();
        assert_eq!(rows.len(), 1);
    }
    #[test]
    fn channel_live_budget_rejection_preserves_prior_records_and_file_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        let conv = ConversationId::new([30; 32]);
        let source = DeviceIdentity::generate().public();
        let effect = EventId::new([32; 32]);
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        store
            .record_channel(conv, source.clone(), effect, 10, payload(false))
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let oversized = ReactionPayload {
            emoji: "x".repeat(MAX_CHANNEL_BYTES as usize),
            ..payload(false)
        };
        assert!(
            store
                .record_channel(conv, source, EventId::new([33; 32]), 11, oversized)
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(store.channel_entries(conv).len(), 1);
        assert_eq!(store.channel_entries(conv)[0].1, effect);
        drop(store);
        let reopened = ReactionStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.channel_entries(conv).len(), 1);
        assert_eq!(reopened.channel_entries(conv)[0].1, effect);
    }

    #[test]
    fn mixed_family_compaction_and_pruning_preserve_independent_retained_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reaction");
        let dm = ConversationId::new([29; 32]);
        let group = ConversationId::new([30; 32]);
        let first = DeviceIdentity::generate().public();
        let second = DeviceIdentity::generate().public();
        let retained_dm = ReactionPayload {
            target: EventId::new([40; 32]),
            ..payload(false)
        };
        let retained_channel = ReactionPayload {
            target: EventId::new([41; 32]),
            ..payload(false)
        };
        let mut store = ReactionStore::open(&path, "pw").unwrap();
        store.record(group, 1, payload(false)).unwrap();
        store
            .record_channel(
                group,
                second.clone(),
                EventId::new([33; 32]),
                1,
                payload(false),
            )
            .unwrap();
        store
            .record_channel(
                group,
                first.clone(),
                EventId::new([34; 32]),
                1,
                retained_channel.clone(),
            )
            .unwrap();
        for wall in 1u64..201 {
            let mut effect = [32; 32];
            effect[..8].copy_from_slice(&wall.to_le_bytes());
            store
                .record_channel(
                    group,
                    first.clone(),
                    EventId::new(effect),
                    wall,
                    payload(wall % 2 == 0),
                )
                .unwrap();
            let value = ReactionPayload {
                remove: wall % 2 == 0,
                ..retained_dm.clone()
            };
            store.record(dm, wall, value).unwrap();
        }
        assert_eq!(store.channel_entries(group).len(), 3);
        assert_eq!(store.iter().count(), 2);
        let (_, records): (EncryptedRecordLog<StoreRecord>, _) =
            EncryptedRecordLog::open(&path, "pw", MAGIC).unwrap();
        assert!(
            records.len() < 85,
            "compaction retains both families without per-toggle growth"
        );
        store
            .remove_where(|_, _, value| value.target == payload(false).target)
            .unwrap();
        drop(store);
        let reopened = ReactionStore::open(&path, "pw").unwrap();
        let ((conversation, target, _), (wall, value)) = reopened.iter().next().unwrap();
        assert_eq!(
            (*conversation, *target, *wall, value.remove),
            (dm, retained_dm.target, 200, true)
        );
        assert_eq!(reopened.iter().count(), 1);
        let rows = reopened.channel_entries(group);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, first);
        assert_eq!(rows[0].3, retained_channel);
        assert_eq!(rows[0].1, EventId::new([34; 32]));
    }
}

mod indexed;
pub(in crate::node) use indexed::IndexedReactionStore;

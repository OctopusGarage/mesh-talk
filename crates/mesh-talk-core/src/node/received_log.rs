//! A local-only, password-encrypted sidecar recording the plaintext of DMs this
//! node RECEIVED. Inbound DMs are decrypted then discarded; this store keeps a
//! private plaintext copy so history can show both sides. Certified devices of the
//! same account may exchange eligible history through account sync; unrelated peers
//! never receive this sidecar. The records hold no signatures. The on-disk framing is the
//! shared [`EncryptedRecordLog`] (magic + salt header, then length-prefixed
//! AES-256-GCM records); this store layers [`ReceivedEntry`] records, a
//! per-conversation index, and an event-id dedup index on top. The file-card
//! variant permits an identical signed manifest under two host conversations.

use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTRECV";

/// One received message's local plaintext record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceivedEntry {
    pub event_id: crate::eventlog::event::EventId,
    pub conversation: ConversationId,
    pub from: String,
    pub wall_clock: u64,
    pub plaintext: Vec<u8>,
}

/// An append-only, password-encrypted log of locally-received message plaintext,
/// indexed in memory by conversation.
pub struct ReceivedLog {
    file: EncryptedRecordLog<ReceivedEntry>,
    revision: u64,
    rewrite_revision: u64,
    by_conversation: HashMap<ConversationId, Vec<ReceivedEntry>>,
    allow_aliases: bool,
    /// First host for each event ID. Message logs reject another host; file-card
    /// logs may append a matching alias under another host.
    seen: HashMap<EventId, (ConversationId, usize)>,
    #[cfg(test)]
    fail_once_after: Option<usize>,
}

impl ReceivedLog {
    /// File cards can have a device-conversation row and an account-conversation
    /// row for the same signed manifest. Message logs keep event IDs unique.
    pub(crate) fn open_file_cards(path: &Path, password: &str) -> Result<Self, LogError> {
        Self::open_inner(path, password, true)
    }

    pub(crate) fn record_file_alias_durable(
        &mut self,
        entry: &ReceivedEntry,
    ) -> Result<(), LogError> {
        self.file.recover_failed_append()?;
        if !self.allow_aliases {
            return Err(LogError::CorruptFile("file alias in message log".into()));
        }
        if let Some(existing) = self
            .by_conversation
            .get(&entry.conversation)
            .and_then(|entries| entries.iter().find(|row| row.event_id == entry.event_id))
        {
            if existing != entry {
                return Err(LogError::CorruptFile("conflicting file alias".into()));
            }
            return self.file.sync();
        }
        if let Some(existing) = self.entry(entry.event_id)
            && !Self::same_alias_content(existing, entry)
        {
            return Err(LogError::CorruptFile("conflicting file alias".into()));
        }
        self.file.append_durable(entry)?;
        let entries = self.by_conversation.entry(entry.conversation).or_default();
        self.seen
            .entry(entry.event_id)
            .or_insert((entry.conversation, entries.len()));
        entries.push(entry.clone());
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    fn same_alias_content(a: &ReceivedEntry, b: &ReceivedEntry) -> bool {
        a.from == b.from && a.wall_clock == b.wall_clock && a.plaintext == b.plaintext
    }
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.file.fail_appends_for_test(enabled)
    }
    #[cfg(test)]
    pub(crate) fn fail_next_append_rollback_for_test(&mut self, bytes: usize) {
        self.file.fail_next_append_rollback_for_test(bytes);
    }
    #[cfg(test)]
    pub(crate) fn fail_once_after_for_test(&mut self, successful: usize) {
        self.fail_once_after = Some(successful);
    }
    pub(crate) fn entry(&self, id: EventId) -> Option<&ReceivedEntry> {
        let (conversation, index) = self.seen.get(&id)?;
        self.by_conversation.get(conversation)?.get(*index)
    }
    /// Durably install a validated receive transaction. Retry synchronizes even
    /// when the identical record is already indexed; conflicting plaintext or
    /// routing for the same signed event is rejected.
    pub fn record_durable(&mut self, entry: &ReceivedEntry) -> Result<(), LogError> {
        self.file.recover_failed_append()?;
        if let Some((conversation, index)) = self.seen.get(&entry.event_id) {
            let existing = &self.by_conversation[conversation][*index];
            if existing != entry {
                return Err(LogError::CorruptFile(
                    "conflicting received transaction".into(),
                ));
            }
            return self.file.sync();
        }
        self.file.append_durable(entry)?;
        let entries = self.by_conversation.entry(entry.conversation).or_default();
        self.seen
            .insert(entry.event_id, (entry.conversation, entries.len()));
        entries.push(entry.clone());
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }
    /// Open (or create) the received log at `path`, replaying stored entries.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        Self::open_inner(path, password, false)
    }

    fn open_inner(path: &Path, password: &str, allow_aliases: bool) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<ReceivedEntry>::open(path, password, MAGIC)?;
        let mut by_conversation: HashMap<ConversationId, Vec<ReceivedEntry>> = HashMap::new();
        let mut seen: HashMap<EventId, (ConversationId, usize)> = HashMap::new();
        for entry in entries {
            if let Some((conversation, index)) = seen.get(&entry.event_id) {
                let existing = &by_conversation[conversation][*index];
                if existing != &entry
                    && (!allow_aliases
                        || existing.conversation == entry.conversation
                        || !Self::same_alias_content(existing, &entry))
                {
                    return Err(LogError::CorruptFile(
                        "conflicting received transaction".into(),
                    ));
                }
                if existing == &entry {
                    continue;
                }
            }
            let entries = by_conversation.entry(entry.conversation).or_default();
            seen.entry(entry.event_id)
                .or_insert((entry.conversation, entries.len()));
            entries.push(entry);
        }
        Ok(Self {
            file,
            by_conversation,
            revision: 0,
            rewrite_revision: 0,
            seen,
            allow_aliases,
            #[cfg(test)]
            fail_once_after: None,
        })
    }

    /// Record a received message's plaintext: append an encrypted record, then index.
    pub fn record(
        &mut self,
        conversation: ConversationId,
        from: String,
        wall_clock: u64,
        plaintext: &[u8],
        event_id: crate::eventlog::event::EventId,
    ) -> Result<(), LogError> {
        #[cfg(test)]
        if let Some(remaining) = self.fail_once_after.as_mut() {
            if *remaining == 0 {
                self.fail_once_after = None;
                return Err(LogError::Io(std::io::Error::other(
                    "injected receive projection failure",
                )));
            }
            *remaining -= 1;
        }
        self.file.recover_failed_append()?;
        // Idempotent on event id: skip a duplicate (e.g. a re-imported account backfill) so it
        // can't durably double the conversation's history.
        if self.seen.contains_key(&event_id) {
            return Ok(());
        }
        let entry = ReceivedEntry {
            event_id,
            conversation,
            from,
            wall_clock,
            plaintext: plaintext.to_vec(),
        };
        self.file.append(&entry)?;
        let entries = self.by_conversation.entry(conversation).or_default();
        self.seen.insert(event_id, (conversation, entries.len()));
        entries.push(entry);
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Remove every entry matching `should_remove`, atomically rewriting the file and
    /// rebuilding BOTH the per-conversation index and the `seen` dedup set. Returns how many
    /// were removed; a no-op (no rewrite) when nothing matches. The local-erase primitive
    /// behind delete-a-message / clear-history / retention.
    ///
    /// Rebuilding `seen` from the kept records is deliberate: a removed message's event stays
    /// in the synced (ciphertext) event log, deduped by id, so it is NOT re-ingested here on a
    /// later sync — the erased plaintext stays gone on this device.
    pub fn remove_where(
        &mut self,
        should_remove: impl Fn(&ReceivedEntry) -> bool,
    ) -> Result<usize, LogError> {
        self.file.recover_failed_append()?;
        let total: usize = self.by_conversation.values().map(Vec::len).sum();
        let kept: Vec<ReceivedEntry> = self
            .by_conversation
            .values()
            .flatten()
            .filter(|e| !should_remove(e))
            .cloned()
            .collect();
        let removed = total - kept.len();
        if removed == 0 {
            return Ok(0);
        }
        self.file.rewrite(&kept)?;
        self.by_conversation.clear();
        self.seen.clear();
        for entry in kept {
            let entries = self.by_conversation.entry(entry.conversation).or_default();
            self.seen
                .entry(entry.event_id)
                .or_insert((entry.conversation, entries.len()));
            entries.push(entry);
        }
        self.revision = self.revision.wrapping_add(1);
        self.rewrite_revision = self.rewrite_revision.wrapping_add(1);
        Ok(removed)
    }

    /// All received entries for `conversation`, sorted by `wall_clock`.
    pub fn entries(&self, conversation: &ConversationId) -> Vec<ReceivedEntry> {
        let mut v = self
            .by_conversation
            .get(conversation)
            .cloned()
            .unwrap_or_default();
        v.sort_by_key(|e| e.wall_clock);
        v
    }

    /// Borrow this conversation's entries WITHOUT cloning (in insertion order — callers
    /// that need them sorted use [`Self::entries`]). For read-only scans (e.g. search)
    /// that would otherwise clone every plaintext.
    pub fn entries_ref(&self, conversation: &ConversationId) -> &[ReceivedEntry] {
        self.by_conversation
            .get(conversation)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Process-local invalidation for archive selectors; wire/storage stay unchanged.
    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }
    pub(crate) fn rewrite_revision(&self) -> u64 {
        self.rewrite_revision
    }

    /// The conversation ids this log holds entries for.
    pub fn conversations(&self) -> Vec<ConversationId> {
        self.by_conversation.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::eventlog::event::EventId;

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn partial_rollback_failure_recovers_same_live_received_log_without_duplicates() {
        for durable in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("received");
            let mut log = ReceivedLog::open(&path, "pw").unwrap();
            let record = |log: &mut ReceivedLog, n, plaintext: &[u8]| {
                let entry = ReceivedEntry {
                    event_id: EventId::new([n; 32]),
                    conversation: conv(1),
                    from: "alice".into(),
                    wall_clock: u64::from(n),
                    plaintext: plaintext.to_vec(),
                };
                if durable {
                    log.record_durable(&entry)
                } else {
                    log.record(
                        entry.conversation,
                        entry.from,
                        entry.wall_clock,
                        &entry.plaintext,
                        entry.event_id,
                    )
                }
            };
            record(&mut log, 1, b"prefix").unwrap();
            let prefix = std::fs::read(&path).unwrap();
            log.fail_next_append_rollback_for_test(7);
            assert!(record(&mut log, 2, b"suffix").is_err());
            assert_eq!(
                std::fs::metadata(&path).unwrap().len(),
                prefix.len() as u64 + 7
            );
            assert_eq!(log.entries(&conv(1)).len(), 1);
            record(&mut log, 2, b"suffix").unwrap();
            record(&mut log, 2, b"suffix").unwrap();
            assert!(
                std::fs::read(&path).unwrap().starts_with(&prefix),
                "recovery must not rewrite the acknowledged encrypted prefix"
            );
            assert_eq!(log.entries(&conv(1)).len(), 2);
            let reopened = ReceivedLog::open(&path, "pw").unwrap();
            assert_eq!(reopened.entries(&conv(1)), log.entries(&conv(1)));
        }
    }

    #[test]
    fn zero_match_erase_repairs_uncommitted_receive_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("received");
        let mut log = ReceivedLog::open(&path, "pw").unwrap();
        log.record(conv(1), "alice".into(), 1, b"prefix", EventId::new([1; 32]))
            .unwrap();
        let prefix = std::fs::read(&path).unwrap();
        log.fail_next_append_rollback_for_test(usize::MAX);
        assert!(
            log.record(
                conv(1),
                "alice".into(),
                2,
                b"uncertain",
                EventId::new([2; 32])
            )
            .is_err()
        );
        assert_eq!(log.remove_where(|_| false).unwrap(), 0);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            prefix,
            "even a no-match erase must first discard an uncertain append"
        );
    }

    #[test]
    fn file_aliases_require_identical_signed_content_and_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("files.log");
        let first = ReceivedEntry {
            event_id: EventId::new([7; 32]),
            conversation: conv(1),
            from: "author".into(),
            wall_clock: 10,
            plaintext: b"manifest".to_vec(),
        };
        let mut alias = first.clone();
        alias.conversation = conv(2);
        let mut files = ReceivedLog::open_file_cards(&path, "pw").unwrap();
        files.record_durable(&first).unwrap();
        files.record_file_alias_durable(&alias).unwrap();
        files.record_file_alias_durable(&alias).unwrap();
        assert_eq!(files.entries(&conv(2)).len(), 1);
        let mut altered = alias.clone();
        altered.plaintext = b"other".to_vec();
        assert!(files.record_file_alias_durable(&altered).is_err());
        drop(files);
        let mut reopened = ReceivedLog::open_file_cards(&path, "pw").unwrap();
        assert_eq!(reopened.entries(&conv(1)), vec![first]);
        assert_eq!(reopened.entries(&conv(2)), vec![alias.clone()]);
        assert!(ReceivedLog::open(&path, "pw").is_err());
        assert_eq!(
            reopened
                .remove_where(|row| row.conversation == conv(1))
                .unwrap(),
            1
        );
        drop(reopened);
        let kept = ReceivedLog::open_file_cards(&path, "pw").unwrap();
        assert!(kept.entries(&conv(1)).is_empty());
        assert_eq!(kept.entries(&conv(2)), vec![alias]);
    }

    #[test]
    fn durable_receive_recovery_rejects_conflicting_event_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recv.log");
        let entry = ReceivedEntry {
            event_id: EventId::new([7; 32]),
            conversation: conv(1),
            from: "a".into(),
            wall_clock: 10,
            plaintext: b"saved".to_vec(),
        };
        let mut log = ReceivedLog::open(&path, "pw").unwrap();
        log.record_durable(&entry).unwrap();
        log.record_durable(&entry).unwrap();
        drop(log);
        let mut log = ReceivedLog::open(&path, "pw").unwrap();
        log.record_durable(&entry).unwrap();
        let mut conflict = entry.clone();
        conflict.plaintext = b"different".to_vec();
        assert!(log.record_durable(&conflict).is_err());
        assert_eq!(log.entries(&conv(1)), vec![entry]);
    }

    #[cfg(unix)]
    #[test]
    fn durable_duplicate_retries_sync_even_if_record_already_exists() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("profile");
        let moved = dir.path().join("moved");
        let mut log = ReceivedLog::open(&parent.join("received"), "pw").unwrap();
        let entry = ReceivedEntry {
            event_id: EventId::new([7; 32]),
            conversation: conv(1),
            from: "a".into(),
            wall_clock: 10,
            plaintext: b"saved".to_vec(),
        };
        log.record_durable(&entry).unwrap();
        std::fs::rename(&parent, &moved).unwrap();
        assert!(
            log.record_durable(&entry).is_err(),
            "an existing record must still cross the durable boundary"
        );
        std::fs::rename(&moved, &parent).unwrap();
        log.record_durable(&entry).unwrap();
        assert_eq!(log.entries(&conv(1)), vec![entry]);
    }

    #[test]
    fn replay_rejects_conflicting_received_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recv.log");
        let entry = ReceivedEntry {
            event_id: EventId::new([7; 32]),
            conversation: conv(1),
            from: "a".into(),
            wall_clock: 10,
            plaintext: b"saved".to_vec(),
        };
        let (mut file, _) = EncryptedRecordLog::<ReceivedEntry>::open(&path, "pw", MAGIC).unwrap();
        file.append_durable(&entry).unwrap();
        file.append_durable(&entry).unwrap();
        drop(file);
        assert_eq!(
            ReceivedLog::open(&path, "pw")
                .unwrap()
                .entries(&conv(1))
                .len(),
            1
        );
        let (mut file, _) = EncryptedRecordLog::<ReceivedEntry>::open(&path, "pw", MAGIC).unwrap();
        let mut conflict = entry;
        conflict.from = "b".into();
        file.append_durable(&conflict).unwrap();
        drop(file);
        assert!(ReceivedLog::open(&path, "pw").is_err());
    }

    #[test]
    fn records_survive_reopen_and_filter_by_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recv.log");
        {
            let mut r = ReceivedLog::open(&path, "pw").unwrap();
            r.record(
                conv(1),
                "alice".into(),
                2000,
                b"second",
                EventId::new([1; 32]),
            )
            .unwrap();
            r.record(
                conv(1),
                "alice".into(),
                1000,
                b"first",
                EventId::new([2; 32]),
            )
            .unwrap();
            r.record(
                conv(2),
                "bob".into(),
                1500,
                b"other-conv",
                EventId::new([3; 32]),
            )
            .unwrap();
        }
        // Reopen from disk: entries are restored, per-conversation, time-ordered.
        let r = ReceivedLog::open(&path, "pw").unwrap();
        let c1 = r.entries(&conv(1));
        assert_eq!(c1.len(), 2);
        assert_eq!(c1[0].plaintext, b"first"); // wall_clock 1000 sorts before 2000
        assert_eq!(c1[1].plaintext, b"second");
        assert_eq!(r.entries(&conv(2)).len(), 1);
        assert!(r.entries(&conv(9)).is_empty());
    }

    #[test]
    fn remove_where_erases_entries_and_rebuilds_seen_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recv.log");
        let drop_id = EventId::new([2; 32]);
        {
            let mut r = ReceivedLog::open(&path, "pw").unwrap();
            r.record(conv(1), "a".into(), 1000, b"keep", EventId::new([1; 32]))
                .unwrap();
            r.record(conv(1), "a".into(), 2000, b"drop", drop_id)
                .unwrap();
            let removed = r.remove_where(|e| e.event_id == drop_id).unwrap();
            assert_eq!(removed, 1);
            assert_eq!(r.entries(&conv(1)).len(), 1);
            // `seen` was rebuilt: the removed id is no longer considered seen, so the SAME
            // event arriving again (a re-sync) WOULD record — but in the node the event-log
            // dedup prevents re-ingest, so this is the intended building block, not a bug.
            r.record(conv(1), "a".into(), 2000, b"drop", drop_id)
                .unwrap();
            assert_eq!(r.entries(&conv(1)).len(), 2);
        }
        // The kept entry survives reopen.
        let r = ReceivedLog::open(&path, "pw").unwrap();
        assert!(r.entries(&conv(1)).iter().any(|e| e.plaintext == b"keep"));
    }

    #[test]
    fn record_is_idempotent_on_event_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recv.log");
        let id = EventId::new([7; 32]);
        {
            let mut r = ReceivedLog::open(&path, "pw").unwrap();
            r.record(conv(1), "alice".into(), 1000, b"hello", id)
                .unwrap();
            r.record(conv(1), "alice".into(), 1000, b"hello", id)
                .unwrap(); // duplicate id
            assert_eq!(
                r.entries(&conv(1)).len(),
                1,
                "a duplicate event id must not be re-recorded"
            );
        }
        // The dedup survives reopen (the seen-set is rebuilt from disk), so a re-imported
        // backfill after restart is still skipped.
        let mut r = ReceivedLog::open(&path, "pw").unwrap();
        r.record(conv(1), "alice".into(), 1000, b"hello", id)
            .unwrap();
        assert_eq!(
            r.entries(&conv(1)).len(),
            1,
            "duplicate skipped after reopen too"
        );
    }
}

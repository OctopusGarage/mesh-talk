//! A local-only, password-encrypted sidecar recording the plaintext of DMs this
//! node RECEIVED. Inbound DMs are decrypted then discarded; this store keeps a
//! private plaintext copy so history can show both sides. It is never synced,
//! never served to peers, and holds no signatures. The on-disk framing is the
//! shared [`EncryptedRecordLog`] (magic + salt header, then length-prefixed
//! AES-256-GCM records); this store layers [`ReceivedEntry`] records, a
//! per-conversation index, and an event-id dedup set on top.

use crate::eventlog::event::{ConversationId, EventId};
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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
    by_conversation: HashMap<ConversationId, Vec<ReceivedEntry>>,
    /// Event ids already stored, so `record` is idempotent — a re-imported backfill (e.g. a
    /// second `link_device`) or any duplicate write is skipped instead of durably doubling
    /// the conversation's history.
    seen: HashSet<EventId>,
}

impl ReceivedLog {
    /// Open (or create) the received log at `path`, replaying stored entries.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<ReceivedEntry>::open(path, password, MAGIC)?;
        let mut by_conversation: HashMap<ConversationId, Vec<ReceivedEntry>> = HashMap::new();
        let mut seen: HashSet<EventId> = HashSet::new();
        for entry in entries {
            seen.insert(entry.event_id);
            by_conversation
                .entry(entry.conversation)
                .or_default()
                .push(entry);
        }
        Ok(Self {
            file,
            by_conversation,
            seen,
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
        // Idempotent on event id: skip a duplicate (e.g. a re-imported account backfill) so it
        // can't durably double the conversation's history.
        if self.seen.contains(&event_id) {
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
        self.seen.insert(event_id);
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(entry);
        Ok(())
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

//! A local-only, password-encrypted sidecar recording the plaintext of DMs this
//! node SENT. Outbound DMs are sealed to the recipient, so the sender cannot
//! decrypt its own messages back from the synced event log; this store keeps a
//! private plaintext copy so history can show both sides. It is never synced,
//! never served to peers, and holds no signatures. The on-disk framing is the
//! shared [`EncryptedRecordLog`] (magic + salt header, then length-prefixed
//! AES-256-GCM records); this store layers [`SentEntry`] records and a
//! per-conversation in-memory index on top.

use crate::eventlog::event::ConversationId;
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTSENT";

/// One sent message's local plaintext record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentEntry {
    pub conversation: ConversationId,
    pub seq: u64,
    pub wall_clock: u64,
    pub plaintext: Vec<u8>,
}

/// An append-only, password-encrypted log of locally-sent message plaintext,
/// indexed in memory by conversation.
pub struct SentLog {
    file: EncryptedRecordLog<SentEntry>,
    by_conversation: HashMap<ConversationId, Vec<SentEntry>>,
}

impl SentLog {
    /// Open (or create) the sent log at `path`, replaying stored entries.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<SentEntry>::open(path, password, MAGIC)?;
        let mut by_conversation: HashMap<ConversationId, Vec<SentEntry>> = HashMap::new();
        for entry in entries {
            by_conversation
                .entry(entry.conversation)
                .or_default()
                .push(entry);
        }
        Ok(Self {
            file,
            by_conversation,
        })
    }

    /// Record a sent message's plaintext: append an encrypted record, then index.
    pub fn record(
        &mut self,
        conversation: ConversationId,
        seq: u64,
        wall_clock: u64,
        plaintext: &[u8],
    ) -> Result<(), LogError> {
        let entry = SentEntry {
            conversation,
            seq,
            wall_clock,
            plaintext: plaintext.to_vec(),
        };
        self.file.append(&entry)?;
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(entry);
        Ok(())
    }

    /// All sent entries for `conversation`, sorted by `(wall_clock, seq)`.
    pub fn entries(&self, conversation: &ConversationId) -> Vec<SentEntry> {
        let mut v = self
            .by_conversation
            .get(conversation)
            .cloned()
            .unwrap_or_default();
        v.sort_by_key(|e| (e.wall_clock, e.seq));
        v
    }

    /// Borrow this conversation's entries WITHOUT cloning (in insertion order — callers
    /// that need them sorted use [`Self::entries`]). For read-only scans (e.g. search)
    /// that would otherwise clone every plaintext.
    pub fn entries_ref(&self, conversation: &ConversationId) -> &[SentEntry] {
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

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn records_survive_reopen_and_filter_by_conversation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let mut s = SentLog::open(&path, "pw").unwrap();
            s.record(conv(1), 1, 2000, b"second").unwrap();
            s.record(conv(1), 0, 1000, b"first").unwrap();
            s.record(conv(2), 1, 1500, b"other-conv").unwrap();
        }
        // Reopen from disk: entries are restored, per-conversation, time-ordered.
        let s = SentLog::open(&path, "pw").unwrap();
        let c1 = s.entries(&conv(1));
        assert_eq!(c1.len(), 2);
        assert_eq!(c1[0].plaintext, b"first"); // wall_clock 1000 sorts before 2000
        assert_eq!(c1[1].plaintext, b"second");
        assert_eq!(s.entries(&conv(2)).len(), 1);
        assert!(s.entries(&conv(9)).is_empty());
    }

    #[test]
    fn header_only_log_reopens_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sent.log");
        {
            let _s = SentLog::open(&path, "pw").unwrap(); // header only, no records
        }
        let s = SentLog::open(&path, "pw").unwrap();
        assert!(s.entries(&conv(1)).is_empty());
    }
}

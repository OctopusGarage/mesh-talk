//! Legacy standalone SDK file inventory and received-file DTO. Production Node
//! uses the profile-owned indexed projection below; this public in-memory helper
//! retains its original standalone record/lookup contract.

use crate::eventlog::event::{ConversationId, EventId};
use crate::file::AnyManifest;
use std::collections::{HashMap, HashSet};

pub(crate) mod indexed;
mod receiving;
pub(crate) mod scopes;
mod work;

/// A received file announcement, surfaced to the application.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    pub protection: Option<super::MessageProtection>,
    /// Absolute authenticated send-time deadline, if selected.
    pub expires_at_ms: Option<u64>,
    /// The DM/channel conversation the manifest arrived in.
    pub conv: ConversationId,
    /// Whether `conv` names a channel rather than a direct conversation.
    pub is_channel: bool,
    /// The sender's user-id fingerprint.
    pub from: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    /// The per-file conversation holding the chunk events (pass to `save_file`).
    pub file_conv: ConversationId,
    /// Whether this is inline MEDIA (media button) vs a generic attachment (attach button).
    /// Decided by the sender's intent (manifest kind), not the file extension.
    pub media: bool,
    pub event_id: EventId,
    pub authored_at: u64,
    pub received_at: u64,
    pub arrival_kind: &'static str,
    pub own_device: bool,
    pub batch_id: Option<String>,
    pub batch_complete: bool,
    pub batch_size: usize,
}

#[derive(Default)]
pub struct FileBook {
    manifests: HashMap<ConversationId, AnyManifest>, // keyed by file_conv
    emitted: HashSet<EventId>,
}

impl FileBook {
    pub fn new() -> Self {
        Self::default()
    }

    /// The manifest for a per-file conversation, if received.
    pub fn manifest(&self, file_conv: &ConversationId) -> Option<&AnyManifest> {
        self.manifests.get(file_conv)
    }

    /// Record an opened manifest; the first file conversation binding remains stable.
    pub fn record(&mut self, manifest: AnyManifest) {
        self.manifests
            .entry(manifest.file_conv())
            .or_insert(manifest);
    }

    /// Whether a FileManifest event id was already surfaced (or seeded on open).
    pub fn is_emitted(&self, id: &EventId) -> bool {
        self.emitted.contains(id)
    }

    /// Mark a FileManifest event id surfaced/seeded.
    pub fn mark_emitted(&mut self, id: EventId) {
        self.emitted.insert(id);
    }

    /// The per-file conversation ids we have manifests for.
    pub fn file_convs(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.manifests.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::ConversationId;

    fn conv(n: u8) -> ConversationId {
        ConversationId::new([n; 32])
    }

    #[test]
    fn records_and_dedups() {
        let mut book = FileBook::new();
        assert!(book.manifest(&conv(1)).is_none());
        let m = crate::file::FileManifest {
            name: "a.txt".into(),
            size: 3,
            mime: "text/plain".into(),
            checksum: [0u8; 32],
            file_key: [1u8; 32],
            file_conv: conv(1),
            chunk_count: 1,
        };
        book.record(AnyManifest::V1(m.clone()));
        assert_eq!(
            book.manifest(&conv(1)).map(|a| a.file_conv()),
            Some(conv(1))
        );

        let id = crate::eventlog::event::EventId::new([9u8; 32]);
        assert!(!book.is_emitted(&id));
        book.mark_emitted(id);
        assert!(book.is_emitted(&id));

        let expected = AnyManifest::V1(m.clone());
        let mut conflicting = m;
        conflicting.file_key = [9; 32];
        book.record(AnyManifest::V1(conflicting));
        assert_eq!(book.manifest(&conv(1)), Some(&expected));
    }
}

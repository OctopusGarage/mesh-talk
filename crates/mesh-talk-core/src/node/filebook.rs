//! In-memory store of received file manifests (keyed by the per-file conversation id)
//! plus an emitted-event set so a manifest is surfaced once. Mirrors `ChannelBook`.
//! Opening a manifest needs the conversation crypto (roster for a DM, the channel
//! group key for a channel), so the `Node` opens it and calls [`FileBook::record`].

use crate::eventlog::event::{ConversationId, EventId};
use crate::file::AnyManifest;
use std::collections::{HashMap, HashSet};

/// A received file announcement, surfaced to the application.
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    /// The DM/channel conversation the manifest arrived in.
    pub conv: ConversationId,
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

    /// Record an opened manifest (idempotent — same file_conv overwrites with the
    /// same manifest).
    pub fn record(&mut self, manifest: AnyManifest) {
        self.manifests.insert(manifest.file_conv(), manifest);
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
    }
}

//! In-memory store of received file manifests (keyed by the per-file conversation id)
//! plus an emitted-event set so a manifest is surfaced once. Mirrors `ChannelBook`.
//! Opening a manifest needs the conversation crypto (roster for a DM, the channel
//! group key for a channel), so the `Node` opens it and calls [`FileBook::record`].

use crate::eventlog::event::{Author, ConversationId, EventId};
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
    manifest_events: HashMap<ConversationId, HashSet<EventId>>,
    authors: HashMap<ConversationId, HashSet<Author>>,
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

    pub(in crate::node) fn accepts(&self, manifest: &AnyManifest) -> bool {
        self.manifests
            .get(&manifest.file_conv())
            .is_none_or(|existing| existing == manifest)
    }

    pub(in crate::node) fn record_event(
        &mut self,
        event: EventId,
        author: Author,
        manifest: AnyManifest,
    ) -> bool {
        if !self.accepts(&manifest) {
            return false;
        }
        self.manifest_events
            .entry(manifest.file_conv())
            .or_default()
            .insert(event);
        self.authors
            .entry(manifest.file_conv())
            .or_default()
            .insert(author);
        self.record(manifest);
        true
    }

    pub(in crate::node) fn manifest_events(&self, conversation: ConversationId) -> Vec<EventId> {
        self.manifest_events
            .get(&conversation)
            .into_iter()
            .flatten()
            .copied()
            .collect()
    }

    pub(in crate::node) fn has_author(&self, file: ConversationId, author: Author) -> bool {
        self.authors
            .get(&file)
            .is_some_and(|authors| authors.contains(&author))
    }

    pub(in crate::node) fn authors(&self, file: ConversationId) -> HashSet<Author> {
        self.authors.get(&file).cloned().unwrap_or_default()
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

        let first = crate::eventlog::event::EventId::new([1; 32]);
        let mut conflicting = m.clone();
        conflicting.file_key = [9; 32];
        let author = Author::from_ed25519([4; 32]);
        assert!(book.record_event(first, author, AnyManifest::V1(m.clone())));
        assert!(book.record_event(
            crate::eventlog::event::EventId::new([2; 32]),
            Author::from_ed25519([5; 32]),
            AnyManifest::V1(m)
        ));
        assert!(!book.record_event(
            crate::eventlog::event::EventId::new([3; 32]),
            author,
            AnyManifest::V1(conflicting)
        ));
        assert_eq!(book.manifest_events(conv(1)).len(), 2);
        assert!(book.has_author(conv(1), author));
        assert!(book.has_author(conv(1), Author::from_ed25519([5; 32])));
    }
}

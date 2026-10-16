//! Bounded, principal-scoped metadata only. Event payloads never enter this cache.
use crate::{eventlog::ConversationId, identity::device::PublicIdentity};
use std::collections::VecDeque;

const MAX_PROJECTIONS: usize = 128;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProjectionKey {
    pub conversation: ConversationId,
    pub raw: ([u8; 32], usize),
    pub peer: PublicIdentity,
    pub own: PublicIdentity,
    pub own_account: String,
    pub established: Option<[u8; 32]>,
    pub privacy_generation: u64,
    pub privacy_configured: bool,
    pub inbound: bool,
    pub sharing: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ProjectionMetadata {
    pub fingerprint: [u8; 32],
    pub len: usize,
    pub requires_v2: bool,
}

#[derive(Default)]
pub(super) struct ChannelProjectionCache {
    entries: VecDeque<(ProjectionKey, ProjectionMetadata)>,
}

impl ChannelProjectionCache {
    pub fn get(&self, key: &ProjectionKey) -> Option<ProjectionMetadata> {
        self.entries
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| *value)
    }

    pub fn insert(&mut self, key: ProjectionKey, value: ProjectionMetadata) {
        // Replace an older revision for the same principal/conversation rather
        // than letting one active conversation consume the entire bound.
        self.entries.retain(|(old, _)| {
            old.conversation != key.conversation
                || old.peer != key.peer
                || old.own != key.own
                || old.inbound != key.inbound
        });
        if self.entries.len() == MAX_PROJECTIONS {
            self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

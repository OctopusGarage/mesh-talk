//! Bounded, principal-scoped metadata only. Event payloads never enter this cache.
use super::privacy_sync::HistorySyncStamp;
use crate::{
    eventlog::{ConversationId, sync::network::SyncMetadata},
    identity::device::PublicIdentity,
};
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
    pub requires_v3: bool,
    pub requires_v4: bool,
    pub requires_v5: bool,
}

enum Entry {
    Legacy(Box<ProjectionKey>, ProjectionMetadata),
    Paged(HistorySyncStamp, SyncMetadata),
}

#[derive(Default)]
pub(super) struct ChannelProjectionCache {
    entries: VecDeque<Entry>,
}

impl ChannelProjectionCache {
    pub fn get(&self, key: &ProjectionKey) -> Option<ProjectionMetadata> {
        self.entries.iter().find_map(|entry| match entry {
            Entry::Legacy(stored, value) if stored.as_ref() == key => Some(*value),
            _ => None,
        })
    }

    pub fn insert(&mut self, key: ProjectionKey, value: ProjectionMetadata) {
        // Replace an older revision for the same principal/conversation rather
        // than letting one active conversation consume the entire bound.
        self.entries.retain(|entry| match entry {
            Entry::Legacy(old, _) => {
                old.conversation != key.conversation
                    || old.peer != key.peer
                    || old.own != key.own
                    || old.inbound != key.inbound
            }
            Entry::Paged(..) => true,
        });
        if self.entries.len() == MAX_PROJECTIONS {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry::Legacy(Box::new(key), value));
    }

    pub fn get_paged(&self, stamp: &HistorySyncStamp) -> Option<SyncMetadata> {
        self.entries.iter().find_map(|entry| match entry {
            Entry::Paged(stored, value) if stored == stamp => Some(*value),
            _ => None,
        })
    }

    /// Return whether admission evicted another bounded metadata entry.
    pub fn insert_paged(&mut self, stamp: HistorySyncStamp, value: SyncMetadata) -> bool {
        if value.fingerprint.is_none() || self.get_paged(&stamp).is_some() {
            return false;
        }
        let evicted = self.entries.len() == MAX_PROJECTIONS;
        if evicted {
            self.entries.pop_front();
        }
        self.entries.push_back(Entry::Paged(stamp, value));
        evicted
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

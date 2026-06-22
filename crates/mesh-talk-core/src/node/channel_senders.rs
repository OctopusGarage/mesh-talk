//! A local-only, password-encrypted store mapping a channel id to this node's
//! serialized per-epoch sending keys (`my_sender`). The on-disk framing is the
//! shared [`EncryptedRecordLog`] (magic + salt header, then length-prefixed
//! AES-256-GCM records). Replay is last-write-wins per channel, so the in-memory
//! index always holds the latest snapshot of our sending chains for each channel.
//!
//! Why this exists: `my_sender` is in-memory only and rebuilt EMPTY on restart (the
//! event log only carries OTHER members' sender-key distributions). Without this
//! store, a node that sends again post-restart generates a NEW sender key and a NEW
//! distribution, but receivers are first-wins on chains they already hold and keep
//! the OLD chain — so they cannot decrypt the post-restart messages. Persisting our
//! OLD chains lets us resume them, which is exactly what receivers can still follow.

use crate::eventlog::event::ConversationId;
use crate::eventlog::LogError;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTCSND";

/// One channel's sending-key snapshot: the channel id (hex) and the serialized
/// per-epoch sending keys (`epoch -> SenderKey::serialize()` bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SenderRecord {
    channel: String,
    senders: Vec<(u64, Vec<u8>)>,
}

/// An append-only, password-encrypted store of this node's channel sending keys, one
/// snapshot per channel. Last write wins: replaying the file rebuilds the latest
/// snapshot for every channel.
pub struct ChannelSenderStore {
    file: EncryptedRecordLog<SenderRecord>,
    latest: HashMap<String, Vec<(u64, Vec<u8>)>>,
}

impl ChannelSenderStore {
    /// Open (or create) the channel-senders store at `path`.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<SenderRecord>::open(path, password, MAGIC)?;
        let mut latest: HashMap<String, Vec<(u64, Vec<u8>)>> = HashMap::new();
        for record in records {
            latest.insert(record.channel, record.senders);
        }
        Ok(Self { file, latest })
    }

    /// The latest stored sending keys for `channel` (empty if none).
    pub fn get(&self, channel: &ConversationId) -> Vec<(u64, Vec<u8>)> {
        self.latest
            .get(&hex::encode(channel.as_bytes()))
            .cloned()
            .unwrap_or_default()
    }

    /// Persist `senders` for `channel` (append a record; update the index).
    pub fn put(
        &mut self,
        channel: &ConversationId,
        senders: &[(u64, Vec<u8>)],
    ) -> Result<(), LogError> {
        let channel_hex = hex::encode(channel.as_bytes());
        let record = SenderRecord {
            channel: channel_hex.clone(),
            senders: senders.to_vec(),
        };
        self.file.append(&record)?;
        self.latest.insert(channel_hex, senders.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::new_channel_id;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
        let ch = new_channel_id();
        assert!(store.get(&ch).is_empty());
        let senders = vec![(0u64, vec![1, 2, 3]), (1u64, vec![4, 5, 6])];
        store.put(&ch, &senders).unwrap();
        assert_eq!(store.get(&ch), senders);
    }

    #[test]
    fn reopen_restores_latest_put_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let ch = new_channel_id();
        {
            let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
            store.put(&ch, &[(0u64, vec![1, 1, 1])]).unwrap();
            // A later put for the same channel must win on reopen.
            store.put(&ch, &[(0u64, vec![9, 9, 9])]).unwrap();
        }
        let store2 = ChannelSenderStore::open(&path, "pw").unwrap();
        assert_eq!(store2.get(&ch), vec![(0u64, vec![9, 9, 9])]);
    }
}

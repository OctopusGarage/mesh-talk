//! Account-owned message snapshots, independent of conversation retention.
use super::node::now_millis;
use super::{HistoryEntry, Node, NodeError};
use crate::eventlog::{LogError, event::ConversationId};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub(super) const MAX_FAVORITES: usize = 4096;
const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;
pub(super) const MAX_FAVORITE_BYTES: usize = 256 * 1024;
pub(super) const MAX_FAVORITE_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteSource {
    pub kind: String,
    pub target: String,
    pub message_ids: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteMetadata {
    pub title: String,
    pub description: String,
    pub notes: BTreeMap<String, String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteFile {
    pub file_conv: String,
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub media: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteMessage {
    pub id: String,
    pub who: String,
    pub sender_name: String,
    pub text: String,
    pub wall_clock: u64,
    pub expires_at_ms: Option<u64>,
    pub sticker: Option<String>,
    pub file: Option<FavoriteFile>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFavorite {
    pub id: String,
    pub source: FavoriteSource,
    pub source_label: String,
    pub metadata: FavoriteMetadata,
    pub messages: Vec<FavoriteMessage>,
    pub saved_at_ms: u64,
    pub revision: u64,
    pub device_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageFavoriteList {
    pub items: Vec<MessageFavorite>,
    pub has_more: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteInspection {
    pub existing: Option<MessageFavorite>,
    pub saved_message_ids: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct FavoriteRecord {
    pub generation: u64,
    pub favorite: MessageFavorite,
    pub removed: bool,
}

fn invalid(message: &str) -> NodeError {
    NodeError::InvalidInput(message.into())
}
fn hex_valid(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn conversation(s: &str) -> Option<ConversationId> {
    if !hex_valid(s, 64) {
        return None;
    }
    let mut bytes = [0; 32];
    for (i, pair) in s.as_bytes().chunks_exact(2).enumerate() {
        bytes[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(ConversationId::new(bytes))
}
fn valid_source(source: &FavoriteSource) -> bool {
    ((source.kind == "account" && hex_valid(&source.target, 32))
        || (source.kind == "channel" && hex_valid(&source.target, 64)))
        && !source.message_ids.is_empty()
        && source.message_ids.len() <= 100
        && source
            .message_ids
            .iter()
            .all(|id| hex_valid(id, 64) && id != &"0".repeat(64))
        && source.message_ids.iter().collect::<BTreeSet<_>>().len() == source.message_ids.len()
}
fn valid_metadata(metadata: &FavoriteMetadata, source: &FavoriteSource) -> bool {
    metadata.title.chars().count() <= 200
        && metadata.description.chars().count() <= 4000
        && metadata.notes.len() <= source.message_ids.len()
        && metadata
            .notes
            .iter()
            .all(|(id, note)| source.message_ids.contains(id) && note.chars().count() <= 1000)
}
impl FavoriteRecord {
    pub(super) fn valid(&self) -> bool {
        let f = &self.favorite;
        self.generation > 0
            && self.generation <= MAX_SAFE_REVISION
            && hex_valid(&f.id, 64)
            && hex_valid(&f.device_id, 32)
            && f.revision > 0
            && f.revision <= MAX_SAFE_REVISION
            && valid_source(&f.source)
            && valid_metadata(&f.metadata, &f.source)
            && f.source_label.len() <= 1024
            && f.messages.len() == f.source.message_ids.len()
            && f.messages.iter().zip(&f.source.message_ids).all(|(m, id)| {
                &m.id == id
                    && m.who.len() <= 128
                    && m.sender_name.len() <= 1024
                    && m.sticker.as_ref().is_none_or(|s| s.len() <= 128)
                    && m.file.as_ref().is_none_or(|file| {
                        hex_valid(&file.file_conv, 64)
                            && file.name.len() <= 4096
                            && file.mime.len() <= 256
                            && file.size <= super::node::MAX_FILE_SIZE
                    })
            })
            && bincode::serialized_size(self).is_ok_and(|n| n <= MAX_FAVORITE_BYTES as u64)
    }
    fn order(&self) -> (u64, bool, u64, &str) {
        (
            self.generation,
            self.removed,
            self.favorite.revision,
            &self.favorite.device_id,
        )
    }
}
pub(super) struct FavoriteStore {
    file: EncryptedRecordLog<FavoriteRecord>,
    pub(super) current: BTreeMap<String, FavoriteRecord>,
}
impl FavoriteStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) =
            EncryptedRecordLog::<FavoriteRecord>::open(path, password, b"MTFAV1")?;
        let mut store = Self {
            file,
            current: BTreeMap::new(),
        };
        for record in records {
            store.validate(&record)?;
            if store
                .current
                .get(&record.favorite.id)
                .is_none_or(|old| old.order() < record.order())
            {
                store.current.insert(record.favorite.id.clone(), record);
            }
        }
        Ok(store)
    }
    fn validate(&self, record: &FavoriteRecord) -> Result<(), LogError> {
        if !record.valid()
            || (!self.current.contains_key(&record.favorite.id)
                && self.current.len() >= MAX_FAVORITES)
        {
            return Err(LogError::CorruptFile(
                "invalid or excessive favorites".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn merge(&mut self, record: FavoriteRecord) -> Result<bool, LogError> {
        self.validate(&record)?;
        if self
            .current
            .get(&record.favorite.id)
            .is_some_and(|old| old.order() >= record.order())
        {
            return Ok(false);
        }
        self.file.append_durable(&record)?;
        self.current.insert(record.favorite.id.clone(), record);
        Ok(true)
    }
    pub(super) fn live_files(&self, now: u64) -> std::collections::HashSet<ConversationId> {
        self.current
            .values()
            .filter(|r| !r.removed)
            .flat_map(|r| &r.favorite.messages)
            .filter(|m| m.expires_at_ms.is_none_or(|end| end > now))
            .filter_map(|m| m.file.as_ref())
            .filter_map(|file| conversation(&file.file_conv))
            .collect()
    }
    pub(super) fn digest(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-favorites-v1");
        for record in self.current.values() {
            hash.update(record.favorite.id.as_bytes());
            hash.update(record.generation.to_be_bytes());
            hash.update([record.removed as u8]);
            hash.update(record.favorite.revision.to_be_bytes());
            hash.update(record.favorite.device_id.as_bytes());
        }
        hash.finalize().into()
    }
    fn next_revision(&self) -> Result<u64, NodeError> {
        self.current
            .values()
            .map(|r| r.favorite.revision)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .map(|r| r.max(now_millis()))
            .filter(|r| *r <= MAX_SAFE_REVISION)
            .ok_or_else(|| invalid("favorite revision exhausted"))
    }
}
pub(super) fn project(mut favorite: MessageFavorite, now: u64) -> MessageFavorite {
    for m in &mut favorite.messages {
        if m.expires_at_ms.is_some_and(|end| end <= now) {
            m.text.clear();
            m.file = None;
            m.sticker = None;
        }
    }
    favorite
}
#[cfg(test)]
#[path = "favorites/capture_tests.rs"]
mod capture_tests;
mod collection;
#[cfg(test)]
#[path = "favorites/store_tests.rs"]
mod tests;

//! Bounded portable Favorites records. Journal structure and merge rules stay here.
use super::*;
use crate::recovery::snapshot::SnapshotError;
use bincode::Options;
use std::collections::HashMap;

pub(crate) const PORTABLE_RECORD_LIMIT: usize = MAX_FAVORITES;
pub(crate) const PORTABLE_FILE_LIMIT: u64 = MAX_FAVORITE_FILE_BYTES;

pub(crate) struct PortableFavorite {
    pub id: String,
    pub removed: bool,
    pub files: Vec<FavoriteFile>,
}

pub(super) fn canonical_id(account_id: &str, source: &FavoriteSource) -> Result<String, NodeError> {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-favorite-v1");
    hash.update(account_id);
    hash.update(bincode::serialize(source).map_err(|_| invalid("invalid favorite"))?);
    Ok(hex::encode(hash.finalize()))
}

fn decode(payload: &[u8]) -> Result<FavoriteRecord, SnapshotError> {
    if payload.len() > MAX_FAVORITE_BYTES {
        return Err(SnapshotError::SizeLimit);
    }
    let record: FavoriteRecord = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FAVORITE_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(payload)
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    if !record.valid() {
        return Err(SnapshotError::InvalidEncoding);
    }
    Ok(record)
}

/// Used only after the snapshot validator has verified account ownership.
pub(in crate::node) fn summarize(payload: &[u8]) -> Result<PortableFavorite, SnapshotError> {
    Ok(summary(decode(payload)?))
}

fn summary(record: FavoriteRecord) -> PortableFavorite {
    PortableFavorite {
        id: record.favorite.id,
        removed: record.removed,
        files: if record.removed {
            Vec::new()
        } else {
            record
                .favorite
                .messages
                .into_iter()
                .filter_map(|m| m.file)
                .collect()
        },
    }
}

fn portable_record(mut record: FavoriteRecord, now: u64) -> FavoriteRecord {
    record.favorite = project(record.favorite, now);
    if record.removed {
        record.favorite.metadata = FavoriteMetadata::default();
        record.favorite.source_label.clear();
        for message in &mut record.favorite.messages {
            message.text.clear();
            message.file = None;
            message.sticker = None;
            message.who.clear();
            message.sender_name.clear();
        }
    }
    record
}

pub(crate) fn validate(
    payload: &[u8],
    account_id: &str,
    at_ms: u64,
) -> Result<PortableFavorite, SnapshotError> {
    let record = decode(payload)?;
    if canonical_id(account_id, &record.favorite.source)
        .map_err(|_| SnapshotError::InvalidEncoding)?
        != record.favorite.id
        || portable_record(record.clone(), at_ms) != record
    {
        return Err(SnapshotError::InvalidEncoding);
    }
    Ok(summary(record))
}

/// One eligibility selection feeds record count, record encoding and file authorization.
/// The caller keeps the store locked while exporting this borrowed projection.
pub(in crate::node) struct PortableFavorites<'a> {
    records: Vec<&'a FavoriteRecord>,
}

impl Node {
    /// Offline bulk export has no participant worker. Prepare only the selected
    /// favorite source channels, one existing bounded owner turn at a time.
    /// The favorite journal lock is released before any projection work.
    pub(in crate::node) fn prepare_favorite_export(&self) -> Result<(), NodeError> {
        let channels: BTreeSet<_> = self
            .favorites
            .lock()
            .expect("favorites mutex")
            .current
            .values()
            .filter(|record| !record.removed && record.favorite.source.kind == "channel")
            .filter_map(|record| conversation(&record.favorite.source.target))
            .map(|channel| *channel.as_bytes())
            .collect();
        // The journal admits at most MAX_FAVORITES records; no event bodies or
        // historical conversation inventory are collected here.
        for channel in channels {
            let channel = ConversationId::new(channel);
            let captured = self
                .log
                .lock()
                .expect("log mutex")
                .repository()?
                .metadata(&channel)?;
            loop {
                let projected = {
                    let log = self.log.lock().expect("log mutex");
                    let repository = log.repository()?;
                    if repository.metadata(&channel)? != captured {
                        return Err(LogError::StaleCursor.into());
                    }
                    self.channel_checkpoints.history_read_ready(channel)?
                        || self
                            .channel_checkpoints
                            .project_step_with_budget(
                                repository,
                                &self.identity,
                                channel,
                                crate::eventlog::sync::network::PreparationBudget::default(),
                            )?
                            .ready
                };
                if projected && self.process_channel_recalls(channel)?.0 {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl FavoriteStore {
    pub(in crate::node) fn portable_projection(
        &self,
        node: &Node,
    ) -> Result<PortableFavorites<'_>, NodeError> {
        let mut records = Vec::new();
        for record in self.current.values() {
            if record.removed || !node.favorite_protection(&record.favorite)? {
                records.push(record);
            }
        }
        Ok(PortableFavorites { records })
    }
}

impl PortableFavorites<'_> {
    pub(in crate::node) fn portable_records<'a>(
        &'a self,
        account_id: &'a str,
        now: u64,
    ) -> impl Iterator<Item = Result<Vec<u8>, SnapshotError>> + 'a {
        self.records.iter().map(move |record| {
            let record = portable_record((*record).clone(), now);
            let payload =
                bincode::serialize(&record).map_err(|_| SnapshotError::InvalidEncoding)?;
            validate(&payload, account_id, now)?;
            Ok(payload)
        })
    }

    pub(in crate::node) fn portable_files(
        &self,
        now: u64,
    ) -> Result<HashMap<ConversationId, FavoriteFile>, SnapshotError> {
        let mut files: HashMap<ConversationId, FavoriteFile> = HashMap::new();
        for file in self
            .records
            .iter()
            .filter(|r| !r.removed)
            .flat_map(|r| &r.favorite.messages)
            .filter(|m| m.expires_at_ms.is_none_or(|end| end > now))
            .filter_map(|m| m.file.as_ref())
        {
            let conv = conversation(&file.file_conv).ok_or(SnapshotError::InvalidEncoding)?;
            if files.get(&conv).is_some_and(|old| old.size != file.size) {
                return Err(SnapshotError::InvalidMedia);
            }
            files.entry(conv).or_insert_with(|| file.clone());
        }
        Ok(files)
    }
}

impl FavoriteStore {
    pub(in crate::node) fn restore_portable(
        &mut self,
        payload: &[u8],
        account_id: &str,
    ) -> Result<(), SnapshotError> {
        let record = decode(payload)?;
        if canonical_id(account_id, &record.favorite.source)
            .map_err(|_| SnapshotError::InvalidEncoding)?
            != record.favorite.id
        {
            return Err(SnapshotError::InvalidEncoding);
        }
        self.merge(record).map_err(|_| SnapshotError::Io)?;
        Ok(())
    }
}

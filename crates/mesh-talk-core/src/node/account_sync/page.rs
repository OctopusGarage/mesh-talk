//! Bounded history-page records, scanning, and inbound frame assembly.

use super::{
    AccountHistoryFrame, BackfillRecord, DmEnvelope, HistoryCursor, MAX_CHUNK, MAX_RECORD,
    NodeError, RECORDS_PER_PAGE, account_conversation_id,
};
#[cfg(test)]
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(super) struct PendingRecord {
    pub(super) kind: u8,
    pub(super) conversation: [u8; 32],
    pub(super) peer_account: Option<String>,
    pub(super) from: String,
    pub(super) wall_clock: u64,
    pub(super) event_id: [u8; 32],
    pub(super) total_len: usize,
    pub(super) plaintext: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct ChannelRecord {
    pub(super) conversation: [u8; 32],
    pub(super) from: String,
    pub(super) wall_clock: u64,
    pub(super) event_id: [u8; 32],
    pub(super) plaintext: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct FileCardRecord {
    pub(super) host: [u8; 32],
    pub(super) peer_account: Option<String>,
    pub(super) from: String,
    pub(super) wall_clock: u64,
    pub(super) event_id: [u8; 32],
    pub(super) payload: Vec<u8>,
}

pub(super) enum HistoryRecord {
    Direct(BackfillRecord),
    Channel(ChannelRecord),
    FileCard(FileCardRecord),
}

#[cfg(test)]
impl HistoryRecord {
    pub(super) fn id(&self) -> [u8; 32] {
        match self {
            Self::Direct(record) => record.event_id,
            Self::Channel(record) => record.event_id,
            Self::FileCard(record) => record.event_id,
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Self::Direct(_) => 0,
            Self::Channel(_) => 1,
            Self::FileCard(_) => 2,
        }
    }

    pub(super) fn cursor(&self) -> HistoryCursor {
        HistoryCursor {
            tag: self.tag(),
            id: self.id(),
        }
    }
}

/// A full-history sweep keeps only one page and a look-ahead record in memory.
/// The order-independent digest lets stores be scanned without sorting all rows.
#[cfg(test)]
pub(super) struct PageBuilder {
    after: Option<HistoryCursor>,
    pub(super) records: Vec<HistoryRecord>,
    digest_xor: [u8; 32],
    count: u64,
}

#[cfg(test)]
impl PageBuilder {
    pub(super) fn new(after: Option<HistoryCursor>) -> Self {
        Self {
            after,
            records: Vec::with_capacity(RECORDS_PER_PAGE + 1),
            digest_xor: [0; 32],
            count: 0,
        }
    }

    pub(super) fn offer(&mut self, key: HistoryCursor, build: impl FnOnce() -> HistoryRecord) {
        let mut hasher = Sha256::new();
        hasher.update(b"mesh-talk-history-record-v2");
        hasher.update([key.tag]);
        hasher.update(key.id);
        let hash: [u8; 32] = hasher.finalize().into();
        for (byte, part) in self.digest_xor.iter_mut().zip(hash) {
            *byte ^= part;
        }
        self.count = self.count.saturating_add(1);
        if self.after.is_some_and(|after| key <= after)
            || self.records.iter().any(|record| record.cursor() == key)
            || (self.records.len() > RECORDS_PER_PAGE
                && self.records.last().is_some_and(|last| key >= last.cursor()))
        {
            return;
        }
        let insert_at = self.records.partition_point(|record| record.cursor() < key);
        self.records.insert(insert_at, build());
        self.records.truncate(RECORDS_PER_PAGE + 1);
    }

    pub(super) fn finish(mut self) -> (Vec<HistoryRecord>, Option<HistoryCursor>, [u8; 32]) {
        let next = if self.records.len() > RECORDS_PER_PAGE {
            self.records.pop();
            self.records.last().map(HistoryRecord::cursor)
        } else {
            None
        };
        let mut hasher = Sha256::new();
        hasher.update(b"mesh-talk-history-digest-v2");
        hasher.update(self.digest_xor);
        hasher.update(self.count.to_be_bytes());
        (self.records, next, hasher.finalize().into())
    }
}

impl PendingRecord {
    pub(super) fn finish(self, own_account_id: &str) -> Option<HistoryRecord> {
        if self.plaintext.len() != self.total_len {
            return None;
        }
        if self.kind == 2 {
            return Some(HistoryRecord::FileCard(FileCardRecord {
                host: self.conversation,
                peer_account: self.peer_account,
                from: self.from,
                wall_clock: self.wall_clock,
                event_id: self.event_id,
                payload: self.plaintext,
            }));
        }
        if self.kind == 1 {
            return Some(HistoryRecord::Channel(ChannelRecord {
                conversation: self.conversation,
                from: self.from,
                wall_clock: self.wall_clock,
                event_id: self.event_id,
                plaintext: self.plaintext,
            }));
        }
        if self.kind != 0 {
            return None;
        }
        let env = DmEnvelope::decode(&self.plaintext)?;
        if env.msg_id != self.event_id || env.route.sender_account != self.from {
            return None;
        }
        let other = if env.route.sender_account == own_account_id {
            &env.route.recipient_account
        } else if env.route.recipient_account == own_account_id {
            &env.route.sender_account
        } else {
            return None;
        };
        if *account_conversation_id(own_account_id, other).as_bytes() != self.conversation {
            return None;
        }
        Some(HistoryRecord::Direct(BackfillRecord {
            conv: self.conversation,
            from: self.from,
            wall_clock: self.wall_clock,
            plaintext: self.plaintext,
            event_id: self.event_id,
        }))
    }
}

pub(super) struct HistoryPageCompletion {
    pub(super) records: Vec<HistoryRecord>,
    pub(super) next_cursor: Option<HistoryCursor>,
    pub(super) digest: [u8; 32],
}

/// Owns the ordering and size limits for one inbound history page.
pub(super) struct HistoryPageAssembler {
    pending: Option<PendingRecord>,
    pub(super) records: Vec<HistoryRecord>,
}

impl HistoryPageAssembler {
    pub(super) fn new() -> Self {
        Self {
            pending: None,
            records: Vec::new(),
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.pending.is_none()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub(super) fn accept(
        &mut self,
        frame: AccountHistoryFrame,
        own_account_id: &str,
    ) -> Result<Option<HistoryPageCompletion>, NodeError> {
        let (kind, conversation, peer_account, from, wall_clock, event_id, total_len, error) =
            match frame {
                AccountHistoryFrame::RecordHeader {
                    conversation,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } => (
                    0,
                    conversation,
                    None,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                    "too many account-history records",
                ),
                AccountHistoryFrame::ChannelHeader {
                    channel,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } => (
                    1,
                    channel,
                    None,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                    "invalid channel-history record",
                ),
                AccountHistoryFrame::FileCardHeader {
                    host,
                    peer_account,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } => (
                    2,
                    host,
                    peer_account,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                    "invalid file-card record length",
                ),
                AccountHistoryFrame::RecordChunk { bytes } => {
                    if bytes.is_empty() || bytes.len() > MAX_CHUNK {
                        return Err(NodeError::Channel("invalid account-history chunk".into()));
                    }
                    let Some(record) = self.pending.as_mut() else {
                        return Err(NodeError::Channel("unexpected history chunk".into()));
                    };
                    if record.plaintext.len().saturating_add(bytes.len()) > record.total_len {
                        return Err(NodeError::Channel("history record overflow".into()));
                    }
                    record.plaintext.extend_from_slice(&bytes);
                    if record.plaintext.len() == record.total_len {
                        let complete = self.pending.take().expect("pending record exists");
                        let record = complete.finish(own_account_id).ok_or_else(|| {
                            NodeError::Channel("invalid account-history record".into())
                        })?;
                        self.records.push(record);
                    }
                    return Ok(None);
                }
                AccountHistoryFrame::Complete {
                    next_cursor,
                    digest,
                } if self.pending.is_none() => {
                    return Ok(Some(HistoryPageCompletion {
                        records: std::mem::take(&mut self.records),
                        next_cursor,
                        digest,
                    }));
                }
                _ => return Err(NodeError::Channel("unexpected history frame".into())),
            };
        if self.pending.is_some() {
            return Err(NodeError::Channel("unexpected history frame".into()));
        }
        if self.records.len() >= RECORDS_PER_PAGE
            || total_len == 0
            || total_len as usize > MAX_RECORD
        {
            return Err(NodeError::Channel(error.into()));
        }
        self.pending = Some(PendingRecord {
            kind,
            conversation,
            peer_account,
            from,
            wall_clock,
            event_id,
            total_len: total_len as usize,
            plaintext: Vec::with_capacity(total_len as usize),
        });
        Ok(None)
    }
}

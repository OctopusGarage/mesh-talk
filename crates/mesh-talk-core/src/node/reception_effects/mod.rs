//! Durable reception work references the existing transcript owner. It never
//! stores another message body or advances the recipient's read state.

use super::received_log::ReceivedEntry;
use crate::eventlog::{
    LogError,
    event::{ConversationId, Event, EventId},
};
use crate::identity::device::PublicIdentity;
use crate::storage::profile::context::ProfileStorage;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod pages;
mod storage;

pub(crate) const MAX_RECEPTION_ROWS: usize = 128;
pub(crate) const MAX_RECEPTION_BODY_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_RECEPTION_SERIALIZED_BYTES: usize = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 2048;
const JSON_ENVELOPE_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ReceptionKind {
    DirectMessage,
    ChannelMessage,
    File,
    ChannelTerminal,
}

impl ReceptionKind {
    fn tag(self) -> i64 {
        match self {
            Self::DirectMessage => 1,
            Self::ChannelMessage => 2,
            Self::File => 3,
            Self::ChannelTerminal => 4,
        }
    }
    fn from_tag(tag: i64) -> Result<Self, LogError> {
        match tag {
            1 => Ok(Self::DirectMessage),
            2 => Ok(Self::ChannelMessage),
            3 => Ok(Self::File),
            4 => Ok(Self::ChannelTerminal),
            _ => Err(invalid("invalid reception kind")),
        }
    }
    fn transcript_kind(self) -> i64 {
        if self == Self::File {
            super::transcript_repository::FILES
        } else {
            super::transcript_repository::RECEIVED
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ArrivalKind {
    Live,
    CatchUp,
    HistorySync,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceptionMetadata {
    pub from_device: String,
    pub received_at: u64,
    pub arrival_kind: ArrivalKind,
    pub own_device: bool,
    pub batch_id: Option<String>,
    pub batch_complete: bool,
    pub batch_size: usize,
    pub reception_only: bool,
}

impl ReceptionMetadata {
    pub(crate) fn for_incoming(
        original: &Event,
        received: &ReceivedEntry,
        owner_device: &PublicIdentity,
        owner_account: &str,
        now: u64,
    ) -> Self {
        Self {
            from_device: original.author.user_id(),
            received_at: now,
            arrival_kind: ArrivalKind::Live,
            own_device: received.from == owner_account
                || original.author.ed25519_pub() == &owner_device.ed25519_pub,
            batch_id: None,
            batch_complete: true,
            batch_size: 1,
            reception_only: false,
        }
    }
    fn validate(&self, kind: ReceptionKind) -> Result<(), LogError> {
        if self.from_device.len() > 200
            || self
                .batch_id
                .as_ref()
                .is_some_and(|id| id.is_empty() || id.len() > 200)
            || self.reception_only != (kind == ReceptionKind::ChannelTerminal)
            || (self.reception_only && (self.batch_id.is_none() || !self.batch_complete))
        {
            return Err(invalid("invalid reception metadata"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReceptionSource {
    pub kind: ReceptionKind,
    pub conversation: ConversationId,
    pub event_id: EventId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceptionCursor {
    pub epoch: [u8; 16],
    pub sequence: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ReceptionLimit {
    pub rows: usize,
    pub body_bytes: usize,
    pub serialized_bytes: usize,
}

impl ReceptionLimit {
    fn validate(self) -> Result<(), LogError> {
        if self.rows == 0
            || self.rows > MAX_RECEPTION_ROWS
            || self.body_bytes == 0
            || self.body_bytes > MAX_RECEPTION_BODY_BYTES
            || self.serialized_bytes == 0
            || self.serialized_bytes > MAX_RECEPTION_SERIALIZED_BYTES
        {
            return Err(LogError::InvalidPageBudget);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ReceptionFact {
    pub sequence: u64,
    pub source: ReceptionSource,
    pub metadata: ReceptionMetadata,
    pub entry: Option<ReceivedEntry>,
}

#[derive(Debug)]
pub(crate) struct ReceptionPage {
    pub rows: Vec<ReceptionFact>,
    pub next: ReceptionCursor,
    pub through: ReceptionCursor,
    pub complete: bool,
    pub body_bytes: usize,
    pub serialized_bound: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ReceptionConsumer {
    Desktop,
    CliOutput,
    CliAutosave,
}

impl ReceptionConsumer {
    fn tag(self) -> i64 {
        match self {
            Self::Desktop => 1,
            Self::CliOutput => 2,
            Self::CliAutosave => 3,
        }
    }
}

pub(crate) struct ReceptionEffects {
    profile: Arc<ProfileStorage>,
    #[cfg(test)]
    body_reads: std::sync::atomic::AtomicUsize,
}

pub(crate) use storage::install_on;

fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn integer(value: u64) -> Result<i64, LogError> {
    i64::try_from(value).map_err(|_| LogError::StaleCursor)
}
fn natural(value: i64) -> Result<u64, LogError> {
    u64::try_from(value).map_err(|_| invalid("negative reception counter"))
}
fn blob<const N: usize>(row: &rusqlite::Row<'_>, index: usize) -> Result<[u8; N], LogError> {
    row.get_ref(index)?
        .as_blob()
        .map_err(|_| invalid("invalid reception identifier"))?
        .try_into()
        .map_err(|_| invalid("invalid reception identifier width"))
}

#[cfg(test)]
mod tests;

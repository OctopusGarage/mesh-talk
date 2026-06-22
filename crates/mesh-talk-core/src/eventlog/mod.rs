//! Append-only, hash-linked, per-conversation event log — the tamper-evident
//! data structure every DM and channel is built from. See
//! `docs/superpowers/specs/2026-06-15-mesh-talk--design.md` §5.
//!
//! Layers: [`event`] (the content-addressed `Event`), [`store`] (the in-memory
//! validating index), [`persist`] (encrypted append-only file + the durable
//! composition), and [`sync`] (the bounded 3-message reconciliation).

pub mod event;
pub mod persist;
pub mod store;
pub mod sync;

pub use event::{Author, ConversationId, Event, EventId, EventKind};
pub use persist::PersistentEventLog;
pub use store::{AppendOutcome, EventLog};
pub use sync::{ApplyReport, SyncFollowup, SyncRequest, SyncResponse, SyncStore};

/// Errors from the event log.
#[derive(Debug)]
pub enum LogError {
    /// `event.id` does not equal the hash of its content (tampered or corrupt).
    CorruptId,
    /// The author's Ed25519 signature over the event id did not verify.
    BadSignature,
    /// `parents` are not in canonical (sorted, de-duplicated) form.
    NonCanonical,
    /// One or more `parents` are not present in the log yet (causally incomplete).
    MissingParents(Vec<EventId>),
    /// The author already has a *different* event at this `seq` (equivocation).
    AuthorEquivocation { author: Author, seq: u64 },
    /// At-rest crypto failure (key derivation / encryption).
    Storage(crate::storage::errors::StorageError),
    /// (De)serialization of an event failed.
    Serialization(String),
    /// I/O failure on the log file.
    Io(std::io::Error),
    /// The on-disk log file is malformed (bad header or undecryptable record).
    CorruptFile(String),
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::CorruptId => write!(f, "event id does not match its content"),
            LogError::BadSignature => write!(f, "event signature did not verify"),
            LogError::NonCanonical => write!(f, "event parents are not in canonical form"),
            LogError::MissingParents(p) => write!(f, "missing {} parent event(s)", p.len()),
            LogError::AuthorEquivocation { seq, .. } => {
                write!(f, "author equivocation at seq {seq}")
            }
            LogError::Storage(e) => write!(f, "storage error: {e}"),
            LogError::Serialization(m) => write!(f, "serialization error: {m}"),
            LogError::Io(e) => write!(f, "io error: {e}"),
            LogError::CorruptFile(m) => write!(f, "corrupt log file: {m}"),
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LogError::Storage(e) => Some(e),
            LogError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<crate::storage::errors::StorageError> for LogError {
    fn from(e: crate::storage::errors::StorageError) -> Self {
        LogError::Storage(e)
    }
}

impl From<std::io::Error> for LogError {
    fn from(e: std::io::Error) -> Self {
        LogError::Io(e)
    }
}

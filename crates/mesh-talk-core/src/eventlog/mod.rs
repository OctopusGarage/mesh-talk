//! Append-only, hash-linked, per-conversation event log — the tamper-evident
//! data structure every DM and channel is built from. See
//! `docs/ARCHITECTURE.md` for the maintained protocol description.
//!
//! Layers: [`event`] (the content-addressed `Event`), [`store`] (the in-memory
//! validating index), [`persist`] (encrypted append-only file + the durable
//! composition), and [`sync`] (the bounded 3-message reconciliation).

pub mod event;
pub(crate) mod order;
pub mod persist;
#[allow(dead_code)] // Indexed backend is wired only after bounded consumers and migration exist.
pub(crate) mod repository;
pub mod store;
pub mod sync;

pub use event::{Author, ConversationId, Event, EventId, EventKind};
pub use persist::PersistentEventLog;
pub use store::{AppendOutcome, EventLog};
pub use sync::{ApplyReport, SyncFollowup, SyncRequest, SyncResponse, SyncStore};

/// Errors from the event log.
#[derive(Debug)]
pub enum LogError {
    /// Explicit admission-policy rejection of an incoming event.
    AdmissionRejected(String),
    /// `event.id` does not equal the hash of its content (tampered or corrupt).
    CorruptId,
    /// The author's Ed25519 signature over the event id did not verify.
    BadSignature,
    /// `parents` are not in canonical (sorted, de-duplicated) form.
    NonCanonical,
    /// No representable timestamp remains for a new local event.
    ClockExhausted,
    /// No representable sequence remains for this local author.
    SequenceExhausted,
    /// Cursor belongs to another store, query, or mutation revision.
    StaleCursor,
    /// The local scope is logically retired while physical reclamation continues.
    ConversationRetiring,
    /// Page limits cannot make progress.
    InvalidPageBudget,
    /// The first selected row exceeds the requested allocation budget.
    PageBudgetTooSmall { required: usize },
    /// An internal persisted revision or commit ordinal cannot advance.
    RepositoryCounterExhausted,
    /// The shared event schema cannot be adopted by this implementation.
    IncompatibleEventSchema,
    /// One or more `parents` are not present in the log yet (causally incomplete).
    MissingParents(Vec<EventId>),
    /// The author already has a *different* event at this `seq` (equivocation).
    AuthorEquivocation { author: Author, seq: u64 },
    /// At-rest crypto failure (key derivation / encryption).
    Storage(crate::storage::errors::StorageError),
    /// Indexed encrypted database failure. SQL/key text is excluded from driver errors.
    Database(Box<dyn std::error::Error + Send + Sync>),
    /// (De)serialization of an event failed.
    Serialization(String),
    /// I/O failure on the log file.
    Io(std::io::Error),
    /// The on-disk log file is malformed (bad header or undecryptable record).
    CorruptFile(String),
}

impl LogError {
    /// Incoming validation/policy failures may be reported without aborting a batch.
    /// Local storage and resource errors must terminate reconciliation.
    pub(crate) fn is_peer_rejection(&self) -> bool {
        matches!(
            self,
            Self::AdmissionRejected(_)
                | Self::NonCanonical
                | Self::CorruptId
                | Self::BadSignature
                | Self::MissingParents(_)
                | Self::AuthorEquivocation { .. }
        )
    }
}

impl std::fmt::Display for LogError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogError::AdmissionRejected(reason) => write!(f, "event admission rejected: {reason}"),
            LogError::CorruptId => write!(f, "event id does not match its content"),
            LogError::BadSignature => write!(f, "event signature did not verify"),
            LogError::NonCanonical => write!(f, "event parents are not in canonical form"),
            LogError::ClockExhausted => write!(f, "event clock exhausted"),
            LogError::SequenceExhausted => write!(f, "event author sequence exhausted"),
            LogError::ConversationRetiring => write!(f, "conversation retirement is in progress"),
            LogError::StaleCursor => write!(f, "stale event page cursor"),
            LogError::InvalidPageBudget => write!(f, "invalid event page budget"),
            LogError::PageBudgetTooSmall { required } => {
                write!(f, "event page requires {required} bytes")
            }
            LogError::RepositoryCounterExhausted => write!(f, "event repository counter exhausted"),
            LogError::IncompatibleEventSchema => write!(f, "incompatible event repository schema"),
            LogError::MissingParents(p) => write!(f, "missing {} parent event(s)", p.len()),
            LogError::AuthorEquivocation { seq, .. } => {
                write!(f, "author equivocation at seq {seq}")
            }
            LogError::Storage(e) => write!(f, "storage error: {e}"),
            LogError::Database(e) => write!(f, "database error: {e}"),
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
            LogError::Database(e) => Some(e.as_ref()),
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

impl From<rusqlite::Error> for LogError {
    fn from(error: rusqlite::Error) -> Self {
        // SQLite input errors can include the complete SQL statement. In
        // particular, a failing key pragma must never expose its passphrase.
        let sanitized = match error {
            rusqlite::Error::SqlInputError { error, .. }
            | rusqlite::Error::SqliteFailure(error, _) => {
                rusqlite::Error::SqliteFailure(error, None)
            }
            other => other,
        };
        Self::Database(Box::new(sanitized))
    }
}

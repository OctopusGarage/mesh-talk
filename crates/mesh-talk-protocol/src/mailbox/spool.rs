//! Durable, bounded mailbox spool for hosts without an indexed relay store.
//! The spool owns its exclusive path lease. The host owns authenticated
//! admission, the clock, and when to invoke expiry.

use crate::legacy_event::{ConversationId, Event, EventId};
use crate::mailbox::{
    capacity::{Admission, CapacityLimits},
    delta::{DeltaError, DeltaSelection},
    envelope_shape,
    rpc::{MailboxCursor, MailboxDeltaPage, PAGE_IDS},
    service::MailboxStore,
};
use mesh_storage::{
    EncryptionKey, SALT_SIZE, StorageError,
    record_log::{EncryptedRecordLog, RecordLogError, ReplacementDurability},
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    num::NonZeroU64,
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 6] = b"MTMSP1";

#[derive(Debug)]
pub enum SpoolError {
    Storage(RecordLogError),
    InvalidRecord,
    StaleCursor,
    Limit,
    CounterExhausted,
}

impl From<RecordLogError> for SpoolError {
    fn from(error: RecordLogError) -> Self {
        Self::Storage(error)
    }
}

impl From<DeltaError> for SpoolError {
    fn from(error: DeltaError) -> Self {
        match error {
            DeltaError::InvalidLimit => Self::Limit,
            DeltaError::InvalidOrdinal => Self::StaleCursor,
            DeltaError::OrdinalExhausted => Self::CounterExhausted,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Deposit {
    Stored,
    AlreadyStored,
    CapacityUnavailable,
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpoolUsage {
    pub envelopes: usize,
    pub encoded_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExpiryReport {
    pub removed: SpoolUsage,
    pub durability: ReplacementDurability,
}

#[derive(Clone, Serialize, Deserialize)]
enum Record {
    Header {
        scope: ConversationId,
        store_uuid: [u8; 16],
        epoch: i64,
    },
    Envelope {
        first_seen: u64,
        event: Event,
    },
}

struct Entry {
    event: Event,
    wire_bytes: u64,
}

pub struct Spool {
    log: EncryptedRecordLog<Record>,
    cursor: MailboxCursor,
    entries: Vec<Entry>,
    positions: HashMap<EventId, usize>,
    bytes: u64,
    limits: CapacityLimits,
    max_age_secs: NonZeroU64,
    _lease: File,
}

fn encoded_size(event: &Event) -> Result<u64, SpoolError> {
    bincode::serialized_size(event).map_err(|_| SpoolError::InvalidRecord)
}

fn valid_envelope(scope: ConversationId, event: &Event) -> bool {
    event.conversation_id == scope
        && envelope_shape(event)
        && event.verify_integrity()
        && event.verify_signature()
}

impl Spool {
    /// Acquire the exclusive lease before replaying records. Keep the persistent
    /// `.mesh-spool-lock` sidecar in place and use a single path for the spool.
    /// The count and wire-byte envelope
    /// limits also bound replay; a file exceeding the configured limits fails
    /// explicitly and remains available for offline recovery.
    pub fn open(
        path: &Path,
        password: &str,
        scope: ConversationId,
        limits: CapacityLimits,
        max_age_secs: NonZeroU64,
    ) -> Result<Self, SpoolError> {
        Self::open_with_key_provider(path, scope, limits, max_age_secs, |salt| {
            EncryptionKey::from_password(password, salt)
        })
    }

    /// Open with a caller-supplied key resolver. The spool still owns its
    /// exclusive writer lease, record-size ceiling, and durable cursor.
    pub fn open_with_key_provider(
        path: &Path,
        scope: ConversationId,
        limits: CapacityLimits,
        max_age_secs: NonZeroU64,
        derive: impl FnOnce(&[u8; SALT_SIZE]) -> Result<EncryptionKey, StorageError>,
    ) -> Result<Self, SpoolError> {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(".mesh-spool-lock");
        let lock_path = PathBuf::from(sidecar);
        if let Some(parent) = lock_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(RecordLogError::from)?;
        }
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(RecordLogError::from)?;
        lease
            .try_lock()
            .map_err(|error| RecordLogError::Io(error.into()))?;

        let mut loaded = None;
        let mut entries = Vec::new();
        let mut positions = HashMap::new();
        let mut bytes = 0u64;
        let mut first_seen = None;
        let mut rejected = None;
        let log = EncryptedRecordLog::open_with_key_and_frame_limit(
            path,
            MAGIC,
            derive,
            128 * 1024,
            |record| {
                let error = match record {
                    Record::Header {
                        scope: actual,
                        store_uuid,
                        epoch,
                    } if loaded.is_none() && actual == scope && epoch >= 0 => {
                        loaded = Some((store_uuid, epoch));
                        return Ok(());
                    }
                    Record::Envelope {
                        first_seen: time,
                        event,
                    } if loaded.is_some()
                        && first_seen.is_none_or(|old| old == time)
                        && !positions.contains_key(&event.id)
                        && valid_envelope(scope, &event) =>
                    {
                        match encoded_size(&event).and_then(|size| {
                            let admission = limits
                                .admit(entries.len() as u64, bytes, size)
                                .map_err(|_| SpoolError::CounterExhausted)?;
                            if admission != Admission::Accept {
                                return Err(SpoolError::Limit);
                            }
                            bytes = bytes
                                .checked_add(size)
                                .ok_or(SpoolError::CounterExhausted)?;
                            Ok(size)
                        }) {
                            Ok(wire_bytes) => {
                                first_seen = Some(time);
                                positions.insert(event.id, entries.len());
                                entries.push(Entry { event, wire_bytes });
                                return Ok(());
                            }
                            Err(error) => error,
                        }
                    }
                    _ => SpoolError::InvalidRecord,
                };
                rejected = Some(error);
                Err(RecordLogError::Io(std::io::Error::other(
                    "spool replay rejected",
                )))
            },
        );
        if let Some(error) = rejected {
            return Err(error);
        }
        let mut log = log?;
        let (store_uuid, epoch) = if let Some(existing) = loaded {
            existing
        } else {
            let mut uuid = [0; 16];
            OsRng.fill_bytes(&mut uuid);
            log.append_durable(&Record::Header {
                scope,
                store_uuid: uuid,
                epoch: 0,
            })?;
            (uuid, 0)
        };
        let after = i64::try_from(entries.len()).map_err(|_| SpoolError::CounterExhausted)?;
        Ok(Self {
            log,
            cursor: MailboxCursor {
                store_uuid,
                conversation: scope,
                epoch,
                first_seen,
                revision: after,
                after,
                window: None,
            },
            entries,
            positions,
            bytes,
            limits,
            max_age_secs,
            _lease: lease,
        })
    }

    fn expired(&self, now: u64) -> bool {
        self.cursor
            .first_seen
            .is_some_and(|first| first.saturating_add(self.max_age_secs.get()) <= now)
    }

    /// Advance the destructive generation only after the replacement commits.
    pub fn expire_at(&mut self, now: u64) -> Result<bool, SpoolError> {
        self.expire_report(now).map(|report| report.is_some())
    }

    /// Retained occupancy, including entries awaiting explicit expiry.
    pub fn usage(&self) -> SpoolUsage {
        SpoolUsage {
            envelopes: self.entries.len(),
            encoded_bytes: self.bytes,
        }
    }

    /// Apply expiry and report the removed data and replacement durability.
    pub fn expire_report(&mut self, now: u64) -> Result<Option<ExpiryReport>, SpoolError> {
        if !self.expired(now) {
            return Ok(None);
        }
        let epoch = self
            .cursor
            .epoch
            .checked_add(1)
            .ok_or(SpoolError::CounterExhausted)?;
        let removed = self.usage();
        let durability = self.log.rewrite_report(&[Record::Header {
            scope: self.cursor.conversation,
            store_uuid: self.cursor.store_uuid,
            epoch,
        }])?;
        self.entries.clear();
        self.positions.clear();
        self.bytes = 0;
        self.cursor.epoch = epoch;
        self.cursor.first_seen = None;
        self.cursor.revision = 0;
        self.cursor.after = 0;
        Ok(Some(ExpiryReport {
            removed,
            durability,
        }))
    }

    /// The host authenticates the depositor before calling this method.
    pub fn deposit(&mut self, event: Event, now: u64) -> Result<Deposit, SpoolError> {
        self.deposit_observed(event, now, |_| {})
    }

    /// Report any committed expiry inline, even when the subsequent deposit
    /// fails. The callback must be fast and nonblocking.
    pub fn deposit_observed(
        &mut self,
        event: Event,
        now: u64,
        on_expiry: impl FnOnce(ExpiryReport),
    ) -> Result<Deposit, SpoolError> {
        // Only byte-for-byte equivalent, previously authenticated entries can
        // skip verification. A matching ID alone cannot authenticate a retry.
        if !self.expired(now)
            && let Some(&position) = self.positions.get(&event.id)
            && self.entries[position].event == event
        {
            return Ok(Deposit::AlreadyStored);
        }
        if !valid_envelope(self.cursor.conversation, &event) {
            return Err(SpoolError::InvalidRecord);
        }
        if let Some(report) = self.expire_report(now)? {
            on_expiry(report);
        }
        if self.positions.contains_key(&event.id) {
            return Ok(Deposit::AlreadyStored);
        }
        let size = encoded_size(&event)?;
        match self
            .limits
            .admit(self.entries.len() as u64, self.bytes, size)
            .map_err(|_| SpoolError::CounterExhausted)?
        {
            Admission::Impossible => return Ok(Deposit::TooLarge),
            Admission::Backpressure => return Ok(Deposit::CapacityUnavailable),
            Admission::Accept => {}
        }
        let next = self
            .cursor
            .after
            .checked_add(1)
            .ok_or(SpoolError::CounterExhausted)?;
        let first_seen = self.cursor.first_seen.unwrap_or(now);
        self.log.append_durable(&Record::Envelope {
            first_seen,
            event: event.clone(),
        })?;
        self.positions.insert(event.id, self.entries.len());
        self.entries.push(Entry {
            event,
            wire_bytes: size,
        });
        self.bytes += size;
        self.cursor.first_seen = Some(first_seen);
        self.cursor.revision = next;
        self.cursor.after = next;
        Ok(Deposit::Stored)
    }

    pub fn durable_have(&self, id: &EventId, now: u64) -> bool {
        !self.expired(now) && self.positions.contains_key(id)
    }

    /// Capture a clock observation for the synchronous request. The host keeps
    /// ownership exclusive until the reply is encoded.
    pub fn view(&self, now: u64) -> SpoolView<'_> {
        SpoolView { spool: self, now }
    }
}

pub struct SpoolView<'a> {
    spool: &'a Spool,
    now: u64,
}

impl MailboxStore for SpoolView<'_> {
    type Error = SpoolError;

    fn delta(
        &self,
        scope: &ConversationId,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<MailboxDeltaPage, SpoolError> {
        if *scope != self.spool.cursor.conversation || self.spool.expired(self.now) {
            return Err(SpoolError::StaleCursor);
        }
        let mut scan = match DeltaSelection::open(&self.spool.cursor, prior, limit)? {
            DeltaSelection::Unchanged(page) => return Ok(page),
            DeltaSelection::Scan(scan) => scan,
        };
        let lower = usize::try_from(scan.lower()).map_err(|_| SpoolError::StaleCursor)?;
        let upper = usize::try_from(scan.upper()).map_err(|_| SpoolError::StaleCursor)?;
        let window = self
            .spool
            .entries
            .get(lower..upper)
            .ok_or(SpoolError::StaleCursor)?;
        for (offset, entry) in window.iter().enumerate().take(limit + 1) {
            if !scan.accept_row() {
                break;
            }
            scan.push((lower + offset) as i64, entry.event.id)?;
        }
        Ok(scan.finish())
    }

    fn bodies(
        &self,
        scope: &ConversationId,
        cursor: &MailboxCursor,
        ids: &[EventId],
        max_bytes: usize,
    ) -> Result<Vec<Event>, SpoolError> {
        if *scope != self.spool.cursor.conversation
            || !cursor.accepts(&self.spool.cursor)
            || self.spool.expired(self.now)
        {
            return Err(SpoolError::StaleCursor);
        }
        if ids.len() > PAGE_IDS || max_bytes == 0 {
            return Err(SpoolError::Limit);
        }
        let mut bytes = 0u64;
        let mut selected = Vec::with_capacity(ids.len());
        for id in ids {
            let position = self
                .spool
                .positions
                .get(id)
                .copied()
                .filter(|&position| (position as i64) < cursor.after)
                .ok_or(SpoolError::StaleCursor)?;
            let entry = &self.spool.entries[position];
            bytes = bytes
                .checked_add(entry.wire_bytes)
                .ok_or(SpoolError::CounterExhausted)?;
            if bytes > max_bytes as u64 {
                return Err(SpoolError::Limit);
            }
            selected.push(&entry.event);
        }
        Ok(selected.into_iter().cloned().collect())
    }
}

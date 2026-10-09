//! Encrypted delivery transaction journal and immutable outbox metadata.

use super::dm_envelope::DmEnvelope;
use super::dm_ratchet::{DmRatchet, PreparedRatchet};
use super::received_log::{ReceivedEntry, ReceivedLog};
use super::sentlog::{SentEntry, SentLog};
use crate::eventlog::event::{ConversationId, Event, EventId, EventKind};
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::LogError;
use crate::identity::device::PublicIdentity;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

const JOURNAL_MAGIC: &[u8; 6] = b"MTDTX1";
const OUTBOX_MAGIC: &[u8; 6] = b"MTDOB1";
const HEADER_BYTES: u64 = 22;
const FRAME_OVERHEAD: u64 = 32; // length + nonce + AES-GCM tag

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus {
    Awaiting,
    Delivered,
}

/// Ciphertext plus the immutable, full destination binding. A legacy public SDK
/// recipient may have no account proof; such a destination cannot issue receipts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeliveryDestination {
    pub(crate) device: PublicIdentity,
    pub(crate) account: Option<String>,
    pub(crate) event: Event,
    pub(crate) receipt_eligible: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OutgoingDelivery {
    pub(crate) logical_id: EventId,
    pub(crate) sender_account: String,
    pub(crate) recipient_account: Option<String>,
    pub(crate) conversation: ConversationId,
    pub(crate) wall_clock: u64,
    pub(crate) destinations: Vec<DeliveryDestination>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceiptDelivery {
    pub(crate) logical_id: EventId,
    pub(crate) original_event_id: EventId,
    pub(crate) conversation: ConversationId,
    pub(crate) wall_clock: u64,
    pub(crate) destination: DeliveryDestination,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ControlBinding {
    pub device: PublicIdentity,
    pub account: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CompletedDelivery {
    pub(crate) logical_id: EventId,
    pub(crate) conversation: ConversationId,
    pub(crate) wall_clock: u64,
    pub(crate) recipient_account: Option<String>,
    pub(crate) remaining: Vec<DeliveryReference>,
}

/// Only unfinished transport work survives delivery confirmation. Ciphertext
/// remains in the immutable event log, not duplicated in completed statuses.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeliveryReference {
    pub(crate) device: PublicIdentity,
    pub(crate) account: Option<String>,
    pub(crate) event_id: EventId,
    pub(crate) receipt_eligible: bool,
}

impl From<&OutgoingDelivery> for CompletedDelivery {
    fn from(message: &OutgoingDelivery) -> Self {
        Self {
            logical_id: message.logical_id,
            conversation: message.conversation,
            wall_clock: message.wall_clock,
            recipient_account: message.recipient_account.clone(),
            remaining: message
                .destinations
                .iter()
                .map(|d| DeliveryReference {
                    device: d.device.clone(),
                    account: d.account.clone(),
                    event_id: d.event.id,
                    receipt_eligible: d.receipt_eligible,
                })
                .collect(),
        }
    }
}

/// Bounded immutable chunk work and retained fanout scopes, without file keys.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileCard {
    pub(crate) id: EventId,
    pub(crate) conversation: ConversationId,
    pub(crate) wall_clock: u64,
    pub(crate) file_conversation: ConversationId,
    pub(crate) final_chunk: Option<EventId>,
    pub(crate) chunk_count: u32,
    pub(crate) destinations: Vec<FileDestination>,
    pub(crate) completion_binding: Option<FileCompletionBinding>,
}

impl FileCard {
    pub(crate) fn incoming(
        original: &Event,
        received: &ReceivedEntry,
        manifest: &crate::file::AnyManifest,
        binding: FileCompletionBinding,
    ) -> Self {
        Self {
            id: original.id,
            conversation: received.conversation,
            wall_clock: received.wall_clock,
            file_conversation: manifest.file_conv(),
            chunk_count: manifest.chunk_count(),
            final_chunk: None,
            destinations: vec![],
            completion_binding: Some(binding),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileCompletionBinding {
    pub(crate) source: PublicIdentity,
    pub(crate) certificate: Option<crate::identity::account::DeviceCertificate>,
    pub(crate) owner_account: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileDestination {
    pub(crate) binding: DeliveryReference,
    pub(crate) active: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CompletedReceipt {
    logical_id: EventId,
    original_event_id: EventId,
    receipt_event_id: EventId,
    conversation: ConversationId,
    wall_clock: u64,
    destination_account: Option<String>,
}

impl From<&ReceiptDelivery> for CompletedReceipt {
    fn from(receipt: &ReceiptDelivery) -> Self {
        Self {
            logical_id: receipt.logical_id,
            original_event_id: receipt.original_event_id,
            receipt_event_id: receipt.destination.event.id,
            conversation: receipt.conversation,
            wall_clock: receipt.wall_clock,
            destination_account: receipt.destination.account.clone(),
        }
    }
}

/// Contains plaintext and previous/next ratchet secrets. Deliberately no Debug or
/// Clone. Kept only in the small encrypted journal until every sidecar is synced.
#[derive(Serialize, Deserialize)]
pub(crate) enum DeliveryTransaction {
    Outgoing {
        message: OutgoingDelivery,
        sent: SentEntry,
        ratchets: Vec<PreparedRatchet>,
    },
    Incoming {
        sender: PublicIdentity,
        original: Box<Event>,
        ratchet: PreparedRatchet,
        received: Box<ReceivedEntry>,
        receipt: Option<Box<ReceiptDelivery>>,
    },
    OutgoingManifest {
        message: OutgoingDelivery,
        received: Box<ReceivedEntry>,
        file: FileCard,
    },
    IncomingManifest {
        sender: PublicIdentity,
        original: Box<Event>,
        received: Box<ReceivedEntry>,
        receipt: Option<Box<ReceiptDelivery>>,
        file: FileCard,
    },
}

impl DeliveryTransaction {
    pub(crate) fn id(&self) -> EventId {
        match self {
            Self::Outgoing { message, .. } | Self::OutgoingManifest { message, .. } => {
                message.logical_id
            }
            Self::Incoming { original, .. } | Self::IncomingManifest { original, .. } => {
                original.id
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct DeliveryLimits {
    pub(crate) messages: usize,
    pub(crate) receipts: usize,
    pub(crate) completed: usize,
    pub(crate) completed_receipts: usize,
    pub(crate) transactions: usize,
    pub(crate) destinations: usize,
    pub(crate) journal_bytes: u64,
    pub(crate) outbox_bytes: u64,
    pub(crate) payload_bytes: usize,
}

impl Default for DeliveryLimits {
    fn default() -> Self {
        Self {
            messages: 10_000,
            receipts: 10_000,
            completed: 100_000,
            completed_receipts: 100_000,
            transactions: 32,
            destinations: 64,
            journal_bytes: 8 * 1024 * 1024,
            outbox_bytes: 64 * 1024 * 1024,
            payload_bytes: 256 * 1024,
        }
    }
}

impl DeliveryLimits {
    fn max_outbox_records(&self) -> usize {
        self.messages
            .saturating_mul(3)
            .saturating_add(self.receipts.saturating_mul(3))
            .saturating_add(self.completed.saturating_mul(2))
            .saturating_add(self.completed_receipts.saturating_mul(2))
            .saturating_add(self.completed_receipts.saturating_mul(3))
            .saturating_add(
                self.messages
                    .saturating_mul(self.destinations)
                    .saturating_mul(2),
            )
            .max(1)
    }
}

#[derive(Serialize, Deserialize)]
enum OutboxRecord {
    Message(OutgoingDelivery),
    Receipt(Box<ReceiptDelivery>),
    Delivered(CompletedDelivery),
    FinishedReceipt(CompletedReceipt),
    Cancel(ConversationId, EventId),
    RetireDestination(EventId, EventId),
    FinishedBoundReceipt(CompletedReceipt, PublicIdentity),
    FileCard(FileCard),
    RetireFileDestination(EventId, EventId),
    ErasedFile(ConversationId, EventId),
    FileComplete(EventId, EventId),
}

/// One owner per profile, protected by the Node's delivery transaction guard.
/// The append-only outbox contains no plaintext or historical ratchet keys.
/// Completing a transaction rewrites only the small secret journal, not the
/// potentially large outbox. The outbox compacts only at its byte/record bound.
pub(crate) struct DeliveryStore {
    profile: Option<(EncryptedRecordLog<DeliveryProfile>, DeliveryProfile)>,
    journal: EncryptedRecordLog<DeliveryTransaction>,
    outbox: EncryptedRecordLog<OutboxRecord>,
    journal_path: PathBuf,
    outbox_path: PathBuf,
    pending: Vec<DeliveryTransaction>,
    messages: BTreeMap<EventId, OutgoingDelivery>,
    completed: BTreeMap<EventId, CompletedDelivery>,
    active_work: BTreeSet<EventId>,
    work_events: BTreeMap<EventId, EventId>,
    destination_cursors: BTreeMap<EventId, EventId>,
    completed_receipts: BTreeMap<EventId, CompletedReceipt>,
    completed_receipt_devices: BTreeMap<EventId, PublicIdentity>,
    receipts: BTreeMap<EventId, ReceiptDelivery>,
    receipt_events: BTreeMap<EventId, EventId>,
    receipt_scopes: BTreeMap<([u8; 32], EventId), BTreeSet<EventId>>,
    files: BTreeMap<EventId, FileCard>,
    active_files: BTreeSet<(EventId, EventId)>,
    file_completions: BTreeMap<EventId, EventId>,
    completion_index: BTreeMap<([u8; 32], EventId), BTreeSet<EventId>>,
    file_conversations: BTreeMap<[u8; 32], BTreeSet<EventId>>,
    erased_files: BTreeSet<([u8; 32], EventId)>,
    erased_manifest_events: BTreeSet<EventId>,
    limits: DeliveryLimits,
    outbox_records: usize,
    needs_sync: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DeliveryProfile {
    device: PublicIdentity,
    account: String,
    adoption: Option<String>,
}

impl DeliveryStore {
    pub(crate) fn bind_profile(
        &mut self,
        log_path: &Path,
        password: &str,
        own: &PublicIdentity,
        account: &str,
    ) -> Result<(), LogError> {
        let mut path = log_path.as_os_str().to_os_string();
        path.push(".delivery-owner");
        let accepted = !self.pending.is_empty()
            || !self.cancellation_rows().is_empty()
            || !self.files.is_empty()
            || !self.erased_files.is_empty();
        if !Path::new(&path).try_exists()? && accepted {
            return Err(invalid("missing delivery profile binding"));
        }
        preflight(Path::new(&path), 4096, 1)?;
        let (mut file, entries) =
            EncryptedRecordLog::<DeliveryProfile>::open(Path::new(&path), password, b"MTDOWN")?;
        let expected = DeliveryProfile {
            device: own.clone(),
            account: account.to_owned(),
            adoption: None,
        };
        if entries.is_empty() && accepted {
            return Err(invalid("missing delivery profile binding"));
        }
        if entries.iter().any(|p| {
            !valid_account(&p.account) || p.adoption.as_deref().is_some_and(|a| !valid_account(a))
        }) {
            return Err(invalid("invalid delivery profile binding"));
        }
        if entries.is_empty() {
            file.append_durable(&expected)?;
        } else if entries.len() != 1 || entries[0].device != *own {
            return Err(invalid("delivery store belongs to another profile"));
        } else if entries[0] != expected {
            // A trusted local account adoption is supported for the same full
            // device only after secret intents are resolved. Old account work
            // loses network/status eligibility; accepted history is retained.
            if entries[0].account != account {
                if !self.pending.is_empty() {
                    return Err(invalid("recover old account delivery before adoption"));
                }
                if entries[0].adoption.as_deref() != Some(account)
                    && !self.cancellation_rows().is_empty()
                {
                    return Err(invalid("prepare old account delivery before adoption"));
                }
                for (conversation, id, _) in self.cancellation_rows() {
                    self.cancel(conversation, id)?;
                }
            }
            file.rewrite(std::slice::from_ref(&expected))?;
        }
        file.sync()?;
        self.profile = Some((file, expected));
        Ok(())
    }

    pub(crate) fn prepare_profile_adoption(&mut self, account: &str) -> Result<(), LogError> {
        if !valid_account(account) {
            return Err(invalid("invalid delivery adoption account"));
        }
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before account adoption"));
        }
        let (file, profile) = self
            .profile
            .as_mut()
            .ok_or_else(|| invalid("delivery owner is unbound"))?;
        let mut prepared = profile.clone();
        prepared.adoption = Some(account.to_owned());
        file.rewrite(std::slice::from_ref(&prepared))?;
        file.sync()?;
        *profile = prepared;
        Ok(())
    }

    pub(crate) fn cancel_profile_adoption(&mut self) -> Result<(), LogError> {
        let (file, profile) = self
            .profile
            .as_mut()
            .ok_or_else(|| invalid("delivery owner is unbound"))?;
        let mut restored = profile.clone();
        restored.adoption = None;
        file.rewrite(std::slice::from_ref(&restored))?;
        file.sync()?;
        *profile = restored;
        Ok(())
    }

    pub(crate) fn control_ids(&self) -> std::collections::HashMap<EventId, ControlBinding> {
        self.receipts
            .values()
            .map(|r| {
                (
                    r.destination.event.id,
                    ControlBinding {
                        device: r.destination.device.clone(),
                        account: r.destination.account.clone(),
                    },
                )
            })
            .chain(self.completed_receipts.values().filter_map(|r| {
                self.completed_receipt_devices
                    .get(&r.original_event_id)
                    .map(|device| {
                        (
                            r.receipt_event_id,
                            ControlBinding {
                                device: device.clone(),
                                account: r.destination_account.clone(),
                            },
                        )
                    })
            }))
            .collect()
    }
    /// Local replay is independent of current network permissions. Bind accepted
    /// state to this profile before installing any saved ratchet transition.
    pub(crate) fn validate_owner(
        &self,
        own: &PublicIdentity,
        account: &str,
    ) -> Result<(), LogError> {
        let message_ok = |m: &OutgoingDelivery| {
            m.sender_account == account
                && m.destinations
                    .iter()
                    .all(|d| d.event.author.ed25519_pub() == &own.ed25519_pub)
        };
        let receipt_ok = |r: &ReceiptDelivery| {
            r.destination.event.author.ed25519_pub() == &own.ed25519_pub
                && r.destination.event.conversation_id
                    == super::delivery_receipt::delivery_conversation_id(own, &r.destination.device)
        };
        if self.messages.values().any(|m| !message_ok(m))
            || self.receipts.values().any(|r| !receipt_ok(r))
        {
            return Err(invalid("delivery store belongs to another profile"));
        }
        for tx in &self.pending {
            let valid = match tx {
                DeliveryTransaction::Outgoing { message, .. }
                | DeliveryTransaction::OutgoingManifest { message, .. } => message_ok(message),
                DeliveryTransaction::Incoming {
                    sender,
                    original,
                    received,
                    receipt,
                    ..
                } => {
                    let route_valid = if received.plaintext.starts_with(b"MTDE1") {
                        DmEnvelope::decode(&received.plaintext).is_some_and(|e| {
                            e.route.sender_account == account
                                || e.route.recipient_account == account
                        })
                    } else {
                        received.conversation == original.conversation_id
                            && received.from == sender.user_id()
                    };
                    original.conversation_id == super::conversation::dm_conversation_id(own, sender)
                        && sender.ed25519_pub != own.ed25519_pub
                        && route_valid
                        && receipt.as_ref().is_none_or(|r| receipt_ok(r))
                }
                DeliveryTransaction::IncomingManifest {
                    sender,
                    original,
                    received,
                    receipt,
                    ..
                } => {
                    original.conversation_id == super::conversation::dm_conversation_id(own, sender)
                        && sender.ed25519_pub != own.ed25519_pub
                        && received.from == sender.user_id()
                        && receipt.as_ref().is_none_or(|r| receipt_ok(r))
                }
            };
            if !valid {
                return Err(invalid("delivery transaction belongs to another profile"));
            }
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn open(dir: &Path, password: &str) -> Result<Self, LogError> {
        Self::open_with_limits(dir, password, DeliveryLimits::default())
    }

    #[cfg(test)]
    pub(crate) fn open_with_limits(
        dir: &Path,
        password: &str,
        limits: DeliveryLimits,
    ) -> Result<Self, LogError> {
        Self::open_paths(
            dir.join("delivery-transactions.log"),
            dir.join("delivery-outbox.log"),
            password,
            limits,
        )
    }

    pub(crate) fn open_for_log(log_path: &Path, password: &str) -> Result<Self, LogError> {
        let mut journal = log_path.as_os_str().to_os_string();
        journal.push(".delivery-transactions");
        let mut outbox = log_path.as_os_str().to_os_string();
        outbox.push(".delivery-outbox");
        Self::open_paths(
            journal.into(),
            outbox.into(),
            password,
            DeliveryLimits::default(),
        )
    }

    fn open_paths(
        journal_path: PathBuf,
        outbox_path: PathBuf,
        password: &str,
        limits: DeliveryLimits,
    ) -> Result<Self, LogError> {
        if limits.journal_bytes < HEADER_BYTES || limits.outbox_bytes < HEADER_BYTES {
            return Err(invalid("delivery file capacity"));
        }
        preflight(&journal_path, limits.journal_bytes, limits.transactions)?;
        let max_records = limits.max_outbox_records();
        preflight(&outbox_path, limits.outbox_bytes, max_records)?;
        let (journal, pending) = EncryptedRecordLog::open(&journal_path, password, JOURNAL_MAGIC)?;
        let (outbox, records) = EncryptedRecordLog::open(&outbox_path, password, OUTBOX_MAGIC)?;
        let mut store = Self {
            profile: None,
            journal,
            outbox,
            journal_path,
            outbox_path,
            pending: Vec::new(),
            messages: BTreeMap::new(),
            completed: BTreeMap::new(),
            active_work: BTreeSet::new(),
            work_events: BTreeMap::new(),
            destination_cursors: BTreeMap::new(),
            completed_receipts: BTreeMap::new(),
            completed_receipt_devices: BTreeMap::new(),
            receipts: BTreeMap::new(),
            receipt_events: BTreeMap::new(),
            receipt_scopes: BTreeMap::new(),
            files: BTreeMap::new(),
            active_files: BTreeSet::new(),
            file_completions: BTreeMap::new(),
            completion_index: BTreeMap::new(),
            file_conversations: BTreeMap::new(),
            erased_files: BTreeSet::new(),
            erased_manifest_events: BTreeSet::new(),
            limits,
            outbox_records: records.len(),
            needs_sync: false,
        };
        for record in records {
            store.validate_record(&record)?;
            store.apply_record(record)?;
        }
        let mut ids = HashSet::new();
        for tx in pending {
            store.validate_transaction(&tx)?;
            if let Some(record) = metadata_for(&tx) {
                store.record_installed(&record)?;
            }
            if !ids.insert(tx.id()) {
                return Err(invalid("duplicate delivery intent"));
            }
            store.pending.push(tx);
        }
        store.check_counts(None)?;
        Ok(store)
    }

    /// Preflight may run before detached ratchet preparation. `begin` also checks
    /// exact serialized size before touching any durable ratchet or history.
    pub(crate) fn check_capacity(
        &self,
        destinations: usize,
        estimated_bytes: u64,
    ) -> Result<(), LogError> {
        if destinations > self.limits.destinations
            || self.pending.len() >= self.limits.transactions
            || self
                .journal_path
                .metadata()?
                .len()
                .saturating_add(estimated_bytes)
                .saturating_add(FRAME_OVERHEAD)
                > self.limits.journal_bytes
        {
            return Err(invalid("delivery capacity exhausted"));
        }
        Ok(())
    }

    /// Success is the durable acceptance point. Return the same id to callers
    /// even if subsequent installation needs recovery. No network is eligible
    /// until recovery succeeds; local errors must never discard this intent.
    pub(crate) fn begin(&mut self, tx: DeliveryTransaction) -> Result<EventId, LogError> {
        self.sync_replacement()?;
        self.validate_transaction(&tx)?;
        if let Some(existing) = self
            .pending
            .iter()
            .find(|existing| existing.id() == tx.id())
        {
            if bincode::serialize(existing).map_err(|_| invalid("delivery encoding"))?
                != bincode::serialize(&tx).map_err(|_| invalid("delivery encoding"))?
            {
                return Err(invalid("conflicting delivery intent"));
            }
            self.journal.sync()?;
            return Ok(tx.id());
        }
        if matches!(&tx, DeliveryTransaction::Outgoing { message, .. } | DeliveryTransaction::OutgoingManifest { message, .. } if self.messages.contains_key(&message.logical_id) || self.completed.contains_key(&message.logical_id))
        {
            return Err(invalid("delivery message already installed"));
        }
        self.check_counts(Some(&tx))?;
        if let DeliveryTransaction::OutgoingManifest { file, .. }
        | DeliveryTransaction::IncomingManifest { file, .. } = &tx
        {
            self.check_file_slot(file)?;
        }
        let bytes = bincode::serialized_size(&tx).map_err(|_| invalid("delivery encoding"))?;
        let destinations = match &tx {
            DeliveryTransaction::Outgoing { message, .. }
            | DeliveryTransaction::OutgoingManifest { message, .. } => message.destinations.len(),
            _ => 1,
        };
        self.check_capacity(destinations, bytes)?;
        let id = tx.id();
        self.pending.push(tx);
        // Stage the reservation only under the owner lock. On any pre-WAL
        // failure remove it again; it is never visible as accepted work.
        let reservation = (|| {
            if let Some(record) = metadata_for(self.pending.last().expect("staged reservation")) {
                self.record_installed(&record)?;
                self.ensure_outbox_space(&record)?;
            } else if let DeliveryTransaction::IncomingManifest { file, .. } =
                self.pending.last().expect("staged reservation")
            {
                self.ensure_outbox_space(&OutboxRecord::FileCard(file.clone()))?;
            }
            self.journal
                .append_durable(self.pending.last().expect("staged reservation"))
        })();
        if let Err(error) = reservation {
            self.pending.pop();
            return Err(error);
        }
        Ok(id)
    }

    pub(crate) fn pending_transactions(&self) -> &[DeliveryTransaction] {
        &self.pending
    }

    pub(crate) fn status(&self, id: EventId) -> Option<DeliveryStatus> {
        if self.completed.contains_key(&id) {
            return Some(DeliveryStatus::Delivered);
        }
        self.messages.get(&id).map(|_| DeliveryStatus::Awaiting).or_else(|| {
            self.pending.iter().any(|tx| matches!(tx, DeliveryTransaction::Outgoing { message, .. } | DeliveryTransaction::OutgoingManifest { message, .. } if message.logical_id == id))
                .then_some(DeliveryStatus::Awaiting)
        })
    }

    pub(crate) fn status_in(
        &self,
        conversation: ConversationId,
        id: EventId,
    ) -> Option<DeliveryStatus> {
        let in_scope = self.messages.get(&id).is_some_and(|m| m.conversation == conversation)
            || self.completed.get(&id).is_some_and(|m| m.conversation == conversation)
            || self.pending.iter().any(|tx| matches!(tx, DeliveryTransaction::Outgoing { message, .. } | DeliveryTransaction::OutgoingManifest { message, .. } if message.logical_id == id && message.conversation == conversation));
        in_scope.then(|| self.status(id)).flatten()
    }

    pub(crate) fn cancellation_rows(&self) -> Vec<(ConversationId, EventId, u64)> {
        self.messages
            .values()
            .map(|m| (m.conversation, m.logical_id, m.wall_clock))
            .chain(
                self.completed
                    .values()
                    .map(|m| (m.conversation, m.logical_id, m.wall_clock)),
            )
            .chain(
                self.receipts
                    .values()
                    .map(|m| (m.conversation, m.logical_id, m.wall_clock)),
            )
            .chain(
                self.completed_receipts
                    .values()
                    .map(|m| (m.conversation, m.logical_id, m.wall_clock)),
            )
            .chain(
                self.files
                    .values()
                    .filter(|file| file.destinations.iter().any(|d| d.active))
                    .map(|file| (file.conversation, file.id, file.wall_clock)),
            )
            .collect()
    }

    /// Replay strictly in journal order. Ratchets cannot be used for unrelated
    /// operations while an earlier intent is incomplete. Locks are acquired by
    /// the caller in delivery → ratchet → log → received/sent order; no awaits.
    #[cfg(test)]
    pub(crate) fn recover_next(
        &mut self,
        ratchet: &mut DmRatchet,
        log: &mut PersistentEventLog,
        sent: &mut SentLog,
        received: &mut ReceivedLog,
    ) -> Result<Option<EventId>, LogError> {
        self.recover_next_with_files(ratchet, log, sent, received, None)
    }

    pub(crate) fn recover_next_with_files(
        &mut self,
        ratchet: &mut DmRatchet,
        log: &mut PersistentEventLog,
        sent: &mut SentLog,
        received: &mut ReceivedLog,
        files: Option<&mut ReceivedLog>,
    ) -> Result<Option<EventId>, LogError> {
        self.sync_replacement()?;
        let Some(tx) = self.pending.first() else {
            return Ok(None);
        };
        let id = tx.id();
        // The host syncs causal parents before begin. Re-sync before replay as
        // well, including the original incoming event and receipt's parents.
        log.sync()?;
        match tx {
            DeliveryTransaction::Outgoing {
                message,
                sent: entry,
                ratchets,
            } => {
                for prepared in ratchets {
                    ratchet.commit_prepared(prepared)?;
                }
                for destination in &message.destinations {
                    log.append_durable(destination.event.clone())?;
                }
                sent.record_durable(entry)?;
            }
            DeliveryTransaction::Incoming {
                original,
                ratchet: prepared,
                received: entry,
                receipt,
                ..
            } => {
                ratchet.commit_prepared(prepared)?;
                log.append_durable((**original).clone())?;
                received.record_durable(entry)?;
                // Persist/schedule the receipt only after plaintext durability.
                if let Some(receipt) = receipt {
                    log.append_durable(receipt.destination.event.clone())?;
                }
            }
            DeliveryTransaction::OutgoingManifest {
                message,
                received,
                file,
            } => {
                self.validate_staged_file(file, log, &message.destinations[0].event.author)?;
                for destination in &message.destinations {
                    log.append_durable(destination.event.clone())?;
                }
                files
                    .ok_or_else(|| invalid("file recovery adapter required"))?
                    .record_durable(received)?;
            }
            DeliveryTransaction::IncomingManifest {
                original, received, ..
            } => {
                log.append_durable((**original).clone())?;
                files
                    .ok_or_else(|| invalid("file recovery adapter required"))?
                    .record_durable(received)?;
            }
        }
        if let DeliveryTransaction::OutgoingManifest { file, .. }
        | DeliveryTransaction::IncomingManifest { file, .. } = &self.pending[0]
        {
            self.install_record(OutboxRecord::FileCard(file.clone()))?;
        }
        if let DeliveryTransaction::IncomingManifest {
            receipt: Some(receipt),
            ..
        } = &self.pending[0]
        {
            log.append_durable(receipt.destination.event.clone())?;
        }
        if let Some(record) = metadata_for(&self.pending[0]) {
            self.install_record(record)?;
        }
        // Rewrite commits before sync; update the live index even if the final
        // directory sync fails, and prohibit further mutations until it retries.
        let kept = &self.pending[1..];
        self.journal.rewrite(kept)?;
        self.pending.remove(0);
        self.needs_sync = true;
        self.sync_replacement()?;
        Ok(Some(id))
    }

    /// Caller authenticates a receipt against exact immutable destination events
    /// before invoking this method. Network presence or relay acceptance never
    /// invokes it. The durable record precedes the status index update.
    #[cfg(test)]
    pub(crate) fn mark_delivered(&mut self, id: EventId) -> Result<(), LogError> {
        self.sync_replacement()?;
        if self.completed.contains_key(&id) {
            return self.outbox.sync();
        }
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before confirming"));
        }
        let Some(message) = self.messages.get(&id) else {
            return Err(invalid("unknown delivery message"));
        };
        let mut completed = CompletedDelivery::from(message);
        completed.remaining.clear();
        completed.recipient_account = None;
        self.install_record(OutboxRecord::Delivered(completed))
    }

    pub(crate) fn mark_delivered_for(
        &mut self,
        id: EventId,
        original: EventId,
    ) -> Result<(), LogError> {
        self.sync_replacement()?;
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before confirming"));
        }
        if self.completed.contains_key(&id) {
            return self.retire_destination(id, original);
        }
        let message = self
            .messages
            .get(&id)
            .ok_or_else(|| invalid("unknown delivery message"))?;
        if !message
            .destinations
            .iter()
            .any(|d| d.event.id == original && d.receipt_eligible)
        {
            return Err(invalid("unknown confirmed destination"));
        }
        let mut completed = CompletedDelivery::from(message);
        completed.remaining.retain(|d| d.event_id != original);
        if completed.remaining.is_empty() {
            completed.recipient_account = None;
        }
        self.install_record(OutboxRecord::Delivered(completed))
    }

    /// Only exact authenticated durable remote-have proofs may retire transport
    /// references. Session success alone never calls this method.
    pub(crate) fn retire_destination(
        &mut self,
        id: EventId,
        original: EventId,
    ) -> Result<(), LogError> {
        self.sync_replacement()?;
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before retiring destination"));
        }
        let completed = self
            .completed
            .get(&id)
            .ok_or_else(|| invalid("unknown completed delivery"))?;
        if !completed.remaining.iter().any(|d| d.event_id == original) {
            return self.outbox.sync();
        }
        self.install_record(OutboxRecord::RetireDestination(id, original))
    }

    pub(crate) fn completed_work(&self, id: EventId) -> Option<&CompletedDelivery> {
        self.completed.get(&id).filter(|m| !m.remaining.is_empty())
    }

    #[cfg(test)]
    pub(crate) fn retry_completed_after(
        &self,
        cursor: Option<EventId>,
        limit: usize,
    ) -> Vec<CompletedDelivery> {
        if self.needs_sync || !self.pending.is_empty() {
            return Vec::new();
        }
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        self.completed
            .range((start, std::ops::Bound::Unbounded))
            .filter(|(_, m)| !m.remaining.is_empty())
            .take(limit.min(64))
            .map(|(_, m)| m.clone())
            .collect()
    }

    /// The host decides whether authenticated direct sync or durable relay
    /// transfer completed retry work. Retains the exact original→receipt id so
    /// restart cannot generate a new receipt for an already saved message.
    pub(crate) fn finish_receipt(
        &mut self,
        conversation: ConversationId,
        original_event_id: EventId,
    ) -> Result<(), LogError> {
        self.sync_replacement()?;
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before completing receipt"));
        }
        if self
            .completed_receipts
            .get(&original_event_id)
            .is_some_and(|receipt| receipt.conversation == conversation)
        {
            return self.outbox.sync();
        }
        let receipt = self
            .receipts
            .values()
            .find(|r| r.original_event_id == original_event_id && r.conversation == conversation)
            .ok_or_else(|| invalid("unknown delivery receipt"))?;
        self.install_record(OutboxRecord::FinishedBoundReceipt(
            CompletedReceipt::from(receipt),
            receipt.destination.device.clone(),
        ))
    }

    pub(crate) fn has_receipt_for(
        &self,
        conversation: ConversationId,
        original_event_id: EventId,
    ) -> bool {
        self.completed_receipts.get(&original_event_id).is_some_and(|r| r.conversation == conversation)
            || self.receipt_events.get(&original_event_id).and_then(|id| self.receipts.get(id)).is_some_and(|r| r.conversation == conversation)
            || self.pending.iter().any(|tx| matches!(tx, DeliveryTransaction::Incoming { receipt: Some(r), .. } | DeliveryTransaction::IncomingManifest { receipt: Some(r), .. } if r.original_event_id == original_event_id && r.conversation == conversation))
    }

    pub(crate) fn message(&self, id: EventId) -> Option<&OutgoingDelivery> {
        self.messages.get(&id)
    }

    pub(crate) fn logical_for_original(&self, original: EventId) -> Option<EventId> {
        self.work_events.get(&original).copied()
    }

    pub(crate) fn file_cards(&self) -> impl Iterator<Item = &FileCard> {
        self.files.values()
    }

    pub(crate) fn file_card(&self, id: EventId) -> Option<&FileCard> {
        self.files.get(&id)
    }

    pub(crate) fn complete_file(
        &mut self,
        card: EventId,
        final_chunk: EventId,
    ) -> Result<(), LogError> {
        self.install_record(OutboxRecord::FileComplete(card, final_chunk))
    }

    pub(crate) fn completed_files(
        &self,
        conversation: ConversationId,
        final_chunk: EventId,
    ) -> Vec<FileCard> {
        self.completion_index
            .get(&(*conversation.as_bytes(), final_chunk))
            .into_iter()
            .flatten()
            .filter_map(|id| self.files.get(id))
            .cloned()
            .collect()
    }

    pub(crate) fn file_completed(&self, id: EventId) -> bool {
        self.file_completions.contains_key(&id)
    }

    pub(crate) fn cards_for_file(&self, conversation: ConversationId) -> Vec<FileCard> {
        self.file_conversations
            .get(conversation.as_bytes())
            .into_iter()
            .flatten()
            .filter_map(|id| self.files.get(id))
            .cloned()
            .collect()
    }

    fn remove_file_completion(&mut self, id: EventId) {
        if let Some(final_chunk) = self.file_completions.remove(&id) {
            if let Some(file) = self.files.get(&id) {
                let key = (*file.file_conversation.as_bytes(), final_chunk);
                if let Some(ids) = self.completion_index.get_mut(&key) {
                    ids.remove(&id);
                    if ids.is_empty() {
                        self.completion_index.remove(&key);
                    }
                }
            }
        }
    }

    pub(crate) fn file_erased(&self, conversation: ConversationId, original: EventId) -> bool {
        self.erased_files
            .contains(&(*conversation.as_bytes(), original))
    }

    /// Signed manifest IDs include their actual wire conversation. UI account
    /// aliases may change after account adoption, but the erased event cannot.
    pub(crate) fn manifest_event_erased(&self, original: EventId) -> bool {
        self.erased_manifest_events.contains(&original)
    }

    pub(crate) fn erase_file(
        &mut self,
        conversation: ConversationId,
        original: EventId,
    ) -> Result<bool, LogError> {
        if self.file_erased(conversation, original) {
            return Ok(false);
        }
        // Legacy imported rows have no reserved slot; fail before deleting them
        // when permanent suppression cannot fit the bounded metadata store.
        if !self
            .files
            .get(&original)
            .is_some_and(|file| file.conversation == conversation)
            && self.files.len().saturating_add(self.erased_files.len())
                >= self.limits.completed_receipts
        {
            return Err(invalid("file erasure capacity exhausted"));
        }
        self.install_record(OutboxRecord::ErasedFile(conversation, original))?;
        Ok(true)
    }

    pub(crate) fn next_file_destination(
        &self,
        cursor: Option<(EventId, EventId)>,
    ) -> Option<(FileCard, FileDestination)> {
        if self.needs_sync || !self.pending.is_empty() {
            return None;
        }
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        let (id, original) = *self
            .active_files
            .range((start, std::ops::Bound::Unbounded))
            .next()
            .or_else(|| self.active_files.first())?;
        let file = self.files.get(&id)?;
        let destination = file
            .destinations
            .iter()
            .find(|d| d.binding.event_id == original)?
            .clone();
        Some((file.clone(), destination))
    }

    pub(crate) fn contains_file_destination(
        &self,
        id: EventId,
        original: EventId,
        device: &PublicIdentity,
        account: &Option<String>,
    ) -> bool {
        self.files.get(&id).is_some_and(|file| {
            file.destinations.iter().any(|d| {
                d.active
                    && d.binding.event_id == original
                    && d.binding.device == *device
                    && d.binding.account == *account
            })
        })
    }

    pub(crate) fn retire_file_destination(
        &mut self,
        id: EventId,
        original: EventId,
    ) -> Result<(), LogError> {
        self.install_record(OutboxRecord::RetireFileDestination(id, original))
    }

    fn check_file_slot(&self, file: &FileCard) -> Result<(), LogError> {
        let pending_slots = self
            .pending
            .iter()
            .filter_map(|tx| match tx {
                DeliveryTransaction::OutgoingManifest { file, .. }
                | DeliveryTransaction::IncomingManifest { file, .. }
                    if !self.files.contains_key(&file.id) =>
                {
                    Some(file.id)
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        if self.file_erased(file.conversation, file.id)
            || (self
                .files
                .get(&file.id)
                .is_some_and(|existing| existing != file))
            || (!self.files.contains_key(&file.id)
                && !pending_slots.contains(&file.id)
                && self
                    .files
                    .len()
                    .saturating_add(self.erased_files.len())
                    .saturating_add(pending_slots.len())
                    >= self.limits.completed_receipts)
        {
            return Err(invalid("file card capacity or binding rejected"));
        }
        Ok(())
    }

    pub(crate) fn validate_staged_file(
        &self,
        file: &FileCard,
        log: &PersistentEventLog,
        author: &crate::eventlog::Author,
    ) -> Result<(), LogError> {
        let chunks = log.events(&file.file_conversation);
        if chunks.len() != file.chunk_count as usize
            || chunks.last().map(|e| e.id) != file.final_chunk
        {
            return Err(invalid("invalid immutable file chunk count"));
        }
        for (index, event) in chunks.iter().enumerate() {
            let parents = if index == 0 {
                vec![]
            } else {
                vec![chunks[index - 1].id]
            };
            if event.kind != EventKind::Message
                || event.seq != index as u64 + 1
                || event.parents != parents
                || !event.verify_integrity()
                || !event.verify_signature()
                || &event.author != author
                || (index > 0 && event.author != chunks[0].author)
            {
                return Err(invalid("invalid immutable file chunk ancestry"));
            }
        }
        Ok(())
    }

    /// Build the immutable local File card, history row and fanout as one
    /// transaction. The store owns their shared identity and chunk binding;
    /// callers only prepare the signed destination events.
    pub(crate) fn prepare_outgoing_manifest(
        &self,
        message: OutgoingDelivery,
        plaintext: Vec<u8>,
        log: &PersistentEventLog,
    ) -> Result<DeliveryTransaction, LogError> {
        let manifest = super::files::validated_manifest(&plaintext)
            .ok_or_else(|| invalid("invalid file manifest"))?;
        let first = message
            .destinations
            .first()
            .ok_or_else(|| invalid("no file destinations"))?;
        let file = FileCard {
            id: message.logical_id,
            conversation: message.conversation,
            wall_clock: message.wall_clock,
            file_conversation: manifest.file_conv(),
            final_chunk: log
                .events(&manifest.file_conv())
                .last()
                .map(|event| event.id),
            chunk_count: manifest.chunk_count(),
            destinations: message
                .destinations
                .iter()
                .map(|destination| FileDestination {
                    binding: DeliveryReference {
                        device: destination.device.clone(),
                        account: destination.account.clone(),
                        event_id: destination.event.id,
                        receipt_eligible: destination.receipt_eligible,
                    },
                    active: true,
                })
                .collect(),
            completion_binding: None,
        };
        self.validate_staged_file(&file, log, &first.event.author)?;
        let received = Box::new(ReceivedEntry {
            event_id: message.logical_id,
            conversation: message.conversation,
            from: first.event.author.user_id(),
            wall_clock: message.wall_clock,
            plaintext,
        });
        Ok(DeliveryTransaction::OutgoingManifest {
            message,
            received,
            file,
        })
    }

    /// A single owned destination, selected by a stable two-dimensional cursor.
    /// This never copies the other ciphertexts in a fanout message.
    pub(crate) fn next_destination(
        &mut self,
        cursor: Option<EventId>,
    ) -> Option<(EventId, DeliveryReference)> {
        if self.needs_sync || !self.pending.is_empty() {
            return None;
        }
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        let id = *self
            .active_work
            .range((start, std::ops::Bound::Unbounded))
            .next()
            .or_else(|| self.active_work.first())?;
        let references = if let Some(m) = self.messages.get(&id) {
            m.destinations
                .iter()
                .map(|d| DeliveryReference {
                    device: d.device.clone(),
                    account: d.account.clone(),
                    event_id: d.event.id,
                    receipt_eligible: d.receipt_eligible,
                })
                .collect::<Vec<_>>()
        } else {
            self.completed.get(&id)?.remaining.clone()
        };
        let reference = references
            .iter()
            .filter(|d| {
                self.destination_cursors
                    .get(&id)
                    .is_none_or(|c| d.event_id > *c)
            })
            .min_by_key(|d| d.event_id)
            .or_else(|| references.iter().min_by_key(|d| d.event_id))?
            .clone();
        self.destination_cursors.insert(id, reference.event_id);
        Some((id, reference))
    }

    pub(crate) fn contains_destination(&self, id: EventId, event: EventId) -> bool {
        self.messages
            .get(&id)
            .is_some_and(|m| m.destinations.iter().any(|d| d.event.id == event))
            || self
                .completed
                .get(&id)
                .is_some_and(|m| m.remaining.iter().any(|d| d.event_id == event))
    }

    pub(crate) fn contains_work_event(&self, event: EventId) -> bool {
        self.receipts.contains_key(&event) || self.work_events.contains_key(&event)
    }

    /// Owned bounded snapshots release the delivery lock before transport awaits.
    #[cfg(test)]
    pub(crate) fn retry_messages(&self, limit: usize) -> Vec<OutgoingDelivery> {
        self.retry_messages_after(None, limit)
    }

    #[cfg(test)]
    pub(crate) fn retry_messages_after(
        &self,
        cursor: Option<EventId>,
        limit: usize,
    ) -> Vec<OutgoingDelivery> {
        if self.needs_sync || !self.pending.is_empty() {
            return Vec::new();
        }
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        self.messages
            .range((start, std::ops::Bound::Unbounded))
            .map(|(_, message)| message)
            .take(limit.min(64))
            .cloned()
            .collect()
    }

    pub(crate) fn retry_receipts(&self, limit: usize) -> Vec<ReceiptDelivery> {
        self.retry_receipts_after(None, limit)
    }

    pub(crate) fn retry_receipts_after(
        &self,
        cursor: Option<EventId>,
        limit: usize,
    ) -> Vec<ReceiptDelivery> {
        if self.needs_sync || !self.pending.is_empty() {
            return Vec::new();
        }
        let start = cursor.map_or(std::ops::Bound::Unbounded, std::ops::Bound::Excluded);
        self.receipts
            .range((start, std::ops::Bound::Unbounded))
            .map(|(_, receipt)| receipt)
            .take(limit.min(64))
            .cloned()
            .collect()
    }

    /// Called inside the same guard after pending recovery and before local
    /// history deletion. Cancels related immutable retry/status metadata. The
    /// host must retain its emitted/erasure bookkeeping to prevent re-decrypt.
    pub(crate) fn cancel(
        &mut self,
        conversation: ConversationId,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.sync_replacement()?;
        if !self.pending.is_empty() {
            return Err(invalid("recover delivery before erasing history"));
        }
        if !self
            .messages
            .get(&id)
            .is_some_and(|m| m.conversation == conversation)
            && !self
                .completed
                .get(&id)
                .is_some_and(|m| m.conversation == conversation)
            && !self
                .receipt_scopes
                .contains_key(&(*conversation.as_bytes(), id))
        {
            self.outbox.sync()?;
            return Ok(false);
        }
        self.install_record(OutboxRecord::Cancel(conversation, id))?;
        Ok(true)
    }

    pub(crate) fn sync_replacement(&mut self) -> Result<(), LogError> {
        if self.needs_sync {
            self.journal.sync()?;
            self.outbox.sync()?;
            self.needs_sync = false;
        }
        Ok(())
    }

    fn install_record(&mut self, record: OutboxRecord) -> Result<(), LogError> {
        self.validate_record(&record)?;
        if self.record_installed(&record)? {
            return self.outbox.sync();
        }
        self.ensure_outbox_space(&record)?;
        self.outbox.append_durable(&record)?;
        self.outbox_records += 1;
        self.apply_record(record)
    }

    fn record_installed(&self, record: &OutboxRecord) -> Result<bool, LogError> {
        match record {
            OutboxRecord::FileComplete(id, final_chunk) => {
                if self
                    .files
                    .get(id)
                    .is_none_or(|file| file.completion_binding.is_none())
                {
                    return Err(invalid("file completion binding unavailable"));
                }
                match self.file_completions.get(id) {
                    Some(existing) if existing != final_chunk => {
                        Err(invalid("conflicting file completion"))
                    }
                    Some(_) => Ok(true),
                    None => Ok(false),
                }
            }
            OutboxRecord::FileCard(file) => {
                if let Some(existing) = self.files.get(&file.id) {
                    if existing != file {
                        return Err(invalid("conflicting immutable file card"));
                    }
                    return Ok(true);
                }
                Ok(false)
            }
            OutboxRecord::RetireFileDestination(id, original) => {
                if !self.files.get(id).is_some_and(|file| {
                    file.destinations
                        .iter()
                        .any(|d| d.binding.event_id == *original)
                }) {
                    return Err(invalid("unknown file destination"));
                }
                Ok(!self.active_files.contains(&(*id, *original)))
            }
            OutboxRecord::ErasedFile(conversation, original) => {
                Ok(self.file_erased(*conversation, *original))
            }
            OutboxRecord::FinishedBoundReceipt(receipt, device) => {
                if let Some(existing) = self
                    .completed_receipt_devices
                    .get(&receipt.original_event_id)
                {
                    if existing != device {
                        return Err(invalid("conflicting completed receipt device"));
                    }
                    return self.record_installed(&OutboxRecord::FinishedReceipt(receipt.clone()));
                }
                Ok(false)
            }
            OutboxRecord::Message(message) => match self.messages.get(&message.logical_id) {
                Some(existing) if existing != message => {
                    Err(invalid("conflicting delivery metadata"))
                }
                Some(_) => Ok(true),
                None => Ok(false),
            },
            OutboxRecord::Receipt(receipt) => {
                match self.receipts.get(&receipt.destination.event.id) {
                    Some(existing) if existing != receipt.as_ref() => {
                        Err(invalid("conflicting receipt metadata"))
                    }
                    Some(_) => Ok(true),
                    None => {
                        if let Some(completed) =
                            self.completed_receipts.get(&receipt.original_event_id)
                        {
                            if *completed != CompletedReceipt::from(receipt.as_ref()) {
                                return Err(invalid("conflicting completed receipt metadata"));
                            }
                            return Ok(true);
                        }
                        if self.receipt_events.contains_key(&receipt.original_event_id) {
                            return Err(invalid("duplicate original receipt metadata"));
                        }
                        Ok(false)
                    }
                }
            }
            OutboxRecord::Delivered(message) => match self.completed.get(&message.logical_id) {
                Some(existing) if existing != message => {
                    Err(invalid("conflicting completed delivery metadata"))
                }
                Some(_) => Ok(true),
                None => Ok(false),
            },
            OutboxRecord::FinishedReceipt(receipt) => {
                match self.completed_receipts.get(&receipt.original_event_id) {
                    Some(existing) if existing != receipt => {
                        Err(invalid("conflicting completed receipt metadata"))
                    }
                    Some(_) => Ok(true),
                    None => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }

    fn apply_record(&mut self, record: OutboxRecord) -> Result<(), LogError> {
        if self.record_installed(&record)? {
            return Ok(());
        }
        let changed = match &record {
            OutboxRecord::Message(m) => Some(m.logical_id),
            OutboxRecord::Delivered(m) => Some(m.logical_id),
            OutboxRecord::RetireDestination(id, _) | OutboxRecord::Cancel(_, id) => Some(*id),
            _ => None,
        };
        if let Some(id) = changed {
            if let Some(m) = self.messages.get(&id) {
                for d in &m.destinations {
                    self.work_events.remove(&d.event.id);
                }
            }
            if let Some(m) = self.completed.get(&id) {
                for d in &m.remaining {
                    self.work_events.remove(&d.event_id);
                }
            }
            self.active_work.remove(&id);
        }
        match record {
            OutboxRecord::FileComplete(id, final_chunk) => {
                let file = self
                    .files
                    .get(&id)
                    .ok_or_else(|| invalid("unknown completed file"))?;
                self.completion_index
                    .entry((*file.file_conversation.as_bytes(), final_chunk))
                    .or_default()
                    .insert(id);
                self.file_completions.insert(id, final_chunk);
            }
            OutboxRecord::FileCard(file) => {
                self.check_file_slot(&file)?;
                self.file_conversations
                    .entry(*file.file_conversation.as_bytes())
                    .or_default()
                    .insert(file.id);
                for destination in &file.destinations {
                    if destination.active {
                        self.active_files
                            .insert((file.id, destination.binding.event_id));
                    }
                }
                self.files.insert(file.id, file);
            }
            OutboxRecord::RetireFileDestination(id, original) => {
                if let Some(file) = self.files.get_mut(&id) {
                    if let Some(destination) = file
                        .destinations
                        .iter_mut()
                        .find(|d| d.binding.event_id == original)
                    {
                        destination.active = false;
                    }
                }
                self.active_files.remove(&(id, original));
            }
            OutboxRecord::ErasedFile(conversation, original) => {
                self.apply_record(OutboxRecord::Cancel(conversation, original))?;
                if self
                    .files
                    .get(&original)
                    .is_some_and(|file| file.conversation == conversation)
                {
                    let file = self.files.remove(&original).expect("checked file");
                    if let Some(ids) = self
                        .file_conversations
                        .get_mut(file.file_conversation.as_bytes())
                    {
                        ids.remove(&original);
                        if ids.is_empty() {
                            self.file_conversations
                                .remove(file.file_conversation.as_bytes());
                        }
                    }
                    for destination in file.destinations {
                        self.active_files
                            .remove(&(original, destination.binding.event_id));
                    }
                }
                self.erased_files
                    .insert((*conversation.as_bytes(), original));
                self.erased_manifest_events.insert(original);
            }
            OutboxRecord::Message(message) => {
                if self.completed.contains_key(&message.logical_id) {
                    return Err(invalid("completed delivery cannot regress"));
                }
                self.messages.insert(message.logical_id, message);
            }
            OutboxRecord::Receipt(receipt) => {
                self.index_receipt(
                    receipt.conversation,
                    receipt.logical_id,
                    receipt.original_event_id,
                );
                self.receipt_events
                    .insert(receipt.original_event_id, receipt.destination.event.id);
                self.receipts.insert(receipt.destination.event.id, *receipt);
            }
            OutboxRecord::Delivered(completed) => {
                if let Some(message) = self.messages.get(&completed.logical_id) {
                    let expected = CompletedDelivery::from(message);
                    if expected.conversation != completed.conversation
                        || expected.wall_clock != completed.wall_clock
                        || (!completed.remaining.is_empty()
                            && expected.recipient_account != completed.recipient_account)
                        || completed
                            .remaining
                            .iter()
                            .any(|d| !expected.remaining.contains(d))
                    {
                        return Err(invalid("conflicting completed delivery metadata"));
                    }
                }
                self.messages.remove(&completed.logical_id);
                self.completed.insert(completed.logical_id, completed);
            }
            OutboxRecord::RetireDestination(id, event) => {
                let completed = self
                    .completed
                    .get_mut(&id)
                    .ok_or_else(|| invalid("unknown completed delivery"))?;
                completed.remaining.retain(|d| d.event_id != event);
                if completed.remaining.is_empty() {
                    completed.recipient_account = None;
                }
            }
            OutboxRecord::FinishedBoundReceipt(completed, device) => {
                let original = completed.original_event_id;
                self.apply_record(OutboxRecord::FinishedReceipt(completed))?;
                self.completed_receipt_devices.insert(original, device);
            }
            OutboxRecord::FinishedReceipt(completed) => {
                if let Some(receipt) = self.receipts.get(&completed.receipt_event_id) {
                    if CompletedReceipt::from(receipt) != completed {
                        return Err(invalid("conflicting completed receipt metadata"));
                    }
                }
                self.receipts.remove(&completed.receipt_event_id);
                self.receipt_events.remove(&completed.original_event_id);
                self.index_receipt(
                    completed.conversation,
                    completed.logical_id,
                    completed.original_event_id,
                );
                self.completed_receipts
                    .insert(completed.original_event_id, completed);
            }
            OutboxRecord::Cancel(conversation, id) => {
                if self
                    .files
                    .get(&id)
                    .is_some_and(|file| file.conversation == conversation)
                {
                    self.remove_file_completion(id);
                }
                if let Some(file) = self
                    .files
                    .get_mut(&id)
                    .filter(|file| file.conversation == conversation)
                {
                    file.completion_binding = None;
                    for destination in &mut file.destinations {
                        destination.active = false;
                        self.active_files
                            .remove(&(id, destination.binding.event_id));
                    }
                }
                if self
                    .messages
                    .get(&id)
                    .is_some_and(|m| m.conversation == conversation)
                {
                    self.messages.remove(&id);
                }
                if self
                    .completed
                    .get(&id)
                    .is_some_and(|m| m.conversation == conversation)
                {
                    self.completed.remove(&id);
                }
                let originals = self
                    .receipt_scopes
                    .get(&(*conversation.as_bytes(), id))
                    .cloned()
                    .unwrap_or_default();
                for original in originals {
                    let active = self
                        .receipt_events
                        .remove(&original)
                        .and_then(|ack| self.receipts.remove(&ack));
                    let completed = self.completed_receipts.remove(&original);
                    self.completed_receipt_devices.remove(&original);
                    let binding = active
                        .map(|r| (r.conversation, r.logical_id))
                        .or_else(|| completed.map(|r| (r.conversation, r.logical_id)));
                    if let Some((scope, logical)) = binding {
                        for key in [(*scope.as_bytes(), logical), (*scope.as_bytes(), original)] {
                            if let Some(ids) = self.receipt_scopes.get_mut(&key) {
                                ids.remove(&original);
                                if ids.is_empty() {
                                    self.receipt_scopes.remove(&key);
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(id) = changed {
            if let Some(m) = self.messages.get(&id) {
                for d in &m.destinations {
                    self.work_events.insert(d.event.id, id);
                }
            }
            if let Some(m) = self.completed.get(&id) {
                for d in &m.remaining {
                    self.work_events.insert(d.event_id, id);
                }
            }
            if self.messages.contains_key(&id) || self.completed_work(id).is_some() {
                self.active_work.insert(id);
            } else {
                self.destination_cursors.remove(&id);
            }
        }
        if self.messages.len() > self.limits.messages
            || self.receipts.len() > self.limits.receipts
            || self.completed.len() > self.limits.completed
            || self.completed_receipts.len() > self.limits.completed_receipts
            || self.files.len().saturating_add(self.erased_files.len())
                > self.limits.completed_receipts
        {
            return Err(invalid("delivery capacity exhausted"));
        }
        Ok(())
    }

    fn index_receipt(&mut self, conversation: ConversationId, logical: EventId, original: EventId) {
        for key in [
            (*conversation.as_bytes(), logical),
            (*conversation.as_bytes(), original),
        ] {
            self.receipt_scopes.entry(key).or_default().insert(original);
        }
    }

    fn ensure_outbox_space(&mut self, record: &OutboxRecord) -> Result<(), LogError> {
        let mut size = bincode::serialized_size(record)
            .map_err(|_| invalid("delivery encoding"))?
            .saturating_add(FRAME_OVERHEAD);
        // Each accepted row already reserves its cancellation record. A valid
        // cancellation releases at least this much future debt; avoid scanning
        // every other row during a local bulk erase. Compaction still handles
        // physical byte/record thresholds through the ordinary fallback below.
        if let OutboxRecord::Cancel(conversation, id) = record {
            let valid_scope = self
                .messages
                .get(id)
                .is_some_and(|m| m.conversation == *conversation)
                || self
                    .completed
                    .get(id)
                    .is_some_and(|m| m.conversation == *conversation)
                || self
                    .receipt_scopes
                    .contains_key(&(*conversation.as_bytes(), *id));
            if valid_scope
                && !self.needs_sync
                && self.pending.is_empty()
                && self.outbox_path.metadata()?.len().saturating_add(size)
                    <= self.limits.outbox_bytes
                && self.outbox_records.saturating_add(1) <= self.limits.max_outbox_records()
            {
                return Ok(());
            }
        }
        // A reserved completion consumes its own future slot. Only exact live
        // bindings qualify: imported legacy rows have no reservation to spend.
        let erased_card = match record {
            OutboxRecord::ErasedFile(conversation, id) => self
                .files
                .get(id)
                .filter(|file| file.conversation == *conversation),
            _ => None,
        };
        let retired_work = match record {
            OutboxRecord::RetireFileDestination(id, original) => {
                usize::from(self.active_files.contains(&(*id, *original)))
            }
            _ => 0,
        };
        let file_debt = self
            .files
            .len()
            .saturating_sub(usize::from(erased_card.is_some()));
        let completion_debt = self
            .files
            .values()
            .filter(|file| {
                file.completion_binding.is_some()
                    && !self.file_completions.contains_key(&file.id)
                    && erased_card.is_none_or(|erased| erased.id != file.id)
                    && !matches!(record, OutboxRecord::FileComplete(id, _) if *id == file.id)
            })
            .count();
        let work_debt = self
            .active_files
            .len()
            .saturating_sub(retired_work)
            .saturating_sub(erased_card.map_or(0, |file| {
                file.destinations.iter().filter(|d| d.active).count()
            }));
        let mut reserved_records = 1usize;
        reserved_records = reserved_records
            .saturating_add(file_debt)
            .saturating_add(completion_debt)
            .saturating_add(work_debt);
        if let OutboxRecord::FileCard(file) = record {
            reserved_records = reserved_records
                .saturating_add(file.destinations.len())
                .saturating_add(1 + usize::from(file.completion_binding.is_some()));
        }
        for tx in &self.pending {
            if let DeliveryTransaction::OutgoingManifest { file, .. }
            | DeliveryTransaction::IncomingManifest { file, .. } = tx
            {
                let pending_file = OutboxRecord::FileCard(file.clone());
                if !self.record_installed(&pending_file)?
                    && !matches!(record, OutboxRecord::FileCard(current) if current.id == file.id)
                {
                    size = size.saturating_add(
                        bincode::serialized_size(&pending_file)
                            .map_err(|_| invalid("delivery encoding"))?
                            .saturating_add(FRAME_OVERHEAD),
                    );
                    reserved_records = reserved_records.saturating_add(1);
                    size = size.saturating_add(
                        100 + (file.destinations.len() as u64).saturating_mul(100)
                            + u64::from(file.completion_binding.is_some()) * 100,
                    );
                    reserved_records = reserved_records.saturating_add(
                        1 + file.destinations.len()
                            + usize::from(file.completion_binding.is_some()),
                    );
                }
            }
            if let Some(pending_record) = metadata_for(tx) {
                if same_metadata_key(record, &pending_record)
                    || self.record_installed(&pending_record)?
                {
                    continue;
                }
                size = size.saturating_add(
                    bincode::serialized_size(&pending_record)
                        .map_err(|_| invalid("delivery encoding"))?
                        .saturating_add(FRAME_OVERHEAD),
                );
                reserved_records = reserved_records.saturating_add(1);
            }
        }
        // Accepted work always retains room for status completion and local
        // cancellation, even at the configured byte cap. This is logical
        // capacity reservation; filesystem failures still surface normally.
        let delivered_size =
            bincode::serialized_size(&OutboxRecord::Delivered(CompletedDelivery {
                logical_id: EventId::new([0; 32]),
                conversation: ConversationId::new([0; 32]),
                wall_clock: 0,
                recipient_account: None,
                remaining: Vec::new(),
            }))
            .map_err(|_| invalid("delivery encoding"))?
            .saturating_add(FRAME_OVERHEAD);
        let cancel_size = bincode::serialized_size(&OutboxRecord::Cancel(
            ConversationId::new([0; 32]),
            EventId::new([0; 32]),
        ))
        .map_err(|_| invalid("delivery encoding"))?
        .saturating_add(FRAME_OVERHEAD);
        let finished_size =
            bincode::serialized_size(&OutboxRecord::FinishedReceipt(CompletedReceipt {
                logical_id: EventId::new([0; 32]),
                original_event_id: EventId::new([0; 32]),
                receipt_event_id: EventId::new([0; 32]),
                conversation: ConversationId::new([0; 32]),
                wall_clock: 0,
                destination_account: Some("0".repeat(32)),
            }))
            .map_err(|_| invalid("delivery encoding"))?
            .saturating_add(FRAME_OVERHEAD)
            .saturating_add(64);
        let mut messages: BTreeMap<EventId, DeliveryStatus> = self
            .messages
            .keys()
            .map(|id| (*id, DeliveryStatus::Awaiting))
            .collect();
        messages.extend(
            self.completed
                .keys()
                .map(|id| (*id, DeliveryStatus::Delivered)),
        );
        let mut receipts: BTreeMap<EventId, &ReceiptDelivery> = self
            .receipts
            .iter()
            .map(|(id, receipt)| (*id, receipt))
            .collect();
        let mut completed_receipts: BTreeMap<EventId, &CompletedReceipt> = self
            .completed_receipts
            .iter()
            .map(|(id, receipt)| (*id, receipt))
            .collect();
        for tx in &self.pending {
            match tx {
                DeliveryTransaction::Outgoing { message, .. }
                | DeliveryTransaction::OutgoingManifest { message, .. } => {
                    messages
                        .entry(message.logical_id)
                        .or_insert(DeliveryStatus::Awaiting);
                }
                DeliveryTransaction::Incoming {
                    receipt: Some(receipt),
                    ..
                }
                | DeliveryTransaction::IncomingManifest {
                    receipt: Some(receipt),
                    ..
                } => {
                    receipts
                        .entry(receipt.destination.event.id)
                        .or_insert(receipt.as_ref());
                }
                _ => {}
            }
        }
        match record {
            OutboxRecord::Message(message) => {
                messages
                    .entry(message.logical_id)
                    .or_insert(DeliveryStatus::Awaiting);
            }
            OutboxRecord::Receipt(receipt) => {
                receipts
                    .entry(receipt.destination.event.id)
                    .or_insert(receipt.as_ref());
            }
            OutboxRecord::Delivered(message) => {
                messages.insert(message.logical_id, DeliveryStatus::Delivered);
            }
            OutboxRecord::FinishedReceipt(receipt)
            | OutboxRecord::FinishedBoundReceipt(receipt, _) => {
                receipts.remove(&receipt.receipt_event_id);
                completed_receipts.insert(receipt.original_event_id, receipt);
            }
            OutboxRecord::Cancel(conversation, id) => {
                if self
                    .messages
                    .get(id)
                    .is_some_and(|m| m.conversation == *conversation)
                    || self
                        .completed
                        .get(id)
                        .is_some_and(|m| m.conversation == *conversation)
                {
                    messages.remove(id);
                }
                receipts.retain(|_, receipt| {
                    receipt.conversation != *conversation
                        || (receipt.logical_id != *id && receipt.original_event_id != *id)
                });
                completed_receipts.retain(|_, receipt| {
                    receipt.conversation != *conversation
                        || (receipt.logical_id != *id && receipt.original_event_id != *id)
                });
            }
            OutboxRecord::RetireDestination(_, _)
            | OutboxRecord::FileCard(_)
            | OutboxRecord::RetireFileDestination(_, _)
            | OutboxRecord::FileComplete(_, _)
            | OutboxRecord::ErasedFile(_, _) => {}
        }
        let future_bytes = messages
            .values()
            .map(|status| {
                if *status == DeliveryStatus::Awaiting {
                    delivered_size + cancel_size
                } else {
                    cancel_size
                }
            })
            .sum::<u64>()
            .saturating_add((cancel_size + finished_size).saturating_mul(receipts.len() as u64))
            .saturating_add(cancel_size.saturating_mul(completed_receipts.len() as u64));
        let retire_size = bincode::serialized_size(&OutboxRecord::RetireDestination(
            EventId::new([0; 32]),
            EventId::new([0; 32]),
        ))
        .map_err(|_| invalid("delivery encoding"))?
        .saturating_add(FRAME_OVERHEAD);
        let mut work: BTreeMap<EventId, (bool, Vec<DeliveryReference>)> = self
            .messages
            .values()
            .map(|m| (m.logical_id, (true, CompletedDelivery::from(m).remaining)))
            .collect();
        work.extend(
            self.completed
                .values()
                .filter(|m| !m.remaining.is_empty())
                .map(|m| (m.logical_id, (false, m.remaining.clone()))),
        );
        for tx in &self.pending {
            if let DeliveryTransaction::Outgoing { message, .. }
            | DeliveryTransaction::OutgoingManifest { message, .. } = tx
            {
                work.entry(message.logical_id)
                    .or_insert_with(|| (true, CompletedDelivery::from(message).remaining));
            }
        }
        match record {
            OutboxRecord::Message(m) => {
                work.insert(m.logical_id, (true, CompletedDelivery::from(m).remaining));
            }
            OutboxRecord::Delivered(m) => {
                work.insert(m.logical_id, (false, m.remaining.clone()));
            }
            OutboxRecord::RetireDestination(id, event) => {
                if let Some((_, refs)) = work.get_mut(id) {
                    refs.retain(|r| r.event_id != *event);
                }
            }
            OutboxRecord::Cancel(_, id) if !messages.contains_key(id) => {
                work.remove(id);
            }
            _ => {}
        }
        let mut fanout_bytes = 0u64;
        for (awaiting, refs) in work.values() {
            if *awaiting && refs.len() > 1 {
                // Initial confirmation contains compact remaining refs plus the
                // recipient account; reserve the worst case before acceptance.
                fanout_bytes = fanout_bytes
                    .saturating_add(
                        bincode::serialized_size(refs).map_err(|_| invalid("delivery encoding"))?,
                    )
                    .saturating_add(48);
            }
            let retire_count = if *awaiting {
                refs.len().saturating_sub(1)
            } else {
                refs.len()
            };
            fanout_bytes =
                fanout_bytes.saturating_add(retire_size.saturating_mul(retire_count as u64));
        }
        size = size
            .saturating_add(future_bytes)
            .saturating_add(fanout_bytes);
        // Every live card reserves permanent erasure and each chunk destination
        // reserves retirement. Tombstones never expire or get evicted.
        size = size
            .saturating_add((file_debt as u64).saturating_mul(100))
            .saturating_add((completion_debt as u64).saturating_mul(100))
            .saturating_add((work_debt as u64).saturating_mul(100));
        if let OutboxRecord::FileCard(file) = record {
            size = size.saturating_add(
                100 + (file.destinations.len() as u64).saturating_mul(100)
                    + u64::from(file.completion_binding.is_some()) * 100,
            );
        }
        let max_records = self.limits.max_outbox_records();
        if self.outbox_path.metadata()?.len().saturating_add(size) <= self.limits.outbox_bytes
            && self.outbox_records.saturating_add(reserved_records) <= max_records
        {
            return Ok(());
        }
        let mut snapshot = Vec::new();
        snapshot.extend(self.files.values().cloned().map(OutboxRecord::FileCard));
        snapshot.extend(
            self.file_completions
                .iter()
                .map(|(id, final_chunk)| OutboxRecord::FileComplete(*id, *final_chunk)),
        );
        snapshot.extend(
            self.erased_files
                .iter()
                .map(|(conv, id)| OutboxRecord::ErasedFile(ConversationId::new(*conv), *id)),
        );
        snapshot.extend(self.messages.values().cloned().map(OutboxRecord::Message));
        snapshot.extend(
            self.completed
                .values()
                .cloned()
                .map(OutboxRecord::Delivered),
        );
        snapshot.extend(self.completed_receipts.values().map(|r| {
            self.completed_receipt_devices
                .get(&r.original_event_id)
                .map_or_else(
                    || OutboxRecord::FinishedReceipt(r.clone()),
                    |device| OutboxRecord::FinishedBoundReceipt(r.clone(), device.clone()),
                )
        }));
        snapshot.extend(
            self.receipts
                .values()
                .cloned()
                .map(Box::new)
                .map(OutboxRecord::Receipt),
        );
        let mut compact_size = HEADER_BYTES;
        for record in &snapshot {
            compact_size = compact_size.saturating_add(
                bincode::serialized_size(record)
                    .map_err(|_| invalid("delivery encoding"))?
                    .saturating_add(FRAME_OVERHEAD),
            );
        }
        if compact_size.saturating_add(size) > self.limits.outbox_bytes
            || snapshot.len().saturating_add(reserved_records) > max_records
        {
            return Err(invalid("delivery outbox capacity exhausted"));
        }
        self.outbox.rewrite(&snapshot)?;
        self.outbox_records = snapshot.len();
        self.needs_sync = true;
        self.sync_replacement()
    }

    fn check_counts(&self, additional: Option<&DeliveryTransaction>) -> Result<(), LogError> {
        let mut messages: HashSet<EventId> = self.messages.keys().copied().collect();
        let mut receipts: HashSet<EventId> = self.receipts.keys().copied().collect();
        let mut file_slots: HashSet<EventId> = self.files.keys().copied().collect();
        let mut file_work: HashSet<EventId> = self.active_files.iter().map(|(id, _)| *id).collect();
        for tx in self.pending.iter().chain(additional) {
            if let DeliveryTransaction::OutgoingManifest { file, .. }
            | DeliveryTransaction::IncomingManifest { file, .. } = tx
            {
                file_slots.insert(file.id);
                if file.destinations.iter().any(|d| d.active) {
                    file_work.insert(file.id);
                }
            }
            match tx {
                DeliveryTransaction::Outgoing { message, .. }
                | DeliveryTransaction::OutgoingManifest { message, .. } => {
                    messages.insert(message.logical_id);
                }
                DeliveryTransaction::Incoming {
                    receipt: Some(receipt),
                    ..
                }
                | DeliveryTransaction::IncomingManifest {
                    receipt: Some(receipt),
                    ..
                } => {
                    receipts.insert(receipt.destination.event.id);
                }
                _ => {}
            }
        }
        let completed_work = self
            .completed
            .values()
            .filter(|m| !m.remaining.is_empty())
            .count();
        let mut work_ids = messages.clone();
        work_ids.extend(
            self.completed
                .values()
                .filter(|m| !m.remaining.is_empty())
                .map(|m| m.logical_id),
        );
        work_ids.extend(file_work);
        if messages.len().saturating_add(completed_work) > self.limits.messages
            || work_ids.len() > self.limits.messages
            || file_slots.len().saturating_add(self.erased_files.len())
                > self.limits.completed_receipts
            || receipts.len() > self.limits.receipts
            || messages.len().saturating_add(self.completed.len()) > self.limits.completed
            || receipts.len().saturating_add(self.completed_receipts.len())
                > self.limits.completed_receipts
            || self.pending.len() + usize::from(additional.is_some()) > self.limits.transactions
        {
            return Err(invalid("delivery capacity exhausted"));
        }
        Ok(())
    }

    fn validate_record(&self, record: &OutboxRecord) -> Result<(), LogError> {
        match record {
            OutboxRecord::FileCard(file) => {
                let mut ids = HashSet::new();
                let mut devices = HashSet::new();
                if file.file_conversation == file.conversation
                    || file.completion_binding.as_ref().is_some_and(|binding| {
                        !valid_account(&binding.owner_account)
                            || binding.certificate.as_ref().is_some_and(|cert| {
                                !cert.verify()
                                    || cert.device_ed25519_pub != binding.source.ed25519_pub
                            })
                    })
                    || file.chunk_count == 0
                    || file.destinations.len() > self.limits.destinations
                    || (!file.destinations.is_empty() && file.final_chunk.is_none())
                    || file.destinations.iter().any(|d| {
                        !ids.insert(d.binding.event_id)
                            || !devices.insert(d.binding.device.ed25519_pub)
                            || d.binding
                                .account
                                .as_deref()
                                .is_some_and(|a| !valid_account(a))
                    })
                {
                    return Err(invalid("invalid immutable file metadata"));
                }
                Ok(())
            }
            OutboxRecord::Message(message) => self.validate_message(message),
            OutboxRecord::Receipt(receipt) => self.validate_destination(&receipt.destination),
            OutboxRecord::Delivered(completed) => {
                let mut ids = HashSet::new();
                if completed.remaining.len() > self.limits.destinations
                    || completed
                        .recipient_account
                        .as_deref()
                        .is_some_and(|a| !valid_account(a))
                    || completed.remaining.iter().any(|r| {
                        !ids.insert(r.event_id)
                            || r.account.as_deref().is_some_and(|a| !valid_account(a))
                            || (r.receipt_eligible
                                && (r.account.is_none()
                                    || r.account != completed.recipient_account))
                    })
                {
                    return Err(invalid("invalid remaining fanout references"));
                }
                Ok(())
            }
            OutboxRecord::FinishedReceipt(receipt)
            | OutboxRecord::FinishedBoundReceipt(receipt, _) => {
                if receipt
                    .destination_account
                    .as_deref()
                    .is_none_or(|a| !valid_account(a))
                {
                    return Err(invalid("invalid completed receipt binding"));
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn validate_destination(&self, destination: &DeliveryDestination) -> Result<(), LogError> {
        if !matches!(
            destination.event.kind,
            EventKind::Message | EventKind::FileManifest
        ) || destination.event.ciphertext.len() > self.limits.payload_bytes
            || destination.event.parents.len() > 1024
            || !destination.event.verify_integrity()
            || !destination.event.verify_signature()
            || destination
                .account
                .as_deref()
                .is_some_and(|a| !valid_account(a))
            || (destination.receipt_eligible && destination.account.is_none())
        {
            return Err(invalid("invalid immutable delivery event"));
        }
        Ok(())
    }

    fn validate_message(&self, message: &OutgoingDelivery) -> Result<(), LogError> {
        if !valid_account(&message.sender_account)
            || message
                .recipient_account
                .as_deref()
                .is_some_and(|a| !valid_account(a))
            || message.destinations.is_empty()
            || message.destinations.len() > self.limits.destinations
        {
            return Err(invalid("invalid delivery destinations"));
        }
        let mut devices = HashSet::new();
        let mut events = HashSet::new();
        let first_author = message.destinations[0].event.author;
        for destination in &message.destinations {
            self.validate_destination(destination)?;
            let author = destination.event.author.ed25519_pub();
            let mut pair = [*author, destination.device.ed25519_pub];
            pair.sort();
            let expected = super::conversation::dm_conversation_id(
                &PublicIdentity {
                    ed25519_pub: pair[0],
                    x25519_pub: [0; 32],
                },
                &PublicIdentity {
                    ed25519_pub: pair[1],
                    x25519_pub: [0; 32],
                },
            );
            if destination.event.author != first_author
                || *author == destination.device.ed25519_pub
                || destination.event.conversation_id != expected
                || !devices.insert(destination.device.ed25519_pub)
                || !events.insert(destination.event.id)
                || destination.event.wall_clock != message.wall_clock
                || (destination.receipt_eligible
                    && destination.account != message.recipient_account)
                || (destination.account.as_deref() == Some(message.sender_account.as_str())
                    && destination.receipt_eligible)
                || (message.recipient_account.is_some()
                    && destination.account != message.recipient_account
                    && destination.account.as_deref() != Some(message.sender_account.as_str()))
            {
                return Err(invalid("invalid delivery event binding"));
            }
        }
        Ok(())
    }

    fn validate_transaction(&self, tx: &DeliveryTransaction) -> Result<(), LogError> {
        match tx {
            DeliveryTransaction::OutgoingManifest {
                message,
                received,
                file,
            } => {
                self.validate_message(message)?;
                self.validate_record(&OutboxRecord::FileCard(file.clone()))?;
                let manifest = super::files::validated_manifest(&received.plaintext)
                    .ok_or_else(|| invalid("invalid file manifest"))?;
                if message.logical_id != message.destinations[0].event.id
                    || file.completion_binding.is_some()
                    || file.id != message.logical_id
                    || file.conversation != message.conversation
                    || file.wall_clock != message.wall_clock
                    || file.file_conversation != manifest.file_conv()
                    || file.chunk_count != manifest.chunk_count()
                    || file.destinations.len() != message.destinations.len()
                    || file
                        .destinations
                        .iter()
                        .zip(&message.destinations)
                        .any(|(f, d)| {
                            !f.active
                                || f.binding.event_id != d.event.id
                                || f.binding.device != d.device
                                || f.binding.account != d.account
                                || f.binding.receipt_eligible != d.receipt_eligible
                        })
                    || received.event_id != message.logical_id
                    || received.conversation != message.conversation
                    || received.wall_clock != message.wall_clock
                    || received.from != message.destinations[0].event.author.user_id()
                    || message.destinations.iter().any(|d| {
                        d.event.kind != EventKind::FileManifest
                            || !super::session::event_fits_frame(&d.event)
                    })
                    || received.plaintext.len() > self.limits.payload_bytes
                    || super::files::validated_manifest(&received.plaintext).is_none()
                {
                    return Err(invalid("invalid outgoing manifest transaction"));
                }
                Ok(())
            }
            DeliveryTransaction::IncomingManifest {
                sender,
                original,
                received,
                receipt,
                file,
            } => {
                self.validate_record(&OutboxRecord::FileCard(file.clone()))?;
                let manifest = super::files::validated_manifest(&received.plaintext)
                    .ok_or_else(|| invalid("invalid file manifest"))?;
                if original.kind != EventKind::FileManifest
                    || file.completion_binding.as_ref().is_none_or(|binding| {
                        binding.source != *sender
                            || self
                                .profile
                                .as_ref()
                                .is_some_and(|(_, owner)| binding.owner_account != owner.account)
                    })
                    || file.id != original.id
                    || file.conversation != received.conversation
                    || file.wall_clock != received.wall_clock
                    || file.file_conversation != manifest.file_conv()
                    || file.chunk_count != manifest.chunk_count()
                    || !file.destinations.is_empty()
                    || file.final_chunk.is_some()
                    || !original.verify_integrity()
                    || !original.verify_signature()
                    || original.author.ed25519_pub() != &sender.ed25519_pub
                    || !super::session::event_fits_frame(original)
                    || received.event_id != original.id
                    || received.wall_clock != original.wall_clock
                    || received.from != sender.user_id()
                    || received.plaintext.len() > self.limits.payload_bytes
                    || super::files::validated_manifest(&received.plaintext).is_none()
                {
                    return Err(invalid("invalid incoming manifest transaction"));
                }
                if let Some(receipt) = receipt {
                    self.validate_destination(&receipt.destination)?;
                    if receipt.destination.event.kind != EventKind::Message
                        || receipt.original_event_id != original.id
                        || receipt.logical_id != original.id
                        || receipt.destination.device != *sender
                        || receipt.destination.receipt_eligible
                        || receipt.destination.account.is_none()
                        || receipt.conversation != received.conversation
                        || receipt.wall_clock != received.wall_clock
                    {
                        return Err(invalid("invalid manifest receipt binding"));
                    }
                }
                Ok(())
            }
            DeliveryTransaction::Outgoing {
                message,
                sent,
                ratchets,
            } => {
                self.validate_message(message)?;
                if message
                    .destinations
                    .iter()
                    .any(|d| d.event.kind != EventKind::Message)
                {
                    return Err(invalid("invalid message transaction kind"));
                }
                if sent.conversation != message.conversation
                    || sent.wall_clock != message.wall_clock
                    || sent.plaintext.len() > self.limits.payload_bytes
                    || ratchets.len() != message.destinations.len()
                {
                    return Err(invalid("invalid outgoing transaction"));
                }
                if let Some(envelope) = DmEnvelope::decode(&sent.plaintext) {
                    if envelope.msg_id != *message.logical_id.as_bytes()
                        || envelope.route.sender_account != message.sender_account
                        || Some(envelope.route.recipient_account.as_str())
                            != message.recipient_account.as_deref()
                        || message.conversation
                            != super::conversation::account_conversation_id(
                                &envelope.route.sender_account,
                                &envelope.route.recipient_account,
                            )
                    {
                        return Err(invalid("invalid outgoing logical binding"));
                    }
                } else if message.destinations.len() != 1
                    || message.logical_id != message.destinations[0].event.id
                    || message.conversation != message.destinations[0].event.conversation_id
                    || sent.seq != message.destinations[0].event.seq
                {
                    return Err(invalid("invalid legacy delivery binding"));
                }
                for (prepared, destination) in ratchets.iter().zip(&message.destinations) {
                    validate_prepared(prepared, &destination.device, self.limits.payload_bytes)?;
                }
                Ok(())
            }
            DeliveryTransaction::Incoming {
                sender,
                original,
                ratchet,
                received,
                receipt,
            } => {
                if original.kind != EventKind::Message
                    || !original.verify_integrity()
                    || !original.verify_signature()
                    || original.author.ed25519_pub() != &sender.ed25519_pub
                    || original.ciphertext.len() > self.limits.payload_bytes
                    || original.parents.len() > 1024
                    || received.event_id != original.id
                    || received.wall_clock != original.wall_clock
                    || received.plaintext.len() > self.limits.payload_bytes
                    || received.from.len() > 64
                {
                    return Err(invalid("invalid incoming transaction"));
                }
                validate_prepared(ratchet, sender, self.limits.payload_bytes)?;
                let logical_id = match DmEnvelope::decode(&received.plaintext) {
                    Some(envelope) => {
                        if received.from != envelope.route.sender_account
                            || received.conversation
                                != super::conversation::account_conversation_id(
                                    &envelope.route.sender_account,
                                    &envelope.route.recipient_account,
                                )
                        {
                            return Err(invalid("invalid incoming account history binding"));
                        }
                        EventId::new(envelope.msg_id)
                    }
                    None => {
                        if received.from != original.author.user_id()
                            || received.conversation != original.conversation_id
                        {
                            return Err(invalid("invalid incoming legacy history binding"));
                        }
                        original.id
                    }
                };
                if let Some(receipt) = receipt {
                    self.validate_destination(&receipt.destination)?;
                    if receipt.original_event_id != original.id
                        || receipt.logical_id != logical_id
                        || receipt.destination.device != *sender
                        || receipt.destination.receipt_eligible
                        || receipt.destination.account.is_none()
                        || receipt.destination.event.conversation_id == original.conversation_id
                        || receipt.conversation != received.conversation
                        || receipt.wall_clock != received.wall_clock
                    {
                        return Err(invalid("invalid receipt transaction binding"));
                    }
                }
                Ok(())
            }
        }
    }
}

fn metadata_for(tx: &DeliveryTransaction) -> Option<OutboxRecord> {
    match tx {
        DeliveryTransaction::Outgoing { message, .. }
        | DeliveryTransaction::OutgoingManifest { message, .. } => {
            Some(OutboxRecord::Message(message.clone()))
        }
        DeliveryTransaction::Incoming { receipt, .. }
        | DeliveryTransaction::IncomingManifest { receipt, .. } => {
            receipt.clone().map(OutboxRecord::Receipt)
        }
    }
}

fn same_metadata_key(a: &OutboxRecord, b: &OutboxRecord) -> bool {
    match (a, b) {
        (OutboxRecord::Message(a), OutboxRecord::Message(b)) => a.logical_id == b.logical_id,
        (OutboxRecord::Receipt(a), OutboxRecord::Receipt(b)) => {
            a.destination.event.id == b.destination.event.id
        }
        _ => false,
    }
}

fn valid_account(account: &str) -> bool {
    account.len() == 32 && account.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_prepared(
    prepared: &PreparedRatchet,
    device: &PublicIdentity,
    max_bytes: usize,
) -> Result<(), LogError> {
    if prepared.peer != device.user_id()
        || prepared.state.len() > max_bytes
        || crate::ratchet::RatchetState::deserialize(&prepared.state).is_none()
    {
        return Err(invalid("invalid delivery ratchet transition"));
    }
    Ok(())
}

fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.to_owned())
}

/// Bound bytes and record count before generic replay allocates/decrypts. This is
/// a trusted profile file owned by a single Node, not a concurrently writable path.
fn preflight(path: &Path, max_bytes: u64, max_records: usize) -> Result<(), LogError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let bytes = file.metadata()?.len();
    if bytes > max_bytes {
        return Err(invalid("delivery file exceeds capacity"));
    }
    let mut header = [0u8; 22];
    if file.read_exact(&mut header).is_err() {
        return Err(invalid("delivery file missing header"));
    }
    let mut consumed = HEADER_BYTES;
    let mut records = 0;
    while consumed.saturating_add(4) <= bytes {
        let mut prefix = [0; 4];
        file.read_exact(&mut prefix)?;
        let length = u32::from_be_bytes(prefix) as u64;
        consumed = consumed.saturating_add(4);
        if consumed.saturating_add(length) > bytes {
            break;
        } // recoverable torn tail
        records += 1;
        if records > max_records {
            return Err(invalid("delivery replay record capacity"));
        }
        std::io::copy(&mut file.by_ref().take(length), &mut std::io::sink())?;
        consumed += length;
    }
    Ok(())
}

#[cfg(test)]
#[path = "delivery_store_tests.rs"]
mod tests;

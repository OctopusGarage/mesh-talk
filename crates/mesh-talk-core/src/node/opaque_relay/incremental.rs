//! Bounded mailbox replication. Cursors record transport progress, never delivery.
use super::*;
use crate::node::work_observation::reason;

mod checkpoints;
mod confirmation;
use crate::node::{
    privacy_sync::{HistorySyncStamp, HistoryUpload},
    session::{self, mailbox},
};
use crate::postoffice::{MailboxCursor, MailboxDeltaPage};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const MAX_VIEWS: usize = 4;
const MAX_PENDING: usize = 256;
const MAX_RESULTS: usize = 64;
// Four entry owners, IDs only; independent of the 16-KiB receipt envelope.
const MAX_REPAIR_HINT_BYTES: usize = 2 * 1024 * 1024;
const PAGE_BYTES: usize = crate::transport::MAX_PLAINTEXT - 2048;

#[cfg(test)]
thread_local! {
    pub(super) static HISTORY_METADATA_ROWS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Default)]
pub(in crate::node) struct UploadCache {
    entries: BTreeMap<Vec<u8>, Arc<Mutex<UploadEntry>>>,
}
struct UploadEntry {
    busy: bool,
    used: std::time::Instant,
    view: Option<HistoryUpload>,
    rank: i64,
    complete: bool,
    stamp: Option<HistorySyncStamp>,
    last: Option<(EventId, EventId)>,
    receipt: Option<Receipt>,
    repair_hints: BTreeMap<EventId, EventId>,
    repair_declarations: HashSet<EventId>,
    repair_lease: Option<super::super::node::ProfileCompactionLease>,
    repair_generation: u64,
    repair_progress: std::time::Instant,
}
impl UploadEntry {
    fn remember_repair_hint(
        &mut self,
        inner: EventId,
        outer: EventId,
    ) -> Result<(), session::SessionError> {
        let bytes = self.repair_hints.len() * 64 + self.repair_declarations.len() * 32;
        if !self.repair_hints.contains_key(&inner) && bytes + 64 > MAX_REPAIR_HINT_BYTES {
            return Err(session::SessionError::ResourceUnavailable);
        }
        self.repair_hints.insert(inner, outer);
        Ok(())
    }
    fn remember_repair_declaration(
        &mut self,
        inner: EventId,
    ) -> Result<bool, session::SessionError> {
        let bytes = self.repair_hints.len() * 64 + self.repair_declarations.len() * 32;
        if !self.repair_declarations.contains(&inner) && bytes + 32 > MAX_REPAIR_HINT_BYTES {
            return Err(session::SessionError::ResourceUnavailable);
        }
        Ok(self.repair_declarations.insert(inner))
    }
}
struct Lease(Arc<Mutex<UploadEntry>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.lock().expect("upload entry").busy = false;
    }
}
impl UploadCache {
    pub(in crate::node) fn observed_len(&self) -> usize {
        self.entries.len()
    }

    pub(in crate::node) fn clear_receive_repair_leases(&mut self) {
        for entry in self.entries.values() {
            let mut entry = entry.lock().expect("upload entry");
            entry.repair_lease = None;
            entry.repair_declarations.clear();
            if let Some(receipt) = entry.receipt.as_mut() {
                receipt.repair = None;
            }
        }
    }
    pub(in crate::node) fn clear_stale_receive_repair_leases(&mut self, generation: u64) {
        for entry in self.entries.values() {
            let mut entry = entry.lock().expect("upload entry");
            if entry.repair_generation != generation
                || entry.repair_progress.elapsed() > std::time::Duration::from_secs(30)
            {
                entry.repair_lease = None;
                entry.repair_declarations.clear();
                if let Some(repair) = entry
                    .receipt
                    .as_mut()
                    .and_then(|receipt| receipt.repair.as_mut())
                    && repair.active
                {
                    // Protection was abandoned: earlier raw parents may now be
                    // compacted. Rewind this runtime's derived scan conservatively.
                    repair.scan = repair.captured.clone();
                    let upper = repair
                        .scan
                        .window
                        .unwrap_or((repair.scan.after, repair.scan.revision));
                    repair.scan.after = 0;
                    repair.scan.window = Some(upper);
                    repair.page = None;
                    repair.scan_complete = false;
                    repair.spent.fill(0);
                }
                if entry.repair_generation != generation
                    && let Some(repair) = entry
                        .receipt
                        .as_mut()
                        .and_then(|receipt| receipt.repair.as_mut())
                {
                    repair.spent.fill(0);
                }
            }
        }
    }
    fn acquire(&mut self, key: &[u8]) -> Result<Lease, session::SessionError> {
        if !self.entries.contains_key(key) {
            if self.entries.len() >= MAX_VIEWS {
                let oldest = self
                    .entries
                    .iter()
                    .filter_map(|(k, e)| {
                        let e = e.lock().expect("upload entry");
                        (!e.busy).then_some((k.clone(), e.used))
                    })
                    .min_by_key(|(_, t)| *t)
                    .map(|(k, _)| k)
                    .ok_or(session::SessionError::ResourceUnavailable)?;
                self.entries.remove(&oldest);
            }
            self.entries.insert(
                key.to_vec(),
                Arc::new(Mutex::new(UploadEntry {
                    busy: false,
                    used: std::time::Instant::now(),
                    view: None,
                    rank: -1,
                    complete: false,
                    stamp: None,
                    last: None,
                    receipt: None,
                    repair_hints: BTreeMap::new(),
                    repair_declarations: HashSet::new(),
                    repair_lease: None,
                    repair_generation: 0,
                    repair_progress: std::time::Instant::now(),
                })),
            );
        }
        let entry = self
            .entries
            .get(key)
            .expect("installed upload entry")
            .clone();
        {
            let mut e = entry.lock().expect("upload entry");
            if e.busy {
                return Err(session::SessionError::ResourceUnavailable);
            }
            e.busy = true;
            e.used = std::time::Instant::now();
        }
        Ok(Lease(entry))
    }
}
#[derive(Default)]
pub(super) enum Mode {
    #[default]
    Unknown,
    Legacy,
    Active(Box<State>),
}
pub(super) struct State {
    key: Vec<u8>,
    lease: Lease,
    receipt: Receipt,
    saved: Option<Receipt>,
    page: Option<MailboxDeltaPage>,
    incoming: VecDeque<EventId>,
    remote_done: bool,
    retry: Option<VecDeque<EventId>>,
    history_checked: bool,
    history_done: bool,
    history_ids: VecDeque<EventId>,
    history_after: i64,
    history_complete: bool,
    controls: Option<VecDeque<SelectedOpaqueControl>>,
    wrote: bool,
    final_probe: bool,
    remote: Option<RemoteBinding>,
    remote_floor: Option<(i64, i64)>,
    repair_generation: Option<u64>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RemoteBinding {
    store_uuid: [u8; 16],
    conversation: ConversationId,
    epoch: i64,
    first_seen: Option<u64>,
}
impl From<&MailboxCursor> for RemoteBinding {
    fn from(cursor: &MailboxCursor) -> Self {
        Self {
            store_uuid: cursor.store_uuid,
            conversation: cursor.conversation,
            epoch: cursor.epoch,
            first_seen: cursor.first_seen,
        }
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CompletedUpload {
    local: HistorySyncStamp,
    remote: RemoteBinding,
    upper: i64,
    revision: i64,
    targets: Vec<(EventId, EventId)>,
}
impl CompletedUpload {
    fn accepts(&self, remote: &RemoteBinding, (upper, revision): (i64, i64)) -> bool {
        self.remote == *remote && self.upper <= upper && self.revision <= revision
    }

    fn remember_target(&mut self, inner: EventId, outer: EventId) {
        if let Some(index) = self.targets.iter().position(|(id, _)| *id == inner) {
            if self.targets[index].1 == outer {
                return;
            }
            self.targets.remove(index);
        }
        if self.targets.len() >= MAX_RESULTS {
            self.targets.remove(0);
        }
        self.targets.push((inner, outer));
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    cursor: Option<MailboxCursor>,
    pending: Vec<EventId>,
    epoch: i64,
    source_floor: i64,
    runtime: [u8; 16],
    session_only: bool,
    completed: Option<CompletedUpload>,
    repair: Option<Repair>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Repair {
    captured: MailboxCursor,
    witness: [u8; 32],
    active: bool,
    scan: MailboxCursor,
    page: Option<RepairPage>,
    rewind: bool,
    focus: Option<(EventId, EventId)>,
    conversation: ConversationId,
    local_epoch: i64,
    global_epoch: Option<i64>,
    owner: EventId,
    spent: [u8; 32],
    scan_complete: bool,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RepairPage {
    cursor: MailboxCursor,
    ids: Vec<EventId>,
    next: usize,
    complete: bool,
}
fn pending_repair_witness(pending: &[EventId]) -> ([u8; 32], Vec<EventId>) {
    use sha2::{Digest, Sha256};
    let mut sorted = pending.to_vec();
    sorted.sort();
    let mut hash = Sha256::new();
    hash.update(b"MeshTalkMailboxRepairV1\0");
    for id in &sorted {
        hash.update(id.as_bytes());
    }
    (hash.finalize().into(), sorted)
}
impl Repair {
    fn spent_for(&self, pending: &[EventId], child: EventId) -> bool {
        pending
            .binary_search(&child)
            .is_ok_and(|position| self.spent[position / 8] & (1_u8 << (position % 8)) != 0)
    }
    fn mark_spent(&mut self, pending: &[EventId], child: EventId) {
        if let Ok(position) = pending.binary_search(&child) {
            self.spent[position / 8] |= 1_u8 << (position % 8);
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Sealed {
    body: Vec<u8>,
    signature: Vec<u8>,
}
fn state(p: &OpaqueMailboxProgress) -> &State {
    match &p.incremental {
        Mode::Active(s) => s,
        _ => unreachable!("negotiated mailbox"),
    }
}
fn state_mut(p: &mut OpaqueMailboxProgress) -> &mut State {
    match &mut p.incremental {
        Mode::Active(s) => s,
        _ => unreachable!("negotiated mailbox"),
    }
}
fn cache_key(p: &OpaqueMailboxProgress) -> Result<Vec<u8>, session::SessionError> {
    bincode::serialize(&(
        p.relay.clone(),
        p.recipient.clone(),
        p.mailbox.id(),
        p.conversation,
    ))
    .map_err(|e| session::SessionError::Serialization(e.to_string()))
}
impl Node {
    fn record_mailbox_result(&self, p: &mut OpaqueMailboxProgress, inner: EventId, outer: EventId) {
        if p.uploaded.len() >= MAX_RESULTS {
            p.uploaded.remove(0);
        }
        p.uploaded.push((inner, outer));
        state(p).lease.0.lock().expect("upload entry").last = Some((inner, outer));
        self.opaque_receive_cache
            .lock()
            .expect("opaque cache")
            .remember(p.mailbox.id(), &[(outer, inner)]);
    }
    fn upload_permitted<F: Fn() -> bool + Sync>(
        &self,
        p: &OpaqueMailboxProgress,
        current: &F,
    ) -> bool {
        current() && self.relay_send_allowed(&p.relay)
    }
    fn validate_mailbox_history(&self, p: &OpaqueMailboxProgress) -> Result<(), LogError> {
        let entry = state(p).lease.0.lock().expect("upload entry");
        if let Some(view) = &entry.view {
            self.sync_store(&p.recipient)
                .lock()
                .expect("projection")
                .validate_history_upload(view)?;
        }
        Ok(())
    }
    async fn send_mailbox_event<IO: AsyncRead + AsyncWrite + Unpin, F: Fn() -> bool + Sync>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        event: Event,
        current: &F,
        history: bool,
    ) -> Result<Option<EventId>, session::SessionError> {
        if !self.upload_permitted(p, current) {
            return Err(session::SessionError::UnexpectedMessage);
        }
        if history {
            self.validate_mailbox_history(p)?;
        }
        let id = event.id;
        if let Some(outer) = self.cached_wrapper_id(p.mailbox.id(), id)
            && session::request_durable_have(channel, p.mailbox.id(), outer).await?
        {
            if history {
                self.validate_mailbox_history(p)?;
            }
            if !self.upload_permitted(p, current) {
                return Err(session::SessionError::UnexpectedMessage);
            }
            return Ok(Some(outer));
        }
        let wrapper = p
            .mailbox
            .wrap(&event)
            .map_err(|e| session::SessionError::Serialization(e.to_string()))?;
        if !self.upload_permitted(p, current) {
            return Err(session::SessionError::UnexpectedMessage);
        }
        if history {
            self.validate_mailbox_history(p)?;
        }
        let outer = wrapper.id;
        if !mailbox::upload(channel, wrapper).await? {
            return Err(session::SessionError::NoProgress);
        }
        if history {
            self.validate_mailbox_history(p)?;
        }
        if !self.upload_permitted(p, current) {
            return Err(session::SessionError::UnexpectedMessage);
        }
        state_mut(p).wrote = true;
        self.remember_wrapper_id(p.mailbox.id(), id, outer);
        Ok(Some(outer))
    }
    async fn incremental_receive_one<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        id: EventId,
        retry: bool,
    ) -> Result<(), session::SessionError> {
        if !self.relay_receive_allowed(&p.relay) {
            return Err(
                LogError::AdmissionRejected("mailbox receive permission revoked".into()).into(),
            );
        }
        let retained = {
            let cache = self.opaque_receive_cache.lock().expect("opaque cache");
            let log = self.log.lock().expect("log lock");
            cache
                .retained_seen_selected(p.mailbox.id(), &[id], &log)
                .unwrap_or_default()
                .contains(&id)
        };
        if retained {
            self.observe_mailbox_work(reason::RETAINED_HIT, None);
        }
        let semantic = !retained
            && p.required_parent.is_none()
            && (self.semantic_profile_seen(p.mailbox.id(), id)?
                || self.consent_transport_seen(p.mailbox.id(), id)?);
        if semantic {
            self.observe_mailbox_work(reason::SEMANTIC_HIT, None);
        }
        if !retained && !semantic {
            let seen = {
                let cache = self.opaque_receive_cache.lock().expect("opaque cache");
                let log = self.log.lock().expect("log lock");
                cache
                    .seen_selected(p.mailbox.id(), &[id], &log)
                    .unwrap_or_default()
                    .contains(&id)
            };
            if seen && !retry {
                self.observe_mailbox_work(reason::SESSION_HIT, None);
                state_mut(p).receipt.session_only = true;
            } else {
                let cursor = state(p)
                    .page
                    .as_ref()
                    .map(|page| page.cursor.clone())
                    .or_else(|| state(p).receipt.cursor.clone())
                    .ok_or(session::SessionError::UnexpectedMessage)?;
                self.observe_mailbox_work(
                    if retry {
                        reason::GET_RETRY
                    } else {
                        reason::GET
                    },
                    None,
                );
                let events = mailbox::get(channel, p.mailbox.id(), &cursor, &[id]).await?;
                // A policy change can arrive while the authenticated GET is in
                // flight. Keep this ID queued without admitting its response.
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                let child = events.first().cloned();
                self.accept_opaque_wrappers(p, events)?;
                if let Some((outer, missing)) = p.missing_parents.take()
                    && outer == id
                    && let Some(child) = child
                {
                    self.recover_mailbox_parents(channel, p, child, cursor, missing)
                        .await?;
                }
                if let Some(inner) = p.processed_events.get(&id) {
                    if inner.is_none() {
                        state_mut(p).receipt.session_only = true;
                    }
                    p.deferred_events.remove(&id);
                } else if !p.deferred_events.contains(&id) {
                    return Err(session::SessionError::NoProgress);
                }
            }
        }
        if !self.relay_receive_allowed(&p.relay) {
            return Err(
                LogError::AdmissionRejected("mailbox receive permission revoked".into()).into(),
            );
        }
        let failed = p.deferred_events.contains(&id);
        let pending = &mut state_mut(p).receipt.pending;
        if failed {
            if !pending.contains(&id) {
                if pending.len() >= MAX_PENDING {
                    return Err(session::SessionError::ResourceUnavailable);
                }
                pending.push(id);
            }
        } else {
            pending.retain(|outer| *outer != id);
        }
        if !failed
            && state(p)
                .receipt
                .repair
                .as_ref()
                .is_some_and(|repair| repair.active && repair.owner == id)
        {
            self.finish_mailbox_repair(p)?;
        }
        p.processed_events.clear();
        Ok(())
    }
    fn validate_mailbox_repair(&self, p: &OpaqueMailboxProgress) -> Result<(), LogError> {
        if let Some(generation) = state(p).repair_generation {
            if self.privacy.generation.load(Ordering::Acquire) != generation {
                return Err(LogError::StaleCursor);
            }
            let entry = state(p).lease.0.lock().expect("upload entry");
            if entry.repair_generation != generation || entry.repair_lease.is_none() {
                return Err(LogError::StaleCursor);
            }
        }
        if state(p).repair_generation.is_some()
            && let Some(repair) = state(p).receipt.repair.as_ref()
            && !self.mailbox_repair_scope_valid(repair)?
        {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    fn mailbox_repair_scope_valid(&self, repair: &Repair) -> Result<bool, LogError> {
        let log = self.log.lock().expect("log lock");
        let repository = log.repository()?;
        Ok(if let Some(epoch) = repair.global_epoch {
            repository.destructive_policy_revision()? == epoch
        } else {
            repository.conversation_destructive_epoch(&repair.conversation)? == repair.local_epoch
        })
    }
    fn hold_mailbox_repair_lease(&self, p: &mut OpaqueMailboxProgress) -> Result<(), LogError> {
        let generation = self.privacy.generation.load(Ordering::Acquire);
        let needed = {
            let entry = state(p).lease.0.lock().expect("upload entry");
            entry.repair_lease.is_none() || entry.repair_generation != generation
        };
        if needed {
            let lease = self.acquire_profile_compaction_lease()?;
            let old = {
                let mut entry = state(p).lease.0.lock().expect("upload entry");
                entry.repair_generation = generation;
                entry.repair_progress = std::time::Instant::now();
                entry.repair_lease.replace(lease)
            };
            drop(old);
        }
        state_mut(p).repair_generation = Some(generation);
        self.validate_mailbox_repair(p)
    }
    fn finish_mailbox_repair(
        &self,
        p: &mut OpaqueMailboxProgress,
    ) -> Result<(), session::SessionError> {
        let (witness, pending) = pending_repair_witness(&state(p).receipt.pending);
        if let Some(repair) = state_mut(p).receipt.repair.as_mut() {
            if repair.witness != witness {
                repair.witness = witness;
                repair.spent.fill(0);
            }
            repair.mark_spent(&pending, repair.owner);
            repair.active = false;
            repair.page = None;
        }
        self.save_mailbox_receipt(p)?;
        let lease = {
            let mut entry = state(p).lease.0.lock().expect("upload entry");
            entry.repair_hints.clear();
            entry.repair_declarations.clear();
            entry.repair_lease.take()
        };
        drop(lease);
        state_mut(p).repair_generation = None;
        Ok(())
    }
    fn retry_recovery_child(
        &self,
        p: &mut OpaqueMailboxProgress,
        child: &Event,
        required: &mut HashSet<EventId>,
    ) -> Result<bool, session::SessionError> {
        self.validate_mailbox_repair(p)?;
        self.accept_opaque_wrappers(p, vec![child.clone()])?;
        self.validate_mailbox_repair(p)?;
        if p.processed_events.contains_key(&child.id) {
            p.deferred_events.remove(&child.id);
            return Ok(true);
        }
        if let Some((outer, missing)) = p.missing_parents.take()
            && outer == child.id
        {
            required.extend(missing);
        }
        Ok(false)
    }
    fn admit_recovery_wrapper(
        &self,
        p: &mut OpaqueMailboxProgress,
        wrapper: Event,
        required: &mut HashSet<EventId>,
        child: EventId,
    ) -> Result<(), session::SessionError> {
        self.validate_mailbox_repair(p)?;
        let event = match p.mailbox.open(&wrapper) {
            Ok(event) => event,
            Err(_) => {
                // An unrelated opaque wrapper has the same transport-only
                // classification as normal receive. It proves neither durable
                // admission nor custody, and cannot survive a runtime change.
                self.accept_opaque_wrappers(p, vec![wrapper])?;
                state_mut(p).receipt.session_only = true;
                return Ok(());
            }
        };
        let id = event.id;
        let outer = wrapper.id;
        let parent = required.contains(&id);
        let was_present = parent && self.log.lock().expect("log lock").has(&id)?;
        let owns_recipe = state(p)
            .receipt
            .repair
            .as_ref()
            .is_some_and(|repair| repair.active && repair.owner == child);
        if parent && event.is_canonical() && event.verify_integrity() && event.verify_signature() {
            self.remember_wrapper_id(p.mailbox.id(), id, outer);
            let mut entry = state(p).lease.0.lock().expect("upload entry");
            entry.remember_repair_hint(id, outer)?;
        }
        p.required_parent = parent.then_some(id);
        let result = self.accept_opaque_wrappers(p, vec![wrapper]);
        p.required_parent = None;
        result?;
        if let Some((failed, missing)) = p.missing_parents.take()
            && failed == outer
            && parent
        {
            if required.len().saturating_add(missing.len()) > session::MAX_HAVE_IDS_TOTAL {
                return Err(session::SessionError::ResourceUnavailable);
            }
            required.extend(missing);
            let fresh = owns_recipe && {
                let mut entry = state(p).lease.0.lock().expect("upload entry");
                entry.remember_repair_declaration(id)?
            };
            if fresh && let Some(repair) = state_mut(p).receipt.repair.as_mut() {
                // One rewind is earned by an exact newly authenticated unresolved
                // declaration, never by oscillating MissingParents subsets.
                repair.rewind = true;
                repair.focus = Some((id, outer));
            }
        }
        if parent && self.log.lock().expect("log lock").has(&id)? {
            let mut entry = state(p).lease.0.lock().expect("upload entry");
            entry.repair_hints.remove(&id);
            entry.repair_declarations.remove(&id);
            if !was_present {
                entry.repair_progress = std::time::Instant::now();
            }
        }
        #[cfg(test)]
        if parent && self.log.lock().expect("log lock").has(&id)? {
            self.trace_account_client(&p.recipient, || "mailbox_parent_recovered".into());
        }
        Ok(())
    }

    async fn recover_known_mailbox_parents<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        child: &Event,
        cursor: &MailboxCursor,
        required: &mut HashSet<EventId>,
        allow_cached: bool,
    ) -> Result<bool, session::SessionError> {
        let mut attempted = HashSet::new();
        loop {
            self.validate_mailbox_repair(p)?;
            if required.len() > session::MAX_HAVE_IDS_TOTAL {
                return Err(session::SessionError::ResourceUnavailable);
            }
            let before = required.len();
            let mut installed = false;
            let candidates: Vec<_> = required.iter().copied().collect();
            for inner in candidates {
                if attempted.contains(&inner) {
                    continue;
                }
                if self.log.lock().expect("log lock").has(&inner)? {
                    required.remove(&inner);
                    continue;
                }
                let hot = state(p)
                    .lease
                    .0
                    .lock()
                    .expect("upload entry")
                    .repair_hints
                    .get(&inner)
                    .copied();
                let outer = p.wrapper_ids.get(&inner).copied().or(hot).or_else(|| {
                    allow_cached
                        .then(|| self.cached_wrapper_id(p.mailbox.id(), inner))
                        .flatten()
                });
                let Some(outer) = outer else {
                    attempted.insert(inner);
                    continue;
                };
                attempted.insert(inner);
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                if !session::request_durable_have(channel, p.mailbox.id(), outer).await? {
                    continue;
                }
                self.validate_mailbox_repair(p)?;
                let wrappers = mailbox::get(channel, p.mailbox.id(), cursor, &[outer]).await?;
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                let _step = self.acquire_profile_compaction_lease()?;
                self.validate_mailbox_repair(p)?;
                for wrapper in wrappers {
                    let event = p
                        .mailbox
                        .open(&wrapper)
                        .map_err(|error| session::SessionError::Serialization(error.to_string()))?;
                    if event.id != inner {
                        return Err(session::SessionError::UnexpectedMessage);
                    }
                    self.admit_recovery_wrapper(p, wrapper, required, child.id)?;
                }
                if self.log.lock().expect("log lock").has(&inner)? {
                    required.remove(&inner);
                    installed = true;
                    if self.retry_recovery_child(p, child, required)? {
                        return Ok(true);
                    }
                }
                tokio::task::yield_now().await;
            }
            if self.retry_recovery_child(p, child, required)? {
                return Ok(true);
            }
            if installed {
                attempted.clear();
            }
            // Every additional pass requires an installed parent or a newly
            // authenticated dependency. Rejections never create a retry loop.
            if !installed && required.len() == before {
                return Ok(false);
            }
        }
    }

    async fn recover_mailbox_parents<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        child: Event,
        cursor: MailboxCursor,
        missing: Vec<EventId>,
    ) -> Result<(), session::SessionError> {
        // One active child owns its captured page; a bitmap qualifies finite
        // failed attempts independently for every sorted durable pending ID.
        let conversation = p
            .mailbox
            .open(&child)
            .map_err(|error| session::SessionError::Serialization(error.to_string()))?
            .conversation_id;
        let changed = state(p)
            .receipt
            .repair
            .as_ref()
            .map(|repair| self.mailbox_repair_scope_valid(repair))
            .transpose()?
            .is_some_and(|valid| !valid);
        let generation_changed = state(p)
            .lease
            .0
            .lock()
            .expect("upload entry")
            .repair_generation
            != self.privacy.generation.load(Ordering::Acquire);
        if changed {
            state_mut(p).receipt.repair = None;
            state(p)
                .lease
                .0
                .lock()
                .expect("upload entry")
                .repair_declarations
                .clear();
        }
        if generation_changed && let Some(repair) = state_mut(p).receipt.repair.as_mut() {
            repair.spent.fill(0);
        }
        self.hold_mailbox_repair_lease(p)?;
        let mut required: HashSet<_> = missing.into_iter().collect();
        if !state(p).receipt.pending.contains(&child.id) {
            if state(p).receipt.pending.len() >= MAX_PENDING {
                return Err(session::SessionError::ResourceUnavailable);
            }
            state_mut(p).receipt.pending.push(child.id);
        }
        let (witness, pending) = pending_repair_witness(&state(p).receipt.pending);
        let floor = cursor.window.unwrap_or((cursor.after, cursor.revision));
        let global_epoch = if pending.len() > 1 {
            Some(
                self.log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .destructive_policy_revision()?,
            )
        } else {
            None
        };
        let remote_changed = state(p).receipt.repair.as_ref().is_some_and(|repair| {
            RemoteBinding::from(&repair.captured) != RemoteBinding::from(&cursor)
        });
        if remote_changed {
            state_mut(p).receipt.repair = None;
        }
        if let Some(repair) = state_mut(p).receipt.repair.as_mut() {
            if repair.witness != witness {
                repair.witness = witness;
                repair.spent.fill(0);
            }
            if repair.active && pending.binary_search(&repair.owner).is_err() {
                repair.active = false;
                repair.page = None;
                repair.focus = None;
            }
            if !repair.active
                && repair
                    .captured
                    .window
                    .unwrap_or((repair.captured.after, repair.captured.revision))
                    != floor
            {
                repair.spent.fill(0);
            }
            repair.global_epoch = global_epoch;
        }
        let spent = state(p)
            .receipt
            .repair
            .as_ref()
            .is_some_and(|repair| repair.spent_for(&pending, child.id));
        let active = state(p)
            .receipt
            .repair
            .as_ref()
            .is_some_and(|repair| repair.active);
        if !active && !spent {
            let previous_spent = state(p)
                .receipt
                .repair
                .as_ref()
                .map_or([0; 32], |repair| repair.spent);
            let mut scan = cursor.clone();
            scan.after = 0;
            scan.window = Some(floor);
            let epoch = self
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .conversation_destructive_epoch(&conversation)?;
            state_mut(p).receipt.repair = Some(Repair {
                captured: cursor.clone(),
                witness,
                active: true,
                scan,
                page: None,
                rewind: false,
                focus: None,
                conversation,
                local_epoch: epoch,
                global_epoch,
                owner: child.id,
                spent: previous_spent,
                scan_complete: false,
            });
            state(p)
                .lease
                .0
                .lock()
                .expect("upload entry")
                .repair_declarations
                .clear();
            self.save_mailbox_receipt(p)?;
        }
        let owns_scan = state(p)
            .receipt
            .repair
            .as_ref()
            .is_some_and(|repair| repair.active && repair.owner == child.id);
        if owns_scan
            && let Some((inner, outer)) = state(p)
                .receipt
                .repair
                .as_ref()
                .and_then(|repair| repair.focus)
        {
            required.insert(inner);
            p.wrapper_ids.insert(inner, outer);
        }
        // Probe only this authenticated child's dependencies. A nonowner never
        // advances, finishes, or spends another child's captured scan.
        if self
            .recover_known_mailbox_parents(channel, p, &child, &cursor, &mut required, true)
            .await?
        {
            if owns_scan || !active {
                self.finish_mailbox_repair(p)?;
            }
            return Ok(());
        }
        if !owns_scan {
            if !active {
                self.finish_mailbox_repair(p)?;
            }
            return Ok(());
        }
        loop {
            self.validate_mailbox_repair(p)?;
            if state(p)
                .receipt
                .repair
                .as_ref()
                .expect("repair marker")
                .scan_complete
            {
                if state(p)
                    .receipt
                    .repair
                    .as_ref()
                    .expect("repair marker")
                    .rewind
                {
                    let repair = state_mut(p).receipt.repair.as_mut().expect("repair marker");
                    repair.rewind = false;
                    repair.scan_complete = false;
                    repair.scan = repair.captured.clone();
                    let upper = repair
                        .scan
                        .window
                        .unwrap_or((repair.scan.after, repair.scan.revision));
                    repair.scan.after = 0;
                    repair.scan.window = Some(upper);
                    repair.page = None;
                    self.save_mailbox_receipt(p)?;
                    state(p)
                        .lease
                        .0
                        .lock()
                        .expect("upload entry")
                        .repair_progress = std::time::Instant::now();
                    #[cfg(test)]
                    self.trace_account_client(&p.recipient, || "mailbox_repair_rewind".into());
                    continue;
                }
                self.finish_mailbox_repair(p)?;
                return Ok(());
            }
            if !self.relay_receive_allowed(&p.relay) {
                let lease = state(p)
                    .lease
                    .0
                    .lock()
                    .expect("upload entry")
                    .repair_lease
                    .take();
                drop(lease);
                return Err(LogError::AdmissionRejected(
                    "mailbox receive permission revoked".into(),
                )
                .into());
            }
            if state(p)
                .receipt
                .repair
                .as_ref()
                .expect("repair marker")
                .page
                .is_none()
            {
                let scan = state(p)
                    .receipt
                    .repair
                    .as_ref()
                    .expect("repair marker")
                    .scan
                    .clone();
                let page = mailbox::delta(channel, p.mailbox.id(), Some(&scan)).await?;
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                self.validate_mailbox_repair(p)?;
                if page.reset {
                    return Err(LogError::StaleCursor.into());
                }
                state_mut(p)
                    .receipt
                    .repair
                    .as_mut()
                    .expect("repair marker")
                    .page = Some(RepairPage {
                    cursor: page.cursor,
                    ids: page.ids,
                    next: 0,
                    complete: page.complete,
                });
                self.save_mailbox_receipt(p)?;
            }
            let page = state(p)
                .receipt
                .repair
                .as_ref()
                .expect("repair marker")
                .page
                .as_ref()
                .expect("repair page")
                .clone();
            if let Some(outer) = page.ids.get(page.next).copied() {
                #[cfg(test)]
                self.trace_account_client(&p.recipient, || "mailbox_repair_body_admitted".into());
                let wrappers =
                    mailbox::get(channel, p.mailbox.id(), &page.cursor, &[outer]).await?;
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                let _step = self.acquire_profile_compaction_lease()?;
                self.validate_mailbox_repair(p)?;
                for wrapper in wrappers {
                    self.admit_recovery_wrapper(p, wrapper, &mut required, child.id)?;
                }
                if self.retry_recovery_child(p, &child, &mut required)? {
                    self.finish_mailbox_repair(p)?;
                    return Ok(());
                }
                p.processed_events.retain(|id, _| *id == child.id);
                state_mut(p)
                    .receipt
                    .repair
                    .as_mut()
                    .expect("repair marker")
                    .page
                    .as_mut()
                    .expect("repair page")
                    .next += 1;
                // Durable/volatile progress follows body classification, including
                // offsets inside a page slower than the exchange watchdog.
                self.save_mailbox_receipt(p)?;
                state(p)
                    .lease
                    .0
                    .lock()
                    .expect("upload entry")
                    .repair_progress = std::time::Instant::now();
                #[cfg(test)]
                self.trace_account_client(&p.recipient, || {
                    format!(
                        "mailbox_repair_frontier {} {}",
                        page.cursor.after,
                        page.next + 1
                    )
                });
                tokio::task::yield_now().await;
                continue;
            }
            {
                let repair = state_mut(p).receipt.repair.as_mut().expect("repair marker");
                repair.scan = page.cursor.clone();
                repair.page = None;
                repair.scan_complete = page.complete;
            }
            self.save_mailbox_receipt(p)?;
            if !page.complete {
                continue;
            }
            if self
                .recover_known_mailbox_parents(
                    channel,
                    p,
                    &child,
                    &page.cursor,
                    &mut required,
                    true,
                )
                .await?
            {
                self.finish_mailbox_repair(p)?;
                return Ok(());
            }
            // Finalization is now durable. A timeout in the targeted phase
            // resumes this phase rather than replaying the last page forever.
        }
    }
    pub(super) async fn advance_incremental_mailbox<IO, F>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        current: &F,
    ) -> Result<Option<bool>, session::SessionError>
    where
        IO: AsyncRead + AsyncWrite + Unpin,
        F: Fn() -> bool + Sync,
    {
        if matches!(p.incremental, Mode::Legacy) {
            return Ok(None);
        }
        // Cold metadata turns may perform no socket I/O. Give other runtime
        // work a turn between bounded steps rather than monopolizing a worker.
        tokio::task::yield_now().await;
        if matches!(p.incremental, Mode::Unknown) {
            if !mailbox::negotiate(channel, p.mailbox.id()).await? {
                p.incremental = Mode::Legacy;
                return Ok(None);
            }
            let key = cache_key(p)?;
            let lease = self
                .opaque_upload_cache
                .lock()
                .expect("upload cache")
                .acquire(&key)?;
            let hot = lease.0.lock().expect("upload entry").receipt.clone();
            let receipt =
                self.load_mailbox_receipt_with_runtime(&key, p.mailbox.id(), hot.clone())?;
            if hot.as_ref().is_some_and(|old| {
                old.epoch != receipt.epoch
                    || old.source_floor > receipt.source_floor
                    || old.runtime != receipt.runtime
            }) {
                let old_lease = {
                    let mut entry = lease.0.lock().expect("upload entry");
                    entry.receipt = None;
                    entry.repair_hints.clear();
                    entry.repair_declarations.clear();
                    entry.repair_lease.take()
                };
                drop(old_lease);
            }
            let saved = Some(receipt.clone());
            p.incremental = Mode::Active(Box::new(State {
                key,
                lease,
                receipt,
                saved,
                page: None,
                incoming: VecDeque::new(),
                remote_done: false,
                retry: None,
                history_checked: false,
                history_done: false,
                history_ids: VecDeque::new(),
                history_after: -1,
                history_complete: false,
                controls: None,
                wrote: false,
                final_probe: false,
                remote: None,
                remote_floor: None,
                repair_generation: None,
            }));
            return Ok(Some(false));
        }
        if !state(p).remote_done && state(p).page.is_none() {
            // A send-only probe observes the current finite upper, never an old
            // receive window; it does not advance the incoming receive cursor.
            let prior = if self.relay_receive_allowed(&p.relay) {
                state(p).receipt.cursor.as_ref()
            } else {
                None
            };
            let page = mailbox::delta(channel, p.mailbox.id(), prior).await?;
            let remote = RemoteBinding::from(&page.cursor);
            let floor = page
                .cursor
                .window
                .unwrap_or((page.cursor.after, page.cursor.revision));
            let own_creation = state(p).wrote
                && state(p).remote.as_ref().is_some_and(|old| {
                    old.first_seen.is_none()
                        && old.store_uuid == remote.store_uuid
                        && old.epoch == remote.epoch
                });
            let changed = state(p)
                .receipt
                .completed
                .as_ref()
                .is_some_and(|completed| !completed.accepts(&remote, floor))
                || state(p)
                    .remote
                    .as_ref()
                    .is_some_and(|old| *old != remote && !own_creation);
            if changed || (page.reset && !own_creation) {
                state_mut(p).receipt.completed = None;
                let mut entry = state(p).lease.0.lock().expect("upload entry");
                entry.rank = -1;
                entry.complete = false;
                drop(entry);
                state_mut(p).history_checked = false;
                state_mut(p).history_done = false;
                state_mut(p).history_ids.clear();
            }
            state_mut(p).remote = Some(remote);
            state_mut(p).remote_floor = Some(floor);
            if page.reset && !own_creation {
                self.observe_mailbox_work(reason::UPLOAD_RESET, None);
                state_mut(p).receipt.pending.clear();
                state_mut(p).receipt.repair = None;
                state_mut(p).repair_generation = None;
                let lease = {
                    let mut entry = state(p).lease.0.lock().expect("upload entry");
                    entry.repair_hints.clear();
                    entry.repair_declarations.clear();
                    entry.repair_lease.take()
                };
                drop(lease);
                state_mut(p).retry = None;
            }
            if self.relay_receive_allowed(&p.relay) {
                state_mut(p).incoming = page.ids.iter().copied().collect();
                state_mut(p).page = Some(page);
            } else {
                state_mut(p).remote_done = true;
            }
        }
        if let Some(id) = state(p).incoming.front().copied() {
            self.incremental_receive_one(channel, p, id, false).await?;
            state_mut(p).incoming.pop_front();
        }
        if state(p).incoming.is_empty() && state(p).page.is_some() {
            let page = state_mut(p).page.take().expect("active page");
            state_mut(p).receipt.cursor = Some(page.cursor);
            state_mut(p).remote_done = page.complete;
            self.save_mailbox_receipt(p)?;
        }
        if state(p).remote_done {
            if state(p).retry.is_none() {
                let mut retry: VecDeque<_> = state(p).receipt.pending.iter().copied().collect();
                if let Some(owner) = state(p)
                    .receipt
                    .repair
                    .as_ref()
                    .filter(|repair| repair.active)
                    .map(|repair| repair.owner)
                    && retry.contains(&owner)
                {
                    retry.retain(|id| *id != owner);
                    retry.push_front(owner);
                }
                state_mut(p).retry = Some(retry);
            }
            if let Some(id) = state(p).retry.as_ref().and_then(|ids| ids.front().copied()) {
                self.incremental_receive_one(channel, p, id, true).await?;
                state_mut(p)
                    .retry
                    .as_mut()
                    .expect("active retry queue")
                    .pop_front();
                self.save_mailbox_receipt(p)?;
            }
        }
        if state(p).controls.is_none() {
            let controls = self.opaque_controls(p, current)?;
            state_mut(p).controls = Some(
                controls
                    .values()
                    .map(SelectedOpaqueControl::from_event)
                    .collect::<Result<_, _>>()?,
            );
        }
        if let Some(selected) = state_mut(p).controls.as_mut().and_then(VecDeque::pop_front)
            && self.upload_permitted(p, current)
            && let Some(event) = self.selected_opaque_control(p, &selected, current)?
            && let Some(outer) = self
                .send_mailbox_event(channel, p, event, current, false)
                .await?
        {
            self.opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .remember_session(p.mailbox.id(), &[outer]);
        }
        self.incremental_history_step(channel, p, current).await?;
        let done = state(p).remote_done
            && state(p).retry.as_ref().is_some_and(VecDeque::is_empty)
            && state(p).history_done
            && state(p).controls.as_ref().is_some_and(VecDeque::is_empty);
        if done && state(p).wrote && !state(p).final_probe {
            state_mut(p).final_probe = true;
            state_mut(p).remote_done = false;
            state_mut(p).retry = None;
            return Ok(Some(false));
        }
        if done && self.upload_permitted(p, current) {
            let captured = {
                let entry = state(p).lease.0.lock().expect("upload entry");
                if entry.complete {
                    entry.stamp.clone()
                } else {
                    None
                }
            };
            if let (Some(local), Some(remote), Some((upper, revision)), Some(conv)) = (
                captured,
                state(p).remote.clone(),
                state(p).remote_floor,
                p.conversation,
            ) && self
                .sync_store(&p.recipient)
                .lock()
                .expect("projection")
                .history_sync_stamp(&conv)?
                == Some(local.clone())
                && !state(p)
                    .receipt
                    .completed
                    .as_ref()
                    .is_some_and(|completed| {
                        completed.local == local && completed.accepts(&remote, (upper, revision))
                    })
            {
                // Keep sufficient proof unchanged when unrelated remote data grows.
                // These bounded results came only from IDs selected by the
                // completed upload view under this exact captured stamp.
                let targets = p.uploaded.clone();
                state_mut(p).receipt.completed = Some(CompletedUpload {
                    local,
                    remote,
                    upper,
                    revision,
                    targets,
                });
            }
            self.save_mailbox_receipt(p)?;
        }
        Ok(Some(done))
    }
    async fn incremental_history_step<
        IO: AsyncRead + AsyncWrite + Unpin,
        F: Fn() -> bool + Sync,
    >(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        current: &F,
    ) -> Result<(), session::SessionError> {
        let Some(conv) = p.conversation else {
            state_mut(p).history_done = true;
            return Ok(());
        };
        if !self.upload_permitted(p, current) {
            state_mut(p).history_done = true;
            return Ok(());
        }
        if !state(p).history_checked {
            let owner = self.sync_store(&p.recipient);
            let store = owner.lock().expect("projection");
            let stamp = store.history_sync_stamp(&conv)?;
            if stamp.as_ref().is_some_and(|stamp| {
                state(p)
                    .receipt
                    .completed
                    .as_ref()
                    .is_some_and(|completed| {
                        &completed.local == stamp
                            && state(p).remote.as_ref() == Some(&completed.remote)
                    })
            }) {
                self.observe_mailbox_work(reason::VIEW_SKIP, None);
                state_mut(p).history_checked = true;
                state_mut(p).history_done = true;
                return Ok(());
            }
            let mut e = state(p).lease.0.lock().expect("upload entry");
            if e.view.is_none() {
                self.observe_mailbox_work(reason::VIEW_ABSENT, None);
                e.view = Some(store.open_history_upload(&conv)?);
                e.rank = -1;
                e.complete = false;
                e.stamp = stamp;
            } else if !e.complete {
                match store.validate_history_upload(e.view.as_ref().expect("upload view")) {
                    Ok(()) => {}
                    Err(LogError::StaleCursor) => {
                        self.observe_mailbox_work(reason::VIEW_STALE, None);
                        e.view = Some(store.open_history_upload(&conv)?);
                        e.rank = -1;
                        e.last = None;
                        e.stamp = stamp;
                    }
                    Err(error) => return Err(error.into()),
                }
            } else if stamp.is_none() || e.stamp != stamp {
                self.observe_mailbox_work(
                    if stamp.is_none() {
                        reason::STAMP_UNKNOWN
                    } else {
                        reason::STAMP_CHANGED
                    },
                    None,
                );
                let reused = store.resume_history_upload(e.view.as_mut().expect("upload view"))?;
                self.observe_mailbox_work(
                    if reused {
                        reason::VIEW_RESUMED
                    } else {
                        reason::VIEW_NEW
                    },
                    None,
                );
                if !reused {
                    e.view = Some(store.open_history_upload(&conv)?);
                    e.rank = -1;
                    e.last = None;
                }
                e.complete = false;
                e.stamp = stamp;
            }
            let done = e.complete;
            let last = e.last;
            drop(e);
            state_mut(p).history_checked = true;
            state_mut(p).history_done = done;
            if done && let Some((inner, outer)) = last {
                p.uploaded.push((inner, outer));
            }
        }
        if state(p).history_done {
            return Ok(());
        }
        if state(p).history_ids.is_empty() {
            let page = {
                let owner = self.sync_store(&p.recipient);
                let store = owner.lock().expect("projection");
                let e = state(p).lease.0.lock().expect("upload entry");
                let view = e.view.as_ref().expect("upload view");
                let step = store.prepare_history_upload_step(
                    view,
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )?;
                #[cfg(test)]
                HISTORY_METADATA_ROWS.with(|rows| rows.set(rows.get() + step.rows));
                if !step.ready {
                    return Ok(());
                }
                let page = store.history_upload_id_page(
                    view,
                    e.rank,
                    crate::eventlog::sync::network::IdPageBudget {
                        ids: 16,
                        examined: 64,
                    },
                )?;
                #[cfg(test)]
                HISTORY_METADATA_ROWS.with(|rows| rows.set(rows.get() + page.ids.len()));
                page
            };
            state_mut(p).history_after = page.next_rank;
            state_mut(p).history_complete = page.complete;
            state_mut(p).history_ids = page.ids.into();
        }
        if let Some(inner) = state(p).history_ids.front().copied() {
            self.validate_mailbox_history(p)?;
            let known = self.cached_wrapper_id(p.mailbox.id(), inner);
            let outer = if let Some(outer) = known {
                if session::request_durable_have(channel, p.mailbox.id(), outer).await? {
                    self.validate_mailbox_history(p)?;
                    if !self.upload_permitted(p, current) {
                        return Err(session::SessionError::UnexpectedMessage);
                    }
                    outer
                } else {
                    self.upload_history_event(channel, p, inner, current)
                        .await?
                }
            } else {
                self.upload_history_event(channel, p, inner, current)
                    .await?
            };
            self.record_mailbox_result(p, inner, outer);
            state_mut(p).history_ids.pop_front();
        }
        if state(p).history_ids.is_empty() {
            let after = state(p).history_after;
            let complete = state(p).history_complete;
            {
                let mut e = state(p).lease.0.lock().expect("upload entry");
                e.rank = after;
                e.complete = complete;
            }
            state_mut(p).history_done = complete;
        }
        Ok(())
    }
    async fn upload_history_event<IO: AsyncRead + AsyncWrite + Unpin, F: Fn() -> bool + Sync>(
        &self,
        channel: &mut crate::transport::SecureChannel<IO>,
        p: &mut OpaqueMailboxProgress,
        id: EventId,
        current: &F,
    ) -> Result<EventId, session::SessionError> {
        let event = {
            let owner = self.sync_store(&p.recipient);
            let store = owner.lock().expect("projection");
            let e = state(p).lease.0.lock().expect("upload entry");
            store
                .history_upload_event(e.view.as_ref().expect("upload view"), id, PAGE_BYTES)?
                .ok_or(LogError::StaleCursor)?
        };
        self.send_mailbox_event(channel, p, event, current, true)
            .await?
            .ok_or(session::SessionError::NoProgress)
    }
}

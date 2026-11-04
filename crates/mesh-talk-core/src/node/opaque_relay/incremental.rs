//! Bounded mailbox replication. Cursors record transport progress, never delivery.
use super::*;

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
}
struct Lease(Arc<Mutex<UploadEntry>>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.lock().expect("upload entry").busy = false;
    }
}
impl UploadCache {
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
        if !retained {
            let seen = {
                let cache = self.opaque_receive_cache.lock().expect("opaque cache");
                let log = self.log.lock().expect("log lock");
                cache
                    .seen_selected(p.mailbox.id(), &[id], &log)
                    .unwrap_or_default()
                    .contains(&id)
            };
            if seen && !retry {
                state_mut(p).receipt.session_only = true;
            } else {
                let cursor = state(p)
                    .page
                    .as_ref()
                    .map(|page| page.cursor.clone())
                    .or_else(|| state(p).receipt.cursor.clone())
                    .ok_or(session::SessionError::UnexpectedMessage)?;
                let events = mailbox::get(channel, p.mailbox.id(), &cursor, &[id]).await?;
                // A policy change can arrive while the authenticated GET is in
                // flight. Keep this ID queued without admitting its response.
                if !self.relay_receive_allowed(&p.relay) {
                    return Err(LogError::AdmissionRejected(
                        "mailbox receive permission revoked".into(),
                    )
                    .into());
                }
                self.accept_opaque_wrappers(p, events)?;
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
        p.processed_events.clear();
        Ok(())
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
            let receipt = self.load_mailbox_receipt(&key, p.mailbox.id())?;
            let lease = self
                .opaque_upload_cache
                .lock()
                .expect("upload cache")
                .acquire(&key)?;
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
                state_mut(p).receipt.pending.clear();
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
                state_mut(p).retry = Some(state(p).receipt.pending.iter().copied().collect());
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
                state_mut(p).history_checked = true;
                state_mut(p).history_done = true;
                return Ok(());
            }
            let mut e = state(p).lease.0.lock().expect("upload entry");
            if e.view.is_none() {
                e.view = Some(store.open_history_upload(&conv)?);
                e.rank = -1;
                e.complete = false;
                e.stamp = stamp;
            } else if !e.complete {
                match store.validate_history_upload(e.view.as_ref().expect("upload view")) {
                    Ok(()) => {}
                    Err(LogError::StaleCursor) => {
                        e.view = Some(store.open_history_upload(&conv)?);
                        e.rank = -1;
                        e.last = None;
                        e.stamp = stamp;
                    }
                    Err(error) => return Err(error.into()),
                }
            } else if stamp.is_none() || e.stamp != stamp {
                let reused = store.resume_history_upload(e.view.as_mut().expect("upload view"))?;
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

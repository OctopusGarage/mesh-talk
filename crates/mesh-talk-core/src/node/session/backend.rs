//! Explicit legacy collection boundary and direct mandatory paged backend.
use super::*;
use crate::eventlog::sync::network::{
    ExportPage, ExportPageBudget, IdPageBudget, NetworkSyncStore, PreparationBudget,
    PreparationStep, SyncMetadata, SyncPage, apply_received_events,
};
use crate::eventlog::{Event, LogError};
use std::collections::{HashSet, VecDeque};

pub(super) trait SessionBackend {
    type View;
    type HaveCursor;
    type ExportCursor;
    fn remote_have_budget(&self) -> Option<&RemoteHaveBudget> {
        None
    }
    fn probe_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Result<(Option<SyncMetadata>, Option<Self::View>), LogError>;
    fn open_view(&self, conversation: &ConversationId) -> Result<Self::View, LogError>;
    fn prepare_view_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        Ok(PreparationStep {
            ready: true,
            ..Default::default()
        })
    }
    fn reset_export_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        Ok(PreparationStep {
            ready: true,
            ..Default::default()
        })
    }
    fn stage_export_have(&self, view: &Self::View, _ids: &[EventId]) -> Result<(), LogError> {
        self.validate_view(view)
    }
    fn prepare_export_step(
        &self,
        view: &Self::View,
        _have: &HashSet<EventId>,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        Ok(PreparationStep {
            ready: true,
            ..Default::default()
        })
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError>;
    fn advertised_count(&self, view: &Self::View) -> Result<u64, LogError>;
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError>;
    fn have_page(
        &self,
        view: &Self::View,
        after: Option<&Self::HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, Self::HaveCursor>, LogError>;
    fn export_page(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        after: &mut Option<Self::ExportCursor>,
        budget: ExportPageBudget,
        max_frames: usize,
    ) -> Result<FramePage<Self::ExportCursor>, LogError>;
    fn apply(&self, events: &[Event]) -> Result<ApplyReport, LogError>;
    fn admission_denied(&self) -> bool;
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError>;
    fn file_consent(
        &self,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<FileConsentStatus, LogError>;
    fn signed_file_consent(
        &self,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Event>, LogError>;
    fn file_message_identity(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError>;
    fn accept_file_message_identity(&self, bundle: Event) -> Result<bool, LogError>;
    fn sticker_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError>;
    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError>;
}
pub(super) struct FramePage<C> {
    page: ExportPage<C>,
    frame_finished: bool,
}

/// Deliberately collecting compatibility boundary. Only IDs are captured;
/// bounded export still delegates to the source's borrowed/policy implementation.
pub(super) struct LegacyCollectingNetworkAdapter<'a, S>(pub(super) &'a Mutex<S>);
pub(super) struct LegacyView {
    conversation: ConversationId,
    ids: Vec<EventId>,
}
impl<S: SyncStore> SessionBackend for LegacyCollectingNetworkAdapter<'_, S> {
    type View = LegacyView;
    type HaveCursor = usize;
    type ExportCursor = VecDeque<Vec<Event>>;
    fn probe_metadata(
        &self,
        conv: &ConversationId,
    ) -> Result<(Option<SyncMetadata>, Option<Self::View>), LogError> {
        let store = self.0.lock().expect("store mutex not poisoned");
        let (fp, count) = store.fingerprint_and_len(conv)?;
        Ok((
            Some(SyncMetadata {
                count: count as u64,
                fingerprint: Some(fp),
                channel_v2: store.requires_channel_v2(conv)?,
                channel_v3: store.requires_channel_v3(conv)?,
                channel_v4: store.requires_channel_v4(conv)?,
            }),
            None,
        ))
    }
    fn open_view(&self, conv: &ConversationId) -> Result<LegacyView, LogError> {
        let store = self.0.lock().expect("store mutex not poisoned");
        let ids = store.event_ids(conv)?;
        Ok(LegacyView {
            conversation: *conv,
            ids,
        })
    }
    fn metadata(&self, view: &LegacyView) -> Result<SyncMetadata, LogError> {
        // Preserve the legacy responder's current capability checks. These may
        // collect for custom SDK stores; opening/streaming a Have never invokes
        // those defaults and never snapshots event bodies.
        let store = self.0.lock().expect("store mutex not poisoned");
        Ok(SyncMetadata {
            count: view.ids.len() as u64,
            fingerprint: None,
            channel_v2: store.requires_channel_v2(&view.conversation)?,
            channel_v3: store.requires_channel_v3(&view.conversation)?,
            channel_v4: store.requires_channel_v4(&view.conversation)?,
        })
    }
    fn advertised_count(&self, view: &LegacyView) -> Result<u64, LogError> {
        Ok(view.ids.len() as u64)
    }
    fn validate_view(&self, _: &LegacyView) -> Result<(), LogError> {
        Ok(())
    }
    fn have_page(
        &self,
        view: &LegacyView,
        after: Option<&usize>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, usize>, LogError> {
        let start = after.copied().unwrap_or(0);
        let end = (start + budget.ids).min(view.ids.len());
        Ok(SyncPage {
            items: view.ids[start..end].to_vec(),
            next: (end < view.ids.len()).then_some(end),
            examined: end - start,
        })
    }
    fn export_page(
        &self,
        view: &LegacyView,
        have: &HashSet<EventId>,
        after: &mut Option<Self::ExportCursor>,
        _: ExportPageBudget,
        max_frames: usize,
    ) -> Result<FramePage<Self::ExportCursor>, LogError> {
        let mut frames = if let Some(frames) = after.take() {
            frames
        } else {
            let store = self.0.lock().expect("store mutex not poisoned");
            let events = store.events_excluding_bounded(
                &view.conversation,
                have,
                MAX_PLAINTEXT - crate::eventlog::sync::SYNC_MARGIN,
                max_frames,
            )?;
            crate::eventlog::sync::followup_frames(
                view.conversation,
                events,
                MAX_PLAINTEXT - crate::eventlog::sync::SYNC_MARGIN,
                max_frames,
            )
            .into_iter()
            .map(|frame| frame.events)
            .collect()
        };
        let events = frames.pop_front().unwrap_or_default();
        let bytes = events
            .iter()
            .map(crate::eventlog::sync::encoded_event_size)
            .sum();
        Ok(FramePage {
            page: ExportPage {
                events,
                next: (!frames.is_empty()).then_some(frames),
                examined: 0,
                encoded_bytes: bytes,
                allocated_bytes: 0,
            },
            frame_finished: true,
        })
    }
    fn apply(&self, events: &[Event]) -> Result<ApplyReport, LogError> {
        let mut store = self.0.lock().expect("store mutex not poisoned");
        apply_received_events(events, |e| store.ingest(e))
    }
    fn admission_denied(&self) -> bool {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .admission_denied()
    }
    fn durable_have(&self, c: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .durable_have(c, id)
    }
    fn file_consent(
        &self,
        c: &ConversationId,
        id: &EventId,
    ) -> Result<FileConsentStatus, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .file_consent(c, id)
    }
    fn signed_file_consent(
        &self,
        c: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .signed_file_consent(c, id)
    }
    fn file_message_identity(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .file_message_identity(file, manifest)
    }
    fn accept_file_message_identity(&self, bundle: Event) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .accept_file_message_identity(bundle)
    }
    fn sticker_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .sticker_file_consent(file, manifest, headers)
    }
    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .sticker_file_proof(file, manifest)
    }
}
pub(super) struct PagedNetworkBackend<'a, S>(
    pub(super) &'a Mutex<S>,
    pub(super) Option<&'a RemoteHaveBudget>,
);
impl<S: NetworkSyncStore> SessionBackend for PagedNetworkBackend<'_, S> {
    fn remote_have_budget(&self) -> Option<&RemoteHaveBudget> {
        self.1
    }
    type View = S::View;
    type HaveCursor = S::HaveCursor;
    type ExportCursor = S::ExportCursor;
    fn probe_metadata(
        &self,
        c: &ConversationId,
    ) -> Result<(Option<SyncMetadata>, Option<Self::View>), LogError> {
        let store = self.0.lock().expect("store mutex not poisoned");
        let view = store.open_view(c)?;
        Ok((None, Some(view)))
    }
    fn open_view(&self, c: &ConversationId) -> Result<S::View, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .open_view(c)
    }
    fn prepare_view_step(
        &self,
        v: &S::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .prepare_view_step(v, budget)
    }
    fn reset_export_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .reset_export_step(view, budget)
    }
    fn stage_export_have(&self, view: &Self::View, ids: &[EventId]) -> Result<(), LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .stage_export_have(view, ids)
    }
    fn prepare_export_step(
        &self,
        v: &S::View,
        have: &HashSet<EventId>,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .prepare_export_step(v, have, budget)
    }
    fn metadata(&self, v: &S::View) -> Result<SyncMetadata, LogError> {
        self.0.lock().expect("store mutex not poisoned").metadata(v)
    }
    fn advertised_count(&self, view: &S::View) -> Result<u64, LogError> {
        Ok(self.metadata(view)?.count)
    }
    fn validate_view(&self, v: &S::View) -> Result<(), LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .validate_view(v)
    }
    fn have_page(
        &self,
        v: &S::View,
        a: Option<&S::HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, S::HaveCursor>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .have_page(v, a, budget)
    }
    fn export_page(
        &self,
        v: &S::View,
        have: &HashSet<EventId>,
        after: &mut Option<Self::ExportCursor>,
        budget: ExportPageBudget,
        _: usize,
    ) -> Result<FramePage<Self::ExportCursor>, LogError> {
        let page = self
            .0
            .lock()
            .expect("store mutex not poisoned")
            .export_page(v, have, after.as_ref(), budget)?;
        Ok(FramePage {
            page,
            frame_finished: false,
        })
    }
    fn apply(&self, events: &[Event]) -> Result<ApplyReport, LogError> {
        let mut store = self.0.lock().expect("store mutex not poisoned");
        apply_received_events(events, |e| store.ingest(e))
    }
    fn admission_denied(&self) -> bool {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .admission_denied()
    }
    fn durable_have(&self, c: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .durable_have(c, id)
    }
    fn file_consent(
        &self,
        c: &ConversationId,
        id: &EventId,
    ) -> Result<FileConsentStatus, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .file_consent(c, id)
    }
    fn signed_file_consent(
        &self,
        c: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .signed_file_consent(c, id)
    }
    fn file_message_identity(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .file_message_identity(file, manifest)
    }
    fn accept_file_message_identity(&self, bundle: Event) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .accept_file_message_identity(bundle)
    }
    fn sticker_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .sticker_file_consent(file, manifest, headers)
    }
    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .sticker_file_proof(file, manifest)
    }
}

pub(super) async fn prepare_view<B: SessionBackend>(
    backend: &B,
    view: &B::View,
) -> Result<(), SessionError> {
    loop {
        if backend
            .prepare_view_step(view, PreparationBudget::default())?
            .ready
        {
            backend.validate_view(view)?;
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}
pub(super) async fn open_prepared_view<B: SessionBackend>(
    backend: &B,
    conversation: &ConversationId,
) -> Result<B::View, SessionError> {
    let view = backend.open_view(conversation)?;
    prepare_view(backend, &view).await?;
    Ok(view)
}
pub(super) async fn prepared_probe<B: SessionBackend>(
    backend: &B,
    conversation: &ConversationId,
) -> Result<(SyncMetadata, Option<B::View>), SessionError> {
    let (metadata, view) = backend.probe_metadata(conversation)?;
    if let Some(view) = &view {
        prepare_view(backend, view).await?;
    }
    let metadata =
        match metadata {
            Some(metadata) => metadata,
            None => backend
                .metadata(view.as_ref().ok_or_else(|| {
                    LogError::CorruptFile("probe has no metadata or view".into())
                })?)?,
        };
    Ok((metadata, view))
}

pub(super) fn have_count(count: u64) -> Result<usize, SessionError> {
    let count = usize::try_from(count).map_err(|_| SessionError::UnexpectedMessage)?;
    if count > MAX_HAVE_IDS_TOTAL {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(count)
}
pub(super) fn unsupported_metadata(m: SyncMetadata, v2: bool, v3: bool, v4: bool) -> bool {
    (!v2 && m.channel_v2) || (!v3 && m.channel_v3) || (!v4 && m.channel_v4)
}

pub(super) async fn send_have_pages<B: SessionBackend, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    backend: &B,
    view: &B::View,
    conversation: ConversationId,
    wrap: fn(HaveChunk) -> SyncWire,
) -> Result<(), SessionError> {
    let expected_count = have_count(backend.advertised_count(view)?)?;
    let mut cursor = None;
    let mut ids = 0usize;
    let mut chunks = 0usize;
    let mut pending = Vec::with_capacity(expected_count.min(HAVE_IDS_PER_CHUNK));
    loop {
        // Keep the last chunk until actual EOF. One lookahead ID proves that a
        // full pending chunk has a successor, even across empty scanning pages.
        // At most 1500 pending IDs plus that one lookahead ID are resident.
        let capacity = HAVE_IDS_PER_CHUNK - pending.len();
        let budget = IdPageBudget {
            ids: capacity.max(1),
            examined: HAVE_IDS_PER_CHUNK,
        };
        let page = backend.have_page(view, cursor.as_ref(), budget)?;
        ids = ids
            .checked_add(page.items.len())
            .ok_or(SessionError::UnexpectedMessage)?;
        if page.items.len() > budget.ids || ids > MAX_HAVE_IDS_TOTAL {
            return Err(SessionError::UnexpectedMessage);
        }
        cursor = page.next;
        if !page.items.is_empty() && capacity == 0 {
            chunks += 1;
            if chunks > MAX_HAVE_CHUNKS {
                return Err(SessionError::UnexpectedMessage);
            }
            let bytes = encode(&wrap(HaveChunk {
                conversation,
                ids: std::mem::take(&mut pending),
                more: true,
            }))?;
            backend.validate_view(view)?;
            channel.send(&bytes).await?;
        }
        pending.reserve_exact(page.items.len());
        pending.extend(page.items);
        if cursor.is_none() {
            if ids != expected_count {
                return Err(LogError::CorruptFile(
                    "network Have count disagrees with captured view".into(),
                )
                .into());
            }
            chunks += 1;
            if chunks > MAX_HAVE_CHUNKS {
                return Err(SessionError::UnexpectedMessage);
            }
            let bytes = encode(&wrap(HaveChunk {
                conversation,
                ids: pending,
                more: false,
            }))?;
            backend.validate_view(view)?;
            channel.send(&bytes).await?;
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
}

pub(super) async fn intersection_count<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    peer: &HashSet<EventId>,
) -> Result<usize, SessionError> {
    let mut cursor = None;
    let mut count = 0usize;
    loop {
        let page = backend.have_page(
            view,
            cursor.as_ref(),
            IdPageBudget {
                ids: HAVE_IDS_PER_CHUNK,
                examined: HAVE_IDS_PER_CHUNK,
            },
        )?;
        count += page.items.iter().filter(|id| peer.contains(id)).count();
        cursor = page.next;
        if cursor.is_none() {
            return Ok(count);
        }
        tokio::task::yield_now().await;
    }
}
// Largest common event payload fits Response (52-byte empty envelope) and
// Followup (44 bytes). Compute framing through the unchanged codec, not guesses.
fn event_frame_budget(conversation: ConversationId) -> Result<usize, SessionError> {
    let overhead = encode(&SyncWire::Response(SyncResponse {
        conversation,
        events: vec![],
        have: vec![],
    }))?
    .len();
    Ok(MAX_PLAINTEXT - overhead)
}
pub(super) async fn select_export_frames<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    conversation: ConversationId,
    have: &HashSet<EventId>,
    max_frames: usize,
) -> Result<Vec<SyncFollowup>, SessionError> {
    loop {
        if backend
            .reset_export_step(view, PreparationBudget::default())?
            .ready
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let mut have_batch = Vec::with_capacity(PreparationBudget::default().rows);
    for id in have {
        have_batch.push(*id);
        if have_batch.len() == PreparationBudget::default().rows {
            backend.stage_export_have(view, &have_batch)?;
            have_batch.clear();
            tokio::task::yield_now().await;
        }
    }
    if !have_batch.is_empty() {
        backend.stage_export_have(view, &have_batch)?;
    }
    loop {
        if backend
            .prepare_export_step(view, have, PreparationBudget::default())?
            .ready
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    let frame_budget = event_frame_budget(conversation)?;
    let mut frames = Vec::new();
    let mut cursor = None;
    let mut frame = Vec::new();
    let mut encoded = 0;
    let mut allocated = 0;
    loop {
        let budget = ExportPageBudget {
            examined: 512,
            events: 512 - frame.len(),
            encoded_bytes: frame_budget - encoded,
            allocated_bytes: 4 * 1024 * 1024 - allocated,
        };
        if budget.events == 0 || budget.encoded_bytes == 0 || budget.allocated_bytes == 0 {
            frames.push(SyncFollowup {
                conversation,
                events: std::mem::take(&mut frame),
            });
            encoded = 0;
            allocated = 0;
            if frames.len() == max_frames {
                return Ok(frames);
            }
        }
        let budget = ExportPageBudget {
            examined: 512,
            events: 512 - frame.len(),
            encoded_bytes: frame_budget - encoded,
            allocated_bytes: 4 * 1024 * 1024 - allocated,
        };
        // Borrowed page cursors are required externally. Preserve the cursor
        // on a budget boundary so the next frame retries the first unselected row.
        // SQL leaves the borrowed cursor intact when the next event needs a new frame.
        let result = backend.export_page(view, have, &mut cursor, budget, max_frames);
        let batch = match result {
            Ok(batch) => batch,
            Err(LogError::PageBudgetTooSmall { .. }) if !frame.is_empty() => {
                frames.push(SyncFollowup {
                    conversation,
                    events: std::mem::take(&mut frame),
                });
                encoded = 0;
                allocated = 0;
                if frames.len() == max_frames {
                    return Ok(frames);
                }
                tokio::task::yield_now().await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let page = batch.page;
        cursor = page.next;
        encoded += page.encoded_bytes;
        allocated += page.allocated_bytes;
        frame.extend(page.events);
        if encoded > frame_budget && !batch.frame_finished {
            return Err(SessionError::UnexpectedMessage);
        }
        let eof = cursor.is_none();
        // Accumulate bounded scan pages until the frame fills or reaches EOF.
        // Empty continuation pages release the lock and yield as well.
        if !frame.is_empty() && (batch.frame_finished || eof) {
            frames.push(SyncFollowup {
                conversation,
                events: std::mem::take(&mut frame),
            });
            encoded = 0;
            allocated = 0;
            if frames.len() == max_frames {
                return Ok(frames);
            }
        }
        if eof {
            return Ok(frames);
        }
        tokio::task::yield_now().await;
    }
}

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
    fn supports_file_chunk_scope(&self, _file: &ConversationId) -> Result<bool, LogError> {
        Ok(false)
    }
    fn available_file_chunks(
        &self,
        _view: &Self::View,
        _author: crate::eventlog::Author,
        _start: u32,
        _count: u32,
    ) -> Result<Option<Vec<bool>>, LogError> {
        Ok(None)
    }
    fn export_file_chunk(
        &self,
        _view: &Self::View,
        _author: crate::eventlog::Author,
        _index: u32,
        _encoded_limit: usize,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    fn bind_session_peer(&self, _peer: &crate::identity::device::PublicIdentity) {}
    fn remote_have_budget(&self) -> Option<&RemoteHaveBudget> {
        None
    }
    fn probe_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Result<(Option<SyncMetadata>, Option<Self::View>), LogError>;
    fn quiet_comparison(
        &self,
        _c: &ConversationId,
    ) -> Result<Option<crate::eventlog::sync::network::QuietComparison>, LogError> {
        Ok(None)
    }
    fn validate_quiet_comparison(
        &self,
        _c: &ConversationId,
        _hint: crate::eventlog::sync::network::QuietComparison,
    ) -> Result<bool, LogError> {
        Ok(false)
    }
    fn quiet_comparison_token(&self, _view: &Self::View) -> Result<Option<[u8; 32]>, LogError> {
        Ok(None)
    }
    fn retain_quiet_comparison(
        &self,
        _view: &Self::View,
        _hint: crate::eventlog::sync::network::QuietComparison,
    ) -> Result<(), LogError> {
        Ok(())
    }
    fn open_view(&self, conversation: &ConversationId) -> Result<Self::View, LogError>;
    fn take_custody_view(
        &self,
        _conversation: &ConversationId,
    ) -> Result<Option<Self::View>, LogError> {
        Ok(None)
    }
    fn retain_custody_view(&self, view: Self::View) -> Result<Option<Self::View>, LogError> {
        Ok(Some(view))
    }
    fn retain_limited_view(&self, view: Self::View) -> Result<Option<Self::View>, LogError> {
        self.retain_custody_view(view)
    }
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
    /// Qualify an exclusively owned prepared view for another selection phase.
    /// True requires the complete current source and authority stamp to match;
    /// finite-cutoff validation alone may intentionally permit later appends.
    /// Have-dependent export preparation remains a separate obligation.
    fn reusable_view(&self, _view: &Self::View) -> Result<bool, LogError> {
        Ok(false)
    }
    fn reusable_export(
        &self,
        _view: &Self::View,
        _have: &HashSet<EventId>,
    ) -> Result<bool, LogError> {
        Ok(false)
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
    fn export_preparation_needs_have(&self, _view: &Self::View) -> Result<bool, LogError> {
        Ok(true)
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
    fn durable_have_in_view(
        &self,
        view: &Self::View,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<bool, LogError> {
        self.validate_view(view)?;
        self.durable_have(conversation, id)
    }

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
                channel_v5: store.requires_channel_v5(conv)?,
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
            channel_v5: store.requires_channel_v5(&view.conversation)?,
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
    fn bind_session_peer(&self, peer: &crate::identity::device::PublicIdentity) {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .bind_session_peer(peer);
    }
    fn remote_have_budget(&self) -> Option<&RemoteHaveBudget> {
        self.1
    }
    type View = S::View;
    type HaveCursor = S::HaveCursor;
    type ExportCursor = S::ExportCursor;
    fn quiet_comparison(
        &self,
        c: &ConversationId,
    ) -> Result<Option<crate::eventlog::sync::network::QuietComparison>, LogError> {
        self.0.lock().expect("store lock").quiet_comparison(c)
    }
    fn validate_quiet_comparison(
        &self,
        c: &ConversationId,
        hint: crate::eventlog::sync::network::QuietComparison,
    ) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store lock")
            .validate_quiet_comparison(c, hint)
    }
    fn quiet_comparison_token(&self, view: &Self::View) -> Result<Option<[u8; 32]>, LogError> {
        self.0
            .lock()
            .expect("store lock")
            .quiet_comparison_token(view)
    }
    fn retain_quiet_comparison(
        &self,
        view: &Self::View,
        hint: crate::eventlog::sync::network::QuietComparison,
    ) -> Result<(), LogError> {
        self.0
            .lock()
            .expect("store lock")
            .retain_quiet_comparison(view, hint)
    }
    fn export_preparation_needs_have(&self, view: &Self::View) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .export_preparation_needs_have(view)
    }
    fn probe_metadata(
        &self,
        c: &ConversationId,
    ) -> Result<(Option<SyncMetadata>, Option<Self::View>), LogError> {
        let mut store = self.0.lock().expect("store mutex not poisoned");
        if let Some(metadata) = store.probe_metadata(c)? {
            return Ok((Some(metadata), None));
        }
        let view = match store.take_custody_view(c)? {
            Some(view) => view,
            None => store.open_view(c)?,
        };
        Ok((None, Some(view)))
    }
    fn open_view(&self, c: &ConversationId) -> Result<S::View, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .open_view(c)
    }
    fn take_custody_view(&self, c: &ConversationId) -> Result<Option<S::View>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .take_custody_view(c)
    }
    fn retain_limited_view(&self, view: S::View) -> Result<Option<S::View>, LogError> {
        self.0.lock().expect("store lock").retain_range_view(view)
    }
    fn retain_custody_view(&self, view: S::View) -> Result<Option<S::View>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .retain_custody_view(view)
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
    fn reusable_view(&self, view: &S::View) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .reusable_view(view)
    }
    fn reusable_export(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
    ) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .reusable_export(view, have)
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
    fn supports_file_chunk_scope(&self, file: &ConversationId) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .supports_file_chunk_scope(file)
    }
    fn available_file_chunks(
        &self,
        view: &S::View,
        author: crate::eventlog::Author,
        start: u32,
        count: u32,
    ) -> Result<Option<Vec<bool>>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .available_file_chunks(view, author, start, count)
    }
    fn export_file_chunk(
        &self,
        view: &S::View,
        author: crate::eventlog::Author,
        index: u32,
        encoded_limit: usize,
    ) -> Result<Option<Event>, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .export_file_chunk(view, author, index, encoded_limit)
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
    fn durable_have_in_view(
        &self,
        view: &S::View,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<bool, LogError> {
        self.0
            .lock()
            .expect("store mutex not poisoned")
            .durable_have_in_view(view, conversation, id)
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
    // A qualified ready cache hit must stay read-only. Partial or changed
    // selections still pass through bounded preparation and fresh validation.
    if backend.reusable_view(view)? {
        return Ok(());
    }
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
    // Check out the existing exclusive connection lease before allocating a
    // new durable selection. The backend rechecks exact source and authority.
    // A retained range selection may still be partially prepared. Checkout
    // qualifies its owner and authority, but does not establish readiness.
    let view = match backend.take_custody_view(conversation)? {
        Some(view) if backend.reusable_view(&view)? => view,
        Some(view) => {
            // An independent custody request needs the current source cut.
            // Range checkout may legitimately preserve an older continuation;
            // that exception must not become a completed legacy Have answer.
            drop(view);
            backend.open_view(conversation)?
        }
        None => backend.open_view(conversation)?,
    };
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

pub(super) async fn bounded_view<B: SessionBackend>(
    backend: &B,
    conversation: &ConversationId,
) -> Result<B::View, SessionError> {
    let view = match backend.take_custody_view(conversation)? {
        Some(view) => view,
        None => backend.open_view(conversation)?,
    };
    for _ in 0..4 {
        if backend
            .prepare_view_step(&view, PreparationBudget::default())?
            .ready
        {
            backend.validate_view(&view)?;
            return Ok(view);
        }
        tokio::task::yield_now().await;
    }
    drop(backend.retain_limited_view(view)?);
    Err(SessionError::ResourceUnavailable)
}
pub(super) async fn bounded_probe<B: SessionBackend>(
    backend: &B,
    conversation: &ConversationId,
) -> Result<(SyncMetadata, Option<B::View>), SessionError> {
    let (metadata, view) = backend.probe_metadata(conversation)?;
    if let (Some(metadata), None) = (&metadata, &view) {
        return Ok((*metadata, None));
    }
    let view = view.ok_or_else(|| LogError::CorruptFile("probe has no metadata or view".into()))?;
    for _ in 0..4 {
        if backend
            .prepare_view_step(&view, PreparationBudget::default())?
            .ready
        {
            backend.validate_view(&view)?;
            return Ok((backend.metadata(&view)?, Some(view)));
        }
        tokio::task::yield_now().await;
    }
    drop(backend.retain_limited_view(view)?);
    Err(SessionError::ResourceUnavailable)
}

pub(super) fn have_count(count: u64) -> Result<usize, SessionError> {
    let count = usize::try_from(count).map_err(|_| SessionError::UnexpectedMessage)?;
    if count > MAX_HAVE_IDS_TOTAL {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(count)
}
struct SessionHaveSource<'a, B: SessionBackend> {
    backend: &'a B,
    view: &'a B::View,
    scan_budget: Option<(usize, usize)>,
}

impl<B: SessionBackend> mesh_replication::session::HaveSource for SessionHaveSource<'_, B> {
    type Id = EventId;
    type Cursor = B::HaveCursor;
    type Error = SessionError;

    fn page(
        &mut self,
        cursor: Option<&Self::Cursor>,
        ids: usize,
        examined: usize,
    ) -> Result<mesh_replication::session::HavePage<EventId, Self::Cursor>, SessionError> {
        let examined = match self.scan_budget {
            Some((0, _) | (_, 0)) => return Err(SessionError::ResourceUnavailable),
            Some((_, remaining)) => examined.min(remaining),
            None => examined,
        };
        let page = self
            .backend
            .have_page(self.view, cursor, IdPageBudget { ids, examined })?;
        if let Some((pages, rows)) = &mut self.scan_budget {
            *pages -= 1;
            *rows = rows
                .checked_sub(page.examined)
                .ok_or(SessionError::ResourceUnavailable)?;
        }
        Ok(mesh_replication::session::HavePage {
            ids: page.items,
            next: page.next,
        })
    }

    fn validate(&self) -> Result<(), SessionError> {
        Ok(self.backend.validate_view(self.view)?)
    }
}

pub(super) async fn intersection_count<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    peer: &HashSet<EventId>,
    limited: bool,
) -> Result<usize, SessionError> {
    use mesh_replication::session::HaveSource;

    let mut source = SessionHaveSource {
        backend,
        view,
        scan_budget: limited.then_some((8, 2000)),
    };
    let mut cursor = None;
    let mut count = 0usize;
    loop {
        let page = source.page(cursor.as_ref(), HAVE_IDS_PER_CHUNK, HAVE_IDS_PER_CHUNK)?;
        count += page.ids.iter().filter(|id| peer.contains(id)).count();
        cursor = page.next;
        if cursor.is_none() {
            return Ok(count);
        }
        tokio::task::yield_now().await;
    }
}
struct SessionExportSource<'a, B: SessionBackend> {
    backend: &'a B,
    view: &'a B::View,
    have: &'a HashSet<EventId>,
    max_frames: usize,
}

impl<B: SessionBackend> mesh_replication::session::ExportSource for SessionExportSource<'_, B> {
    type Event = Event;
    type Cursor = B::ExportCursor;
    type Error = LogError;

    fn page(
        &mut self,
        cursor: &mut Option<Self::Cursor>,
        space: mesh_replication::session::PageSpace,
    ) -> mesh_replication::session::ExportResult<Event, Self::Cursor, LogError> {
        use mesh_replication::session::{EventPage, ExportError};

        let budget = ExportPageBudget {
            examined: space.examined,
            events: space.events,
            encoded_bytes: space.encoded_bytes,
            allocated_bytes: space.allocated_bytes,
        };
        let batch = self
            .backend
            .export_page(self.view, self.have, cursor, budget, self.max_frames)
            .map_err(|error| match error {
                LogError::PageBudgetTooSmall { .. } => ExportError::PageTooSmall(error),
                _ => ExportError::Source(error),
            })?;
        let page = batch.page;
        Ok(EventPage {
            events: page.events,
            next: page.next,
            encoded_bytes: page.encoded_bytes,
            allocated_bytes: page.allocated_bytes,
            frame_finished: batch.frame_finished,
        })
    }
}

pub(super) async fn prepare_outgoing_selection<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    have: &HashSet<EventId>,
) -> Result<(), SessionError> {
    prepare_outgoing(backend, view, have, None).await
}

async fn prepare_outgoing<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    have: &HashSet<EventId>,
    mut steps: Option<usize>,
) -> Result<(), SessionError> {
    let mut charge = || match &mut steps {
        Some(0) => Err(SessionError::ResourceUnavailable),
        Some(remaining) => {
            *remaining -= 1;
            Ok(())
        }
        None => Ok(()),
    };
    if !backend.reusable_export(view, have)? {
        loop {
            charge()?;
            if backend
                .reset_export_step(view, PreparationBudget::default())?
                .ready
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        if backend.export_preparation_needs_have(view)? {
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
        }
        loop {
            charge()?;
            if backend
                .prepare_export_step(view, have, PreparationBudget::default())?
                .ready
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    }
    Ok(())
}

pub(super) async fn select_export_batches<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    have: &HashSet<EventId>,
    max_frames: usize,
    encoded_per_frame: usize,
) -> Result<Vec<Vec<Event>>, SessionError> {
    select_export(backend, view, have, max_frames, encoded_per_frame, None).await
}

async fn select_export<B: SessionBackend>(
    backend: &B,
    view: &B::View,
    have: &HashSet<EventId>,
    max_frames: usize,
    encoded_per_frame: usize,
    budget: Option<SyncTurnBudget>,
) -> Result<Vec<Vec<Event>>, SessionError> {
    use mesh_replication::session::{ExportError, ExportLimits};

    let (records, bytes) = budget.map_or((512, encoded_per_frame), |limit| {
        (
            limit.records.min(512),
            limit.body_bytes.min(encoded_per_frame),
        )
    });
    if records == 0 || bytes == 0 {
        return Err(SessionError::ResourceUnavailable);
    }
    prepare_outgoing(backend, view, have, budget.map(|_| 4)).await?;

    let limits = ExportLimits {
        frames: max_frames,
        examined: 512,
        events_per_frame: records,
        encoded_per_frame: bytes,
        allocated_per_frame: 4 * 1024 * 1024,
    };
    mesh_replication::session::select_export_frames(
        &mut SessionExportSource {
            backend,
            view,
            have,
            max_frames,
        },
        limits,
        tokio::task::yield_now,
    )
    .await
    .map_err(|error| match error {
        ExportError::Source(error) | ExportError::PageTooSmall(error) => SessionError::from(error),
        ExportError::Limit => SessionError::UnexpectedMessage,
    })
}

pub(super) struct RoundStoreAdapter<'a, B>(pub &'a B, pub(super) Option<&'a Mutex<SyncTurnBudget>>);

impl<B: SessionBackend> legacy_round::RoundBackend<SessionError> for RoundStoreAdapter<'_, B> {
    type Peer = ReconciliationPeer;
    type View = B::View;
    type RemoteHave = have_budget::ReceivedHave;

    fn bind_session_peer(&self, peer: &Self::Peer) {
        self.0.bind_session_peer(peer);
    }

    async fn prepared_probe(
        &self,
        scope: &ConversationId,
    ) -> Result<legacy_round::Probe<Self::View>, SessionError> {
        if self.1.is_some() {
            bounded_probe(self.0, scope).await
        } else {
            prepared_probe(self.0, scope).await
        }
    }

    async fn open_prepared_view(&self, scope: &ConversationId) -> Result<Self::View, SessionError> {
        if self.1.is_some() {
            bounded_view(self.0, scope).await
        } else {
            open_prepared_view(self.0, scope).await
        }
    }

    fn advertised_count(&self, view: &Self::View) -> Result<usize, SessionError> {
        have_count(self.0.advertised_count(view)?)
    }

    fn have_count(&self, count: u64) -> Result<usize, SessionError> {
        have_count(count)
    }

    fn validate_view(&self, view: &Self::View) -> Result<(), SessionError> {
        Ok(self.0.validate_view(view)?)
    }

    fn reusable_view(&self, view: &Self::View) -> Result<bool, SessionError> {
        Ok(self.0.reusable_view(view)?)
    }

    fn retain_custody_view(&self, view: Self::View) -> Result<Option<Self::View>, SessionError> {
        Ok(self.0.retain_custody_view(view)?)
    }

    fn quiet_comparison(
        &self,
        scope: &ConversationId,
    ) -> Result<Option<legacy_round::QuietComparison>, SessionError> {
        Ok(self.0.quiet_comparison(scope)?)
    }

    fn validate_quiet_comparison(
        &self,
        scope: &ConversationId,
        hint: legacy_round::QuietComparison,
    ) -> Result<bool, SessionError> {
        Ok(self.0.validate_quiet_comparison(scope, hint)?)
    }

    fn quiet_comparison_token(&self, view: &Self::View) -> Result<Option<[u8; 32]>, SessionError> {
        Ok(self.0.quiet_comparison_token(view)?)
    }

    fn retain_quiet_comparison(
        &self,
        view: &Self::View,
        hint: legacy_round::QuietComparison,
    ) -> Result<(), SessionError> {
        Ok(self.0.retain_quiet_comparison(view, hint)?)
    }

    type HaveReservation = Option<tokio::sync::OwnedSemaphorePermit>;
    type HaveCursor = B::HaveCursor;

    fn have_limits(&self) -> HaveLimits {
        if self.1.is_some() {
            HaveLimits {
                ids_per_frame: HAVE_IDS_PER_CHUNK,
                frames: 8,
                total_ids: 2000,
            }
        } else {
            HAVE_LIMITS
        }
    }

    fn have_source<'a>(
        &'a self,
        view: &'a Self::View,
    ) -> impl mesh_replication::session::HaveSource<
        Id = EventId,
        Cursor = Self::HaveCursor,
        Error = SessionError,
    > + 'a {
        SessionHaveSource {
            backend: self.0,
            view,
            scan_budget: self.1.map(|_| (8, 2000)),
        }
    }

    fn reserve_remote_have(&self) -> Result<Self::HaveReservation, SessionError> {
        self.0
            .remote_have_budget()
            .map(RemoteHaveBudget::try_admit)
            .transpose()
    }

    fn collect_remote_have(
        &self,
        ids: Vec<EventId>,
        permit: Self::HaveReservation,
        compare: bool,
    ) -> Result<Self::RemoteHave, SessionError> {
        if compare {
            have_budget::ReceivedHave::from_vec_for_comparison(ids, permit, true)
        } else {
            have_budget::ReceivedHave::from_vec(ids, permit)
        }
    }

    fn have_error(&self, error: HaveError<SessionError>) -> SessionError {
        error.into()
    }

    async fn yield_inventory_work(&self) {
        tokio::task::yield_now().await;
    }

    async fn intersection_count(
        &self,
        view: &Self::View,
        have: &Self::RemoteHave,
    ) -> Result<usize, SessionError> {
        intersection_count(self.0, view, have, self.1.is_some()).await
    }

    fn comparison_fingerprint(&self, remote: &Self::RemoteHave) -> Option<[u8; 32]> {
        remote.comparison_fingerprint()
    }

    fn apply(&self, events: &[Event]) -> Result<ApplyReport, SessionError> {
        if let Some(budget) = self.1 {
            budget
                .lock()
                .expect("turn budget lock")
                .charge_events(events)?;
        }
        Ok(self.0.apply(events)?)
    }

    fn admission_denied(&self) -> bool {
        self.0.admission_denied()
    }

    async fn select_export_batches(
        &self,
        view: &Self::View,
        remote: &Self::RemoteHave,
        frames: usize,
        encoded_per_frame: usize,
    ) -> Result<Vec<Vec<Event>>, SessionError> {
        let Some(budget) = self.1 else {
            return select_export_batches(self.0, view, remote, frames, encoded_per_frame).await;
        };
        let remaining = *budget.lock().expect("turn budget lock");
        let batches = select_export(
            self.0,
            view,
            remote,
            frames,
            encoded_per_frame,
            Some(remaining),
        )
        .await?;
        let mut budget = budget.lock().expect("turn budget lock");
        let mut next = *budget;
        for batch in &batches {
            next.charge_events(batch)?;
        }
        *budget = next;
        Ok(batches)
    }
}
impl<B: SessionBackend> legacy_round::ResponderBackend<SessionError> for RoundStoreAdapter<'_, B> {
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, SessionError> {
        Ok(self.0.metadata(view)?)
    }

    fn durable_have_in_view(
        &self,
        view: &Self::View,
        scope: &ConversationId,
        id: &EventId,
    ) -> Result<bool, SessionError> {
        Ok(self.0.durable_have_in_view(view, scope, id)?)
    }

    fn file_message_identity(
        &self,
        file: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Event>, SessionError> {
        Ok(self.0.file_message_identity(file, id)?)
    }

    fn accept_file_message_identity(&self, event: Event) -> Result<bool, SessionError> {
        Ok(self.0.accept_file_message_identity(event)?)
    }

    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Vec<Event>>, SessionError> {
        Ok(self.0.sticker_file_proof(file, id)?)
    }

    fn sticker_file_consent(
        &self,
        file: &ConversationId,
        id: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, SessionError> {
        Ok(self.0.sticker_file_consent(file, id, headers)?)
    }

    fn signed_file_consent(
        &self,
        file: &ConversationId,
        id: &EventId,
    ) -> Result<Option<Event>, SessionError> {
        Ok(self.0.signed_file_consent(file, id)?)
    }

    fn file_consent(
        &self,
        file: &ConversationId,
        id: &EventId,
    ) -> Result<FileConsentStatus, SessionError> {
        Ok(self.0.file_consent(file, id)?)
    }
}

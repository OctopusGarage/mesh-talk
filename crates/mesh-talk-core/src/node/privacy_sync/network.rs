//! Production participant network adapter. A finite authorization selection lives
//! in the encrypted profile DB; unchanged pages only check scalar revisions.
//! Preparation advances bounded header/edge pages and yields in the session owner.
use super::*;
use crate::eventlog::{
    repository::network::{ExportCursor, HaveCursor, NetworkView},
    sync::{
        FileConsentStatus,
        network::{
            ExportPage, ExportPageBudget, IdPageBudget, NetworkSyncStore, PreparationBudget,
            PreparationStep, SyncMetadata, SyncPage,
        },
    },
};
use std::sync::{Mutex, atomic::Ordering};

/// Exact local history and authorization observation, not mailbox completion.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(in crate::node) struct HistorySyncStamp {
    fingerprint: [u8; 32],
}

/// One bounded runtime cache entry. Its existing SQL selection and profile
/// lease stay owned until this entry is evicted; no guard crosses an await.
pub(in crate::node) struct HistoryUpload {
    view: ParticipantView,
    binding: crate::eventlog::repository::network::HistorySyncBinding,
    peer: PublicIdentity,
    inbound: bool,
    pull_only: bool,
    export_initialized: Mutex<bool>,
    export_ready: Mutex<bool>,
}

#[cfg(test)]
pub(in crate::node) struct HistoryUploadPage {
    pub(in crate::node) events: Vec<Event>,
    pub(in crate::node) next_rank: i64,
    pub(in crate::node) complete: bool,
}

pub(in crate::node) struct HistoryUploadIdPage {
    pub(in crate::node) ids: Vec<EventId>,
    pub(in crate::node) next_rank: i64,
    pub(in crate::node) complete: bool,
}

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct Policy {
    privacy_generation: u64,
    pending: [u8; 32],
    pub(super) network_paused: bool,
    pub(super) sibling: bool,
    device_policy: u64,
}
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
struct Observation {
    source: i64,
    destructive_policy: i64,
    roster: Option<[u8; 32]>,
    proofs: Option<u64>,
    file_scopes: (u64, (u64, u64)),
    controls: Option<[i64; 32]>,
    delivery_controls: u64,
    established: Option<[u8; 32]>,
}
impl Observation {
    fn permits_preparation_progress(&self, current: &Self) -> bool {
        let mut expected = self.clone();
        match (self.controls, current.controls) {
            // Complete kind metadata distinguishes ordinary appends from policy
            // changes without discarding the repository's finite cutoff.
            (Some(before), Some(after)) if before == after => expected.source = current.source,
            // Cold backfill may establish the first control stamp, but cannot
            // certify any concurrent append while its previous stamp is absent.
            (None, Some(_)) if self.source == current.source => {
                expected.controls = current.controls
            }
            _ => {}
        }
        expected == *current
    }
}
pub(in crate::node) struct ParticipantView {
    pub(super) conversation: ConversationId,
    pub(super) repository: NetworkView,
    pub(super) preparation: Box<Mutex<super::principal::Preparation>>,
    pub(super) policy: Policy,
    observed: Mutex<Observation>,
    // Staged invitations already have an 8 MiB owner budget.
    pub(super) pending: Vec<EventId>,
    export_have: Mutex<Option<[u8; 32]>>,
    sibling_closure: bool,
    metadata_stamp: Option<HistorySyncStamp>,
}
#[derive(Clone)]
pub(in crate::node) enum ParticipantHaveCursor {
    Retained(HaveCursor),
    Pending(usize),
}
#[derive(Clone)]
pub(in crate::node) struct ParticipantExportCursor {
    retained: ExportCursor,
    have: [u8; 32],
}
impl GuardedSyncStore<'_> {
    pub(in crate::node) fn history_upload_id_page(
        &self,
        upload: &HistoryUpload,
        after_rank: i64,
        budget: IdPageBudget,
    ) -> Result<HistoryUploadIdPage, LogError> {
        budget.validate()?;
        self.validate_history_upload(upload)?;
        if !*upload.export_ready.lock().expect("upload export lock") {
            return Err(LogError::StaleCursor);
        }
        let (ids, next_rank, complete) = {
            let preparation = upload
                .view
                .preparation
                .lock()
                .expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_upload_id_rank_page(
                    preparation.selection()?,
                    &upload.view.repository,
                    after_rank,
                    preparation
                        .history_generation()
                        .map_or(0, |generation| generation.count),
                    budget,
                )?
        };
        self.validate_history_upload(upload)?;
        Ok(HistoryUploadIdPage {
            ids,
            next_rank,
            complete,
        })
    }

    pub(in crate::node) fn history_upload_event(
        &self,
        upload: &HistoryUpload,
        id: EventId,
        max_bytes: usize,
    ) -> Result<Option<Event>, LogError> {
        self.validate_history_upload(upload)?;
        if !*upload.export_ready.lock().expect("upload export lock") {
            return Err(LogError::StaleCursor);
        }
        let event = {
            let preparation = upload
                .view
                .preparation
                .lock()
                .expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_upload_event(
                    preparation.selection()?,
                    &upload.view.repository,
                    id,
                    preparation
                        .history_generation()
                        .map_or(0, |generation| generation.count),
                    max_bytes,
                )?
        };
        self.validate_history_upload(upload)?;
        Ok(event)
    }

    pub(in crate::node) fn history_upload_contains(
        &self,
        upload: &HistoryUpload,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.validate_history_upload(upload)?;
        if !*upload.export_ready.lock().expect("upload export lock") {
            return Err(LogError::StaleCursor);
        }
        let contains = {
            let preparation = upload
                .view
                .preparation
                .lock()
                .expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_upload_contains(
                    preparation.selection()?,
                    &upload.view.repository,
                    id,
                    preparation
                        .history_generation()
                        .map_or(0, |generation| generation.count),
                )?
        };
        self.validate_history_upload(upload)?;
        Ok(contains)
    }

    pub(in crate::node) fn open_history_upload(
        &self,
        conversation: &ConversationId,
    ) -> Result<HistoryUpload, LogError> {
        let view = NetworkSyncStore::open_view(self, conversation)?;
        // Connection-staged invitation ancestry belongs to that connection's
        // bounded exchange, not a reusable retained-history upload.
        if !view.pending.is_empty() {
            return Err(LogError::StaleCursor);
        }
        let binding = view.repository.history_binding.clone();
        view.preparation
            .lock()
            .expect("view preparation lock")
            .enable_history_upload();
        let upload = HistoryUpload {
            view,
            binding,
            peer: self.peer.clone(),
            inbound: self.inbound,
            pull_only: self.pull_only,
            export_initialized: Mutex::new(false),
            export_ready: Mutex::new(false),
        };
        self.validate_history_upload(&upload)?;
        Ok(upload)
    }

    pub(in crate::node) fn validate_history_upload(
        &self,
        upload: &HistoryUpload,
    ) -> Result<(), LogError> {
        if upload.peer != self.peer
            || upload.inbound != self.inbound
            || upload.pull_only != self.pull_only
        {
            return Err(LogError::StaleCursor);
        }
        self.validate_view(&upload.view)?;
        let captured = upload
            .view
            .preparation
            .lock()
            .expect("view preparation lock")
            .history_generation();
        if let Some(captured) = captured {
            let current = self
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .order_generation(
                    &upload.view.conversation,
                    crate::eventlog::repository::order::CausalOrder::Logical,
                )?;
            if !current.is_some_and(|current| {
                current.job == captured.job && current.count >= captured.count
            }) {
                return Err(LogError::StaleCursor);
            }
        }
        Ok(())
    }

    pub(in crate::node) fn prepare_history_upload_step(
        &self,
        upload: &HistoryUpload,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        self.validate_history_upload(upload)?;
        let view = &upload.view;
        let ready = view
            .preparation
            .lock()
            .expect("view preparation lock")
            .ready();
        let export_initialized = *upload
            .export_initialized
            .lock()
            .expect("upload export lock");
        let mut step = if !ready {
            let mut step = NetworkSyncStore::prepare_view_step(self, view, budget)?;
            // The outgoing parent closure is a separate bounded phase. Never
            // hand off partially computed sibling/share-off flags.
            step.ready = false;
            step
        } else if !export_initialized {
            let preparation = view.preparation.lock().expect("view preparation lock");
            let mut step = self
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .reset_selected_export_step(preparation.selection()?, &view.repository, budget)?;
            if step.ready {
                *upload
                    .export_initialized
                    .lock()
                    .expect("upload export lock") = true;
            }
            step.ready = false;
            step
        } else {
            let preparation = view.preparation.lock().expect("view preparation lock");
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_export_step(
                    preparation.selection()?,
                    &view.repository,
                    view.sibling_closure,
                    !self.inbound && !preparation.replica_shared,
                    budget,
                )?
        };
        self.validate_history_upload(upload)?;
        if step.ready {
            *upload.export_ready.lock().expect("upload export lock") = true;
        }
        // Reading an already ready entry spends no extra rows.
        step.ready |= *upload.export_ready.lock().expect("upload export lock");
        Ok(step)
    }

    pub(in crate::node) fn resume_history_upload(
        &self,
        upload: &mut HistoryUpload,
    ) -> Result<bool, LogError> {
        if upload.peer != self.peer
            || upload.inbound != self.inbound
            || upload.pull_only != self.pull_only
            || !*upload.export_ready.lock().expect("upload export lock")
        {
            return Ok(false);
        }
        match self.validate_history_upload(upload) {
            Ok(()) => {}
            Err(LogError::StaleCursor | LogError::ConversationRetiring) => return Ok(false),
            Err(error) => return Err(error),
        }
        let conversation = upload.view.conversation;
        let (repository, binding, generation) = {
            let log = self.node.log.lock().expect("log lock");
            let source = log.repository()?;
            (
                NetworkSyncStore::open_view(source, &conversation)?,
                source.history_sync_binding(&conversation)?,
                source.order_generation(
                    &conversation,
                    crate::eventlog::repository::order::CausalOrder::Logical,
                )?,
            )
        };
        if binding == upload.binding {
            return Ok(true);
        }
        {
            let mut preparation = upload
                .view
                .preparation
                .lock()
                .expect("view preparation lock");
            if !preparation.resume_history(generation) {
                return Ok(false);
            }
        }
        upload.view.repository = repository;
        upload.binding = binding;
        *upload.export_ready.lock().expect("upload export lock") = false;
        self.validate_history_upload(upload)?;
        upload
            .view
            .preparation
            .lock()
            .expect("view preparation lock")
            .save_history_progress(self, &upload.view)?;
        Ok(true)
    }

    #[cfg(test)]
    pub(in crate::node) fn history_upload_page(
        &self,
        upload: &HistoryUpload,
        after_rank: i64,
        budget: ExportPageBudget,
    ) -> Result<HistoryUploadPage, LogError> {
        budget.validate()?;
        self.validate_history_upload(upload)?;
        if !*upload.export_ready.lock().expect("upload export lock") {
            return Err(LogError::StaleCursor);
        }
        let (events, next_rank, complete) = {
            let preparation = upload
                .view
                .preparation
                .lock()
                .expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_upload_rank_page(
                    preparation.selection()?,
                    &upload.view.repository,
                    after_rank,
                    preparation
                        .history_generation()
                        .map_or(0, |generation| generation.count),
                    budget,
                )?
        };
        self.validate_history_upload(upload)?;
        Ok(HistoryUploadPage {
            events,
            next_rank,
            complete,
        })
    }

    pub(in crate::node) fn history_sync_stamp(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<HistorySyncStamp>, LogError> {
        use sha2::{Digest, Sha256};
        let policy = self.network_policy(conversation)?;
        let mut observation = self.network_observation(conversation)?;
        if observation.controls.is_none() {
            // Backfill belongs to bounded session preparation; a cold index
            // never supplies evidence that policy history is unchanged.
            return Ok(None);
        }
        let binding = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .history_sync_binding(conversation)?;
        // Ordinary appends in another conversation do not affect this source.
        // Global raw controls and destructive dependencies remain exact fences.
        observation.source = 0;
        let mut after = self.network_observation(conversation)?;
        after.source = 0;
        let current_binding = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .history_sync_binding(conversation)?;
        if observation != after
            || binding != current_binding
            || !self.policy_unchanged(conversation, &policy)
        {
            return Ok(None);
        }
        let bytes = bincode::serialize(&(
            binding,
            &self.peer,
            self.node.identity.public(),
            self.node.account_id(),
            policy,
            observation,
            self.pull_only,
            self.inbound,
        ))
        .map_err(|error| LogError::Serialization(error.to_string()))?;
        Ok(Some(HistorySyncStamp {
            fingerprint: Sha256::digest(bytes).into(),
        }))
    }

    fn network_policy(&self, conversation: &ConversationId) -> Result<Policy, LogError> {
        Ok(Policy {
            privacy_generation: self.node.privacy.generation.load(Ordering::SeqCst),
            pending: crate::eventlog::sync::fingerprint(self.pending_ids(conversation)),
            network_paused: self.node.group_file_sharing_status().paused,
            sibling: self.node.certified_sibling(&self.peer),
            device_policy: self.node.device_sync_policy_snapshot().1,
        })
    }
    fn policy_unchanged(&self, conversation: &ConversationId, policy: &Policy) -> bool {
        self.node.privacy.generation.load(Ordering::SeqCst) == policy.privacy_generation
            && crate::eventlog::sync::fingerprint(self.pending_ids(conversation)) == policy.pending
            && self.node.device_sync_policy_snapshot().1 == policy.device_policy
            && self.node.certified_sibling(&self.peer) == policy.sibling
            && self.node.group_file_sharing_status().paused == policy.network_paused
    }
    fn network_observation(&self, conversation: &ConversationId) -> Result<Observation, LogError> {
        let established = self
            .node
            .channels
            .lock()
            .expect("channels lock")
            .state(conversation)
            .map(|state| {
                use sha2::{Digest, Sha256};
                let mut hash = Sha256::new();
                hash.update(state.genesis_owner().len().to_le_bytes());
                hash.update(state.genesis_owner().as_bytes());
                let mut members = state.members().to_vec();
                members.sort_unstable_by_key(|m| (m.ed25519_pub, m.x25519_pub));
                for member in members {
                    hash.update(member.ed25519_pub);
                    hash.update(member.x25519_pub);
                }
                hash.finalize().into()
            });
        // Each statement releases its owner guard before acquiring the next.
        // The before/after observations validate the projection; no combined
        // owner-lock ordering or lock across repository work is required.
        let source = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .source_revision()?;
        let destructive_policy = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .destructive_policy_revision()?;
        let roster = self
            .node
            .roster
            .lock()
            .expect("roster lock")
            .historical_announcement(&self.peer)
            .map(|proof| {
                use sha2::{Digest, Sha256};
                // Route/last-seen heartbeats are not authorization changes. Capture
                // the exact bounded authenticated proof used by this principal.
                bincode::serialize(proof)
                    .map(|bytes| Sha256::digest(bytes).into())
                    .map_err(|error| LogError::Serialization(error.to_string()))
            })
            .transpose()?;
        let proofs = self
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock")
            .as_ref()
            .map(|s| s.proofs.revision());
        let delivery_controls = self
            .node
            .delivery_control_ids
            .lock()
            .expect("control ids lock")
            .revision();
        let file_scopes = (
            self.node.privacy.projection_revision.load(Ordering::SeqCst),
            self.node.file_scope_revision()?,
        );
        let controls = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .raw_policy_revisions()?
            .map(|mut revisions| {
                for kind in [
                    EventKind::Message,
                    EventKind::Edit,
                    EventKind::Delete,
                    EventKind::React,
                    EventKind::ReadMarker,
                    EventKind::FileManifest,
                    EventKind::Profile,
                ] {
                    revisions[kind as usize] = -1;
                }
                revisions
            });
        Ok(Observation {
            source,
            destructive_policy,
            roster,
            proofs,
            delivery_controls,
            file_scopes,
            controls,
            established,
        })
    }

    fn pending_ids(&self, conversation: &ConversationId) -> Vec<EventId> {
        let mut ids: Vec<_> = self
            .pending_channels
            .get(conversation)
            .into_iter()
            .flat_map(|pending| pending.events.iter().map(|event| event.id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}
impl NetworkSyncStore for GuardedSyncStore<'_> {
    type View = ParticipantView;
    type HaveCursor = ParticipantHaveCursor;
    type ExportCursor = ParticipantExportCursor;
    fn probe_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<SyncMetadata>, LogError> {
        let Some(stamp) = self.history_sync_stamp(conversation)? else {
            return Ok(None);
        };
        let metadata = self
            .node
            .channel_projection_cache
            .lock()
            .expect("projection cache lock not poisoned")
            .get_paged(&stamp);
        if metadata.is_some() && self.history_sync_stamp(conversation)?.as_ref() != Some(&stamp) {
            return Ok(None);
        }
        Ok(metadata)
    }
    fn open_view(&self, conversation: &ConversationId) -> Result<Self::View, LogError> {
        // Capture BEFORE the finite repository cutoff. A later append must
        // never label this older view with its newer exact history stamp.
        let metadata_stamp = self.history_sync_stamp(conversation)?;
        let policy = self.network_policy(conversation)?;
        let observed = self.network_observation(conversation)?;
        let repository = NetworkSyncStore::open_view(
            self.node.log.lock().expect("log lock").repository()?,
            conversation,
        )?;
        let pending = self.pending_ids(conversation);
        let sibling_closure = policy.sibling
            && *conversation
                == super::super::conversation::dm_conversation_id(
                    &self.node.identity.public(),
                    &self.peer,
                );
        let view = ParticipantView {
            conversation: *conversation,
            repository,
            preparation: Box::new(Mutex::new(super::principal::Preparation::default())),
            policy,
            observed: Mutex::new(observed),
            pending,
            export_have: Mutex::new(None),
            sibling_closure,
            metadata_stamp,
        };
        #[cfg(test)]
        self.node
            .trace_account_client(&self.peer, || "network_view_captured".into());
        Ok(view)
    }
    fn take_custody_view(
        &mut self,
        conversation: &ConversationId,
    ) -> Result<Option<Self::View>, LogError> {
        let Some((denied, view)) = self.custody_view.take() else {
            return Ok(None);
        };
        if view.conversation != *conversation || denied != self.denied {
            return Ok(None);
        }
        let current = self.history_sync_stamp(conversation)?;
        if current.is_none() || current.as_ref() != view.metadata_stamp.as_ref() {
            return Ok(None);
        }
        match self.validate_view(&view) {
            Ok(()) => Ok(Some(view)),
            Err(LogError::StaleCursor) => Ok(None),
            Err(error) => Err(error),
        }
    }
    fn retain_custody_view(&mut self, view: Self::View) -> Result<Option<Self::View>, LogError> {
        // Evict before admitting another selection. The existing SQL owner
        // bounds both live selections and incremental retirement work.
        self.custody_view = None;
        let ready = view
            .preparation
            .lock()
            .expect("view preparation lock")
            .ready();
        if !ready {
            return Ok(Some(view));
        }
        self.validate_view(&view)?;
        if let Some(stamp) = &view.metadata_stamp
            && self.history_sync_stamp(&view.conversation)?.as_ref() == Some(stamp)
        {
            self.custody_view = Some((self.denied, view));
            Ok(None)
        } else {
            Ok(Some(view))
        }
    }
    fn prepare_view_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.validate_view(view)?;
        // Keep the observation that was actually validated. Recapturing here
        // could absorb an authority change between validation and preparation.
        let before = view.observed.lock().expect("view observation lock").clone();
        let step = {
            let mut preparation = view.preparation.lock().expect("view preparation lock");
            preparation.step(self, view, budget)?
        };
        let after = self.network_observation(&view.conversation)?;
        if !before.permits_preparation_progress(&after)
            || !self.policy_unchanged(&view.conversation, &view.policy)
        {
            return Err(LogError::StaleCursor);
        }
        *view.observed.lock().expect("view observation lock") = after;
        Ok(step)
    }
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError> {
        NetworkSyncStore::validate_view(
            self.node.log.lock().expect("log lock").repository()?,
            &view.repository,
        )?;
        if !self.policy_unchanged(&view.conversation, &view.policy) {
            return Err(LogError::StaleCursor);
        }
        let current = self.network_observation(&view.conversation)?;
        let ready = view
            .preparation
            .lock()
            .expect("view preparation lock")
            .ready();
        let mut observed = view.observed.lock().expect("view observation lock");
        let unchanged = if ready {
            // Ordinary appends preserve the finite cutoff. Raw control changes,
            // including rejected controls, and all mutable policy owners do not.
            let mut expected = observed.clone();
            expected.source = current.source;
            expected == current
        } else {
            observed.permits_preparation_progress(&current)
        };
        if !unchanged {
            return Err(LogError::StaleCursor);
        }
        *observed = current;
        Ok(())
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError> {
        self.validate_view(view)?;
        let preparation = view.preparation.lock().expect("view preparation lock");
        if !preparation.ready() {
            return Err(LogError::StaleCursor);
        }
        let metadata = preparation.metadata;
        drop(preparation);
        if metadata.fingerprint.is_some()
            && let Some(stamp) = &view.metadata_stamp
            && self.history_sync_stamp(&view.conversation)?.as_ref() == Some(stamp)
        {
            self.node
                .channel_projection_cache
                .lock()
                .expect("projection cache lock not poisoned")
                .insert_paged(stamp.clone(), metadata);
        }
        Ok(metadata)
    }
    fn have_page(
        &self,
        view: &Self::View,
        after: Option<&ParticipantHaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, ParticipantHaveCursor>, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        if let Some(ParticipantHaveCursor::Pending(start)) = after {
            if *start > view.pending.len() {
                return Err(LogError::StaleCursor);
            }
            let end = start
                .saturating_add(budget.ids.min(budget.examined))
                .min(view.pending.len());
            let result = SyncPage {
                items: view.pending[*start..end].to_vec(),
                examined: end - start,
                next: (end < view.pending.len()).then_some(ParticipantHaveCursor::Pending(end)),
            };
            self.validate_view(view)?;
            return Ok(result);
        }
        let retained_after = match after {
            Some(ParticipantHaveCursor::Retained(cursor)) => Some(cursor),
            None => None,
            Some(ParticipantHaveCursor::Pending(_)) => unreachable!(),
        };
        let page = {
            let preparation = view.preparation.lock().expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_have_page(
                    preparation.selection()?,
                    &view.repository,
                    retained_after,
                    budget,
                )?
        };
        let next = page
            .next
            .map(ParticipantHaveCursor::Retained)
            .or_else(|| (!view.pending.is_empty()).then_some(ParticipantHaveCursor::Pending(0)));
        self.validate_view(view)?;
        Ok(SyncPage {
            items: page.items,
            examined: page.examined,
            next,
        })
    }
    fn reset_export_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.validate_view(view)?;
        *view.export_have.lock().expect("view export lock") = None;
        let preparation = view.preparation.lock().expect("view preparation lock");
        if !preparation.ready() {
            return Err(LogError::StaleCursor);
        }
        self.node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .reset_selected_export_step(preparation.selection()?, &view.repository, budget)
    }
    fn stage_export_have(&self, view: &Self::View, ids: &[EventId]) -> Result<(), LogError> {
        self.validate_view(view)?;
        let preparation = view.preparation.lock().expect("view preparation lock");
        self.node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .stage_selected_have(preparation.selection()?, &view.repository, ids)
    }
    fn prepare_export_step(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.validate_view(view)?;
        let step = {
            let preparation = view.preparation.lock().expect("view preparation lock");
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selected_export_step(
                    preparation.selection()?,
                    &view.repository,
                    view.sibling_closure,
                    !self.inbound && !preparation.replica_shared,
                    budget,
                )?
        };
        self.validate_view(view)?;
        if step.ready {
            *view.export_have.lock().expect("view export lock") = Some(
                crate::eventlog::sync::fingerprint(have.iter().copied().collect()),
            );
        }
        Ok(step)
    }
    fn export_page(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        after: Option<&ParticipantExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<ParticipantExportCursor>, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        // Have already has an exchange-owned cardinality/capacity budget. Keep
        // only its digest between pages; selected membership stays on disk.
        let fingerprint = crate::eventlog::sync::fingerprint(have.iter().copied().collect());
        if after.is_some_and(|cursor| cursor.have != fingerprint) {
            return Err(LogError::StaleCursor);
        }
        let preparation = view.preparation.lock().expect("view preparation lock");
        if !preparation.ready() {
            return Err(LogError::StaleCursor);
        }
        let prepared = view.export_have.lock().expect("view export lock");
        if *prepared != Some(fingerprint) {
            return Err(LogError::StaleCursor);
        }
        let page = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .selected_export_page(
                preparation.selection()?,
                &view.repository,
                have,
                after.map(|a| &a.retained),
                budget,
            )?;
        drop(prepared);
        drop(preparation);
        self.validate_view(view)?;
        Ok(ExportPage {
            events: page.events,
            next: page.next.map(|retained| ParticipantExportCursor {
                retained,
                have: fingerprint,
            }),
            examined: page.examined,
            encoded_bytes: page.encoded_bytes,
            allocated_bytes: page.allocated_bytes,
        })
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        #[cfg(not(test))]
        {
            SyncStore::ingest(self, event)
        }
        #[cfg(test)]
        {
            let message =
                (event.kind == EventKind::Message).then_some((event.conversation_id, event.id));
            let result = SyncStore::ingest(self, event);
            if matches!(result, Ok(AppendOutcome::Appended))
                && let Some((channel, id)) = message
            {
                // The durable append owner has returned and released its store guards.
                self.node.trace_account_client(&self.peer, || {
                    format!(
                        "gossip_diag_ciphertext_applied channel={} event={}",
                        hex::encode(channel.as_bytes()),
                        id.to_hex()
                    )
                });
            }
            result
        }
    }

    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        SyncStore::durable_have(self, conversation, id)
    }
    fn durable_have_in_view(
        &self,
        view: &Self::View,
        conversation: &ConversationId,
        id: &EventId,
    ) -> Result<bool, LogError> {
        if *conversation != view.conversation {
            return Err(LogError::StaleCursor);
        }
        self.validate_view(view)?;
        let retained = {
            let preparation = view.preparation.lock().expect("view preparation lock");
            if !preparation.ready() {
                return Err(LogError::StaleCursor);
            }
            let log = self.node.log.lock().expect("log lock");
            let selected = log
                .repository()?
                .selected_contains(preparation.selection()?, *id)?;
            if selected {
                log.sync()?;
            }
            selected
        };
        let stored = retained
            || (!self.denied
                && self
                    .node
                    .historical_file_completion(*conversation, *id, &self.peer)?);
        self.validate_view(view)?;
        Ok(stored)
    }
    fn file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<FileConsentStatus, LogError> {
        SyncStore::file_consent(self, file, manifest)
    }
    fn signed_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        SyncStore::signed_file_consent(self, file, manifest)
    }
    fn file_message_identity(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        SyncStore::file_message_identity(self, file, manifest)
    }
    fn accept_file_message_identity(&mut self, bundle: Event) -> Result<bool, LogError> {
        SyncStore::accept_file_message_identity(self, bundle)
    }
    fn sticker_file_consent(
        &mut self,
        file: &ConversationId,
        manifest: &EventId,
        headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError> {
        SyncStore::sticker_file_consent(self, file, manifest, headers)
    }
    fn sticker_file_proof(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError> {
        SyncStore::sticker_file_proof(self, file, manifest)
    }
    fn admission_denied(&self) -> bool {
        self.denied
    }
}

#[cfg(test)]
fn open_prepared(store: &GuardedSyncStore<'_>, conversation: &ConversationId) -> ParticipantView {
    let view = NetworkSyncStore::open_view(store, conversation).unwrap();
    for _ in 0..100_000 {
        if NetworkSyncStore::prepare_view_step(store, &view, PreparationBudget::default())
            .unwrap()
            .ready
        {
            return view;
        }
    }
    panic!("participant preparation did not terminate");
}

#[cfg(test)]
mod incremental_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

    pub(super) fn prepare_upload(store: &GuardedSyncStore<'_>, upload: &HistoryUpload) -> usize {
        let mut rows = 0;
        for _ in 0..10_000 {
            let budget = PreparationBudget {
                rows: 8,
                edges: 16,
                bytes: 65536,
            };
            let step = store.prepare_history_upload_step(upload, budget).unwrap();
            assert!(
                step.rows <= budget.rows
                    && step.edges <= budget.edges
                    && step.bytes <= budget.bytes
            );
            rows += step.rows;
            if step.ready {
                return rows;
            }
        }
        panic!("history upload preparation failed to progress");
    }

    pub(super) fn upload_page_budget() -> ExportPageBudget {
        ExportPageBudget {
            examined: 16,
            events: 16,
            encoded_bytes: 65536,
            allocated_bytes: 262144,
        }
    }

    fn prepare_export(
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        have: &HashSet<EventId>,
    ) {
        while !NetworkSyncStore::reset_export_step(store, view, PreparationBudget::default())
            .unwrap()
            .ready
        {}
        let ids: Vec<_> = have.iter().copied().collect();
        for ids in ids.chunks(64) {
            NetworkSyncStore::stage_export_have(store, view, ids).unwrap();
        }
        while !NetworkSyncStore::prepare_export_step(
            store,
            view,
            have,
            PreparationBudget::default(),
        )
        .unwrap()
        .ready
        {}
    }

    pub(super) fn collect(
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        have: &HashSet<EventId>,
    ) -> Vec<EventId> {
        prepare_export(store, view, have);
        let mut after = None;
        let mut result = Vec::new();
        loop {
            let page = NetworkSyncStore::export_page(
                store,
                view,
                have,
                after.as_ref(),
                ExportPageBudget {
                    examined: 1,
                    events: 1,
                    encoded_bytes: 65536,
                    allocated_bytes: 262144,
                },
            )
            .unwrap();
            result.extend(page.events.into_iter().map(|event| event.id));
            after = page.next;
            if after.is_none() {
                break;
            }
        }
        result
    }

    #[test]
    fn participant_network_pending_observation_keeps_authority_and_cold_index_fences() {
        let captured = Observation {
            source: 10,
            destructive_policy: 0,
            roster: Some([1; 32]),
            proofs: Some(1),
            file_scopes: (1, (1, 1)),
            controls: Some([-1; 32]),
            delivery_controls: 1,
            established: Some([2; 32]),
        };
        let mut appended = captured.clone();
        appended.source += 1;
        assert!(captured.permits_preparation_progress(&appended));
        let changes: [fn(&mut Observation); 9] = [
            |value| value.destructive_policy += 1,
            |value| value.roster = None,
            |value| value.proofs = Some(2),
            |value| value.file_scopes.0 += 1,
            |value| value.file_scopes.1.0 += 1,
            |value| value.file_scopes.1.1 += 1,
            |value| value.delivery_controls += 1,
            |value| value.established = None,
            |value| value.controls.as_mut().unwrap()[EventKind::ChannelReplica as usize] = 11,
        ];
        for (index, change) in changes.into_iter().enumerate() {
            let mut changed = appended.clone();
            change(&mut changed);
            assert!(
                !captured.permits_preparation_progress(&changed),
                "ordinary append hid changed authority field {index}"
            );
        }

        let mut cold = captured.clone();
        cold.controls = None;
        assert!(cold.permits_preparation_progress(&captured));
        assert!(!cold.permits_preparation_progress(&appended));
        let mut still_cold = cold.clone();
        still_cold.source += 1;
        assert!(!cold.permits_preparation_progress(&still_cold));
        assert!(!captured.permits_preparation_progress(&cold));
    }

    #[test]
    fn participant_network_preparation_survives_unrelated_ordinary_appends() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        for seq in 1..=129 {
            node.log
                .lock()
                .unwrap()
                .append_durable(Event::new(
                    &node.identity,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![7; 32],
                ))
                .unwrap();
        }
        // Derived raw-kind readiness is complete before this exchange. Ordinary
        // activity in another conversation changes no captured authority input.
        while !node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .prepare_kind_metadata_step(PreparationBudget::default())
            .unwrap()
            .ready
        {}
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
        let budget = PreparationBudget {
            rows: 8,
            edges: 16,
            bytes: 64 * 1024,
        };
        assert!(
            !NetworkSyncStore::prepare_view_step(&*store, &view, budget)
                .unwrap()
                .ready
        );
        let mut ready = false;
        for seq in 1..=1000 {
            node.log
                .lock()
                .unwrap()
                .append_durable(Event::new(
                    &node.identity,
                    ConversationId::new([91; 32]),
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![3],
                ))
                .unwrap();
            let work = NetworkSyncStore::prepare_view_step(&*store, &view, budget).expect(
                "unrelated ordinary activity must not repeatedly discard finite preparation",
            );
            assert!(
                work.rows <= budget.rows
                    && work.edges <= budget.edges
                    && work.bytes <= budget.bytes
            );
            if work.ready {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "continuous unrelated traffic starved the finite exchange"
        );
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &view).unwrap().count,
            129
        );
        NetworkSyncStore::validate_view(&*store, &view).unwrap();
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "finite preparation and validation must not decode historical bodies"
        );
    }

    #[tokio::test]
    async fn participant_network_small_ready_channel_has_no_empty_preparation_turns() {
        for retained in [8usize, 9] {
            let directory = tempfile::tempdir().unwrap();
            let peer = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
            let (node, _, _) = super::super::super::test_support::node_with_files(
                directory.path(),
                DeviceIdentity::generate(),
                Account::generate(),
                &proof,
            );
            let channel = node
                .create_channel("Small ready group", vec![peer.public()])
                .await
                .unwrap();
            while node.log.lock().unwrap().events(&channel).unwrap().len() < retained {
                node.append_event(channel, EventKind::Message, vec![1])
                    .unwrap();
            }
            node.await_channel_metadata(channel).await.unwrap();
            let owner = node.sync_store(&peer.public());
            let store = owner.lock().unwrap();
            // Prepare current kind/order/authority metadata once, as the real
            // recovery worker's completed setup does. Each later view is new.
            drop(open_prepared(&store, &channel));
            for _ in 0..3 {
                let view = NetworkSyncStore::open_view(&*store, &channel).unwrap();
                let budget = PreparationBudget::default();
                let mut turns = 0;
                loop {
                    let work = NetworkSyncStore::prepare_view_step(&*store, &view, budget).unwrap();
                    turns += 1;
                    assert!(
                        work.rows <= budget.rows
                            && work.edges <= budget.edges
                            && work.bytes <= budget.bytes
                    );
                    assert!(
                        work.ready || work.rows != 0 || work.edges != 0 || work.bytes != 0,
                        "an already-current phase must not persist and yield an empty turn"
                    );
                    assert!(
                        turns <= 2,
                        "a {retained}-event current view must finish within two measured turns"
                    );
                    if work.ready {
                        break;
                    }
                }
                assert_eq!(
                    NetworkSyncStore::metadata(&*store, &view).unwrap().count,
                    retained as u64
                );
                NetworkSyncStore::validate_view(&*store, &view).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn participant_network_canonical_authority_respects_each_preparation_budget() {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            account,
            &proof,
        );
        let channel = node
            .create_channel("Bounded authority", vec![peer.public()])
            .await
            .unwrap();
        // These retained, rejected controls must still consume canonical work.
        // Do not run the producer after appending: preparation owns this backlog.
        let mut parent = node
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .last()
            .unwrap()
            .id;
        for n in 0..129 {
            let event = Event::new(
                &peer,
                channel,
                n + 1,
                vec![parent],
                1000 + n,
                1000 + n,
                EventKind::ChannelRename,
                vec![255],
            );
            parent = event.id;
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = NetworkSyncStore::open_view(&*store, &channel).unwrap();
        let budget = PreparationBudget {
            rows: 3,
            edges: 7,
            bytes: 64 * 1024,
        };
        let mut ready = false;
        let mut turns = 0;
        for _ in 0..20_000 {
            let step = NetworkSyncStore::prepare_view_step(&*store, &view, budget).unwrap();
            assert!(
                step.rows <= budget.rows
                    && step.edges <= budget.edges
                    && step.bytes <= budget.bytes,
                "canonical authority exceeded the caller's complete turn budget"
            );
            turns += 1;
            if step.ready {
                ready = true;
                break;
            }
        }
        assert!(
            ready,
            "small turns must eventually finish canonical authority"
        );
        assert!(
            turns > 129 / budget.rows,
            "preparation silently drained rejected controls"
        );
        NetworkSyncStore::validate_view(&*store, &view).unwrap();
        let expected = store.visible_projection(&channel, false).unwrap();
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &view).unwrap().count as usize,
            expected.len(),
            "bounded authority changed the legacy permission result"
        );
    }

    #[tokio::test]
    async fn participant_network_wide_parent_preparation_honors_small_byte_turns() {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            account,
            &proof,
        );
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.own_group_proof(),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&proof)
            .unwrap();
        node.set_invisible(true).await.unwrap();
        node.account_devices
            .lock()
            .unwrap()
            .insert(super::super::super::account_devices::AccountDevice {
                public: peer.public(),
                certificate: node.account.certify(&peer.public().ed25519_pub),
            })
            .unwrap();
        node.set_device_sync_category(crate::node::SyncCategory::History, false)
            .unwrap();
        node.set_device_sync_category(crate::node::SyncCategory::Attachments, true)
            .unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let mut parents = Vec::new();
        for seq in 1..=1025 {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![1],
            );
            parents.push(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let child = Event::new(
            &node.identity,
            conversation,
            1026,
            parents.clone(),
            1026,
            0,
            EventKind::FileManifest,
            vec![2],
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(child.clone())
            .unwrap();
        let counts = || {
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts()
        };
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        assert!(
            node.privacy
                .state
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .policy
                .snapshot()
                .invisible,
            "exercise private parent authorization before export closure"
        );
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
        let budget = PreparationBudget {
            rows: 7,
            edges: 11,
            bytes: 512,
        };
        let bounded = |step: PreparationStep| {
            assert!(
                step.rows <= budget.rows
                    && step.edges <= budget.edges
                    && step.bytes <= budget.bytes,
                "every rejected or admitted edge must fit the turn"
            );
            step.ready
        };
        let before = counts();
        let mut prepared = false;
        for _ in 0..20_000 {
            if bounded(NetworkSyncStore::prepare_view_step(&*store, &view, budget).unwrap()) {
                prepared = true;
                break;
            }
        }
        assert!(
            prepared,
            "wide legal parent lists must retain resumable progress"
        );
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &view).unwrap().count,
            1026
        );
        assert_eq!(counts(), before);
        for count in [1024, 1025] {
            let have: HashSet<_> = parents[..count].iter().copied().collect();
            loop {
                let reset = NetworkSyncStore::reset_export_step(
                    &*store,
                    &view,
                    PreparationBudget {
                        bytes: 64,
                        ..budget
                    },
                )
                .unwrap();
                assert!(
                    reset.rows <= 2 && reset.bytes <= 64,
                    "replacing peer Have uses the same finite byte admission: {reset:?}"
                );
                if reset.ready {
                    break;
                }
            }
            for ids in parents[..count].chunks(64) {
                NetworkSyncStore::stage_export_have(&*store, &view, ids).unwrap();
            }
            let before = counts();
            assert!(
                matches!(
                    NetworkSyncStore::prepare_export_step(
                        &*store,
                        &view,
                        &have,
                        PreparationBudget {
                            bytes: 63,
                            ..budget
                        }
                    ),
                    Err(LogError::PageBudgetTooSmall { required: 64 })
                ),
                "a budget that cannot examine one edge must fail instead of spinning"
            );
            let mut edge_turns = 0;
            let mut ready = false;
            for _ in 0..20_000 {
                let step =
                    NetworkSyncStore::prepare_export_step(&*store, &view, &have, budget).unwrap();
                edge_turns += usize::from(step.edges > 0);
                if bounded(step) {
                    ready = true;
                    break;
                }
            }
            assert!(ready);
            assert!(
                edge_turns > 100,
                "one wide vertex must yield between edge pages"
            );
            assert_eq!(counts(), before, "Have closure reads no Event bodies");
            let mut cursor = None;
            let mut exported = Vec::new();
            loop {
                let page = NetworkSyncStore::export_page(
                    &*store,
                    &view,
                    &have,
                    cursor.as_ref(),
                    ExportPageBudget {
                        examined: 64,
                        events: 1,
                        encoded_bytes: 65536,
                        allocated_bytes: 262144,
                    },
                )
                .unwrap();
                exported.extend(page.events.into_iter().map(|event| event.id));
                cursor = page.next;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(
                exported,
                if count == 1025 {
                    vec![child.id]
                } else {
                    vec![]
                },
                "the last required Have parent decides export without losing the candidate"
            );
        }
    }

    #[test]
    fn participant_network_open_defers_history_work_at_increasing_sizes() {
        for count in [128, 512] {
            let directory = tempfile::tempdir().unwrap();
            let peer = DeviceIdentity::generate();
            let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
            let (node, _, _) = super::super::super::test_support::node_with_files(
                directory.path(),
                DeviceIdentity::generate(),
                Account::generate(),
                &proof,
            );
            let conversation = super::super::super::conversation::dm_conversation_id(
                &node.identity.public(),
                &peer.public(),
            );
            for seq in 1..=count {
                let event = Event::new(
                    &node.identity,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![7; 2048],
                );
                node.log.lock().unwrap().append_durable(event).unwrap();
            }
            let counts = || {
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts()
            };
            let owner = node.sync_store(&peer.public());
            let store = owner.lock().unwrap();
            let before = counts();
            let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
            assert_eq!(
                counts(),
                before,
                "opening {count} retained messages must only capture scalar context; bounded preparation belongs to session turns"
            );
            let mut turns = 0;
            let mut reloaded = false;
            loop {
                let budget = PreparationBudget::default();
                let step = NetworkSyncStore::prepare_view_step(&*store, &view, budget).unwrap();
                assert!(
                    step.rows <= budget.rows
                        && step.edges <= budget.edges
                        && step.bytes <= budget.bytes,
                    "every preparation turn must honor work budgets"
                );
                turns += 1;
                if !reloaded && !step.ready && view.preparation.lock().unwrap().metadata.count > 0 {
                    view.preparation
                        .lock()
                        .unwrap()
                        .discard_transient_preparation();
                    reloaded = true;
                }
                if step.ready {
                    break;
                }
                assert!(turns < 20 * count, "preparation must make finite progress");
            }
            assert!(turns >= count / 64);
            assert!(
                reloaded,
                "exercise a real durable-cursor resume before readiness"
            );
            assert_eq!(
                NetworkSyncStore::metadata(&*store, &view).unwrap().count,
                count
            );
            assert_eq!(
                counts(),
                before,
                "ordinary message preparation must only read headers"
            );
        }
    }

    #[tokio::test]
    async fn participant_network_chained_preparation_amortizes_commits_across_reloaded_turns() {
        for count in [128, 256] {
            let directory = tempfile::tempdir().unwrap();
            let peer = DeviceIdentity::generate();
            let peer_account = Account::generate();
            let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
            let (node, _, _) = super::super::super::test_support::node_with_files(
                directory.path(),
                DeviceIdentity::generate(),
                Account::generate(),
                &proof,
            );
            node.configure_privacy(
                directory.path(),
                "pw",
                &node.own_group_proof(),
                std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
            )
            .unwrap();
            node.privacy
                .state
                .write()
                .unwrap()
                .as_mut()
                .unwrap()
                .proofs
                .record(&proof)
                .unwrap();
            node.set_allowed(&peer_account.account_id(), true)
                .await
                .unwrap();
            node.set_invisible(true).await.unwrap();
            let conversation = super::super::super::conversation::dm_conversation_id(
                &node.identity.public(),
                &peer.public(),
            );
            let mut expected = Vec::new();
            for seq in 1..=count {
                let event = Event::new(
                    &node.identity,
                    conversation,
                    seq,
                    expected.last().copied().into_iter().collect(),
                    seq,
                    0,
                    EventKind::Message,
                    vec![7; 2048],
                );
                expected.push(event.id);
                node.log.lock().unwrap().append_durable(event).unwrap();
            }
            let database = node.storage.database();
            let selected_rows = || {
                database
                    .read(|c| {
                        Ok(
                            c.query_row("SELECT count(*) FROM participant_sync_rows", [], |r| {
                                r.get::<_, i64>(0)
                            })?,
                        )
                    })
                    .unwrap()
            };
            let owner = node.sync_store(&peer.public());
            let store = owner.lock().unwrap();
            let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
            let mut checked_row_turns = 0;
            let mut reloaded_turns = 0;
            let mut ready = false;
            for _ in 0..20 * count {
                let rows_before = selected_rows();
                let commits_before = database.successful_write_commits();
                let budget = PreparationBudget::default();
                let step = NetworkSyncStore::prepare_view_step(&*store, &view, budget).unwrap();
                let rows_added = selected_rows() - rows_before;
                let commits = database.successful_write_commits() - commits_before;
                assert!(
                    step.rows <= budget.rows
                        && step.edges <= budget.edges
                        && step.bytes <= budget.bytes
                );
                // A turn already inside Rows isolates row admission from cold index setup.
                if rows_before > 0 && rows_added > 1 {
                    checked_row_turns += 1;
                    assert!(
                        commits <= 2,
                        "{count} chained messages: a bounded turn admitted {rows_added} rows with {commits} durable write commits; rows and their resume cursor must commit together"
                    );
                }
                if step.ready {
                    ready = true;
                    break;
                }
                // Restart every committed row batch. The SHA accumulator is
                // intentionally transient; repeatedly restarting Hash would
                // prevent any multi-page fingerprint from reaching EOF.
                let mut preparation = view.preparation.lock().unwrap();
                if rows_added > 0 && preparation.selection.is_some() {
                    preparation.discard_transient_preparation();
                    reloaded_turns += 1;
                }
            }
            assert!(ready, "reloaded bounded preparation must finish");
            assert!(checked_row_turns > 0 && reloaded_turns > 1);
            assert_eq!(
                NetworkSyncStore::metadata(&*store, &view).unwrap().count,
                count
            );
            assert_eq!(
                collect(&store, &view, &HashSet::new()),
                expected,
                "buffered parent closure must survive every durable resume boundary"
            );
        }
    }

    #[test]
    fn participant_network_preparation_rolls_back_rows_on_cursor_failure() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let mut expected = Vec::new();
        for seq in 1..=8 {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![1; 2048],
            );
            expected.push(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
        let single_row = PreparationBudget {
            rows: 1,
            ..PreparationBudget::default()
        };
        for _ in 0..100 {
            NetworkSyncStore::prepare_view_step(&*store, &view, single_row).unwrap();
            if view.preparation.lock().unwrap().metadata.count == 1 {
                break;
            }
        }
        assert_eq!(view.preparation.lock().unwrap().metadata.count, 1);
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TEMP TRIGGER test_fail_selection_progress BEFORE UPDATE ON participant_sync_progress BEGIN SELECT RAISE(ABORT,'injected selection cursor failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(
            NetworkSyncStore::prepare_view_step(&*store, &view, PreparationBudget::default())
                .is_err()
        );
        assert_eq!(
            node.storage
                .database()
                .read(|c| {
                    c.query_row("SELECT count(*) FROM participant_sync_rows", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(Into::into)
                })
                .unwrap(),
            1,
            "row admission and the resume cursor must both roll back on a failed save"
        );
        assert!(
            !view.preparation.lock().unwrap().ready(),
            "failed cursor save cannot publish a ready view"
        );
        node.storage
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER test_fail_selection_progress")?;
                Ok(())
            })
            .unwrap();
        for _ in 0..100 {
            if NetworkSyncStore::prepare_view_step(&*store, &view, PreparationBudget::default())
                .unwrap()
                .ready
            {
                break;
            }
        }
        assert_eq!(NetworkSyncStore::metadata(&*store, &view).unwrap().count, 8);
        assert_eq!(collect(&store, &view, &HashSet::new()), expected);
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let ordinary = Event::new(
            &node.identity,
            conversation,
            9,
            vec![],
            9,
            0,
            EventKind::Message,
            vec![1],
        );
        node.log.lock().unwrap().append_durable(ordinary).unwrap();
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.9".parse().unwrap(), &node.user_id());
        NetworkSyncStore::validate_view(&*store, &view).unwrap();
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &view).unwrap().count,
            8,
            "ordinary append preserves the finite cutoff"
        );
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before
        );
        let preparing = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
        NetworkSyncStore::prepare_view_step(&*store, &preparing, PreparationBudget::default())
            .unwrap();
        let changed = Event::new(
            &node.identity,
            conversation,
            10,
            vec![],
            10,
            0,
            EventKind::ChannelReplica,
            vec![0],
        );
        node.log.lock().unwrap().append_durable(changed).unwrap();
        assert!(matches!(
            NetworkSyncStore::prepare_view_step(&*store, &preparing, PreparationBudget::default()),
            Err(LogError::StaleCursor)
        ));
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &view),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "generation invalidation must not hydrate history"
        );
    }

    #[test]
    fn participant_network_sibling_selection_recomputes_exact_have_ancestry() {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            account,
            &proof,
        );
        node.account_devices
            .lock()
            .unwrap()
            .insert(super::super::super::account_devices::AccountDevice {
                public: peer.public(),
                certificate: node.account.certify(&peer.public().ed25519_pub),
            })
            .unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        node.set_device_sync_category(crate::node::SyncCategory::History, false)
            .unwrap();
        node.set_device_sync_category(crate::node::SyncCategory::Attachments, true)
            .unwrap();
        let a = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            100,
            0,
            EventKind::Message,
            vec![1],
        );
        let b = Event::new(
            &node.identity,
            conversation,
            2,
            vec![a.id],
            1,
            0,
            EventKind::FileManifest,
            vec![2],
        );
        let c = Event::new(
            &node.identity,
            conversation,
            3,
            vec![b.id],
            2,
            0,
            EventKind::FileManifest,
            vec![3],
        );
        for e in [&a, &b, &c] {
            node.log.lock().unwrap().append_durable(e.clone()).unwrap();
        }
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        let mut upload = store.open_history_upload(&conversation).unwrap();
        prepare_upload(&store, &upload);
        let initial_upload = store
            .history_upload_page(&upload, -1, upload_page_budget())
            .unwrap();
        assert!(
            initial_upload.events.is_empty() && initial_upload.complete,
            "sibling upload must retain the missing history-parent fence"
        );
        for (have, expected) in [
            (HashSet::new(), vec![]),
            (HashSet::from([a.id]), vec![b.id, c.id]),
            (HashSet::from([a.id, b.id]), vec![c.id]),
            (HashSet::new(), vec![]),
        ] {
            let before = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts();
            assert_eq!(collect(&store, &view, &have), expected);
            let after = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts();
            assert_eq!(
                after.0 - before.0,
                expected.len(),
                "closure must use headers/edges, even when Have changes"
            );
        }
        prepare_export(&store, &view, &HashSet::from([a.id]));
        let first = NetworkSyncStore::export_page(
            &*store,
            &view,
            &HashSet::from([a.id]),
            None,
            ExportPageBudget {
                examined: 1,
                events: 1,
                encoded_bytes: 65536,
                allocated_bytes: 262144,
            },
        )
        .unwrap();
        assert!(first.next.is_some());
        assert!(matches!(
            NetworkSyncStore::export_page(
                &*store,
                &view,
                &HashSet::new(),
                first.next.as_ref(),
                ExportPageBudget {
                    examined: 1,
                    events: 1,
                    encoded_bytes: 65536,
                    allocated_bytes: 262144
                }
            ),
            Err(LogError::StaleCursor)
        ));
        node.log
            .lock()
            .unwrap()
            .append_durable(Event::new(
                &node.identity,
                conversation,
                4,
                vec![c.id],
                101,
                0,
                EventKind::FileManifest,
                vec![4],
            ))
            .unwrap();
        assert!(store.resume_history_upload(&mut upload).unwrap());
        prepare_upload(&store, &upload);
        let blocked = store
            .history_upload_page(&upload, initial_upload.next_rank, upload_page_budget())
            .unwrap();
        assert!(
            blocked.events.is_empty() && blocked.complete,
            "resumed sibling closure must not release a tail with an excluded ancestor"
        );
        node.set_device_sync_category(crate::node::SyncCategory::Attachments, false)
            .unwrap();
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &view),
            Err(LogError::StaleCursor)
        ));
    }

    #[tokio::test]
    async fn participant_network_parent_retirement_invalidates_prepared_file_authorization() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let peer_account = Account::generate();
        let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.own_group_proof(),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&proof)
            .unwrap();
        node.set_allowed(&peer_account.account_id(), true)
            .await
            .unwrap();
        node.set_invisible(true).await.unwrap();
        let parent = ConversationId::new([190; 32]);
        let genesis = Event::new(
            &node.identity,
            parent,
            1,
            vec![],
            1,
            0,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "retirement dependency".into(),
                members: vec![node.identity.public(), peer.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        node.log.lock().unwrap().append_durable(genesis).unwrap();
        node.process_channel(parent);
        let path = directory.path().join("payload.txt");
        std::fs::write(&path, b"file authorization belongs to its live parent").unwrap();
        let file = node
            .send_file_channel(parent, &path, crate::file::FileKind::File)
            .await
            .unwrap();
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &file);
        assert!(NetworkSyncStore::metadata(&*store, &view).unwrap().count > 0);
        let have = HashSet::new();
        prepare_export(&store, &view, &have);
        let scopes = node.file_scope_revision().unwrap();
        let transcript_rows = node.received_files.lock().unwrap().count(None).unwrap();
        let parent_rows = || {
            node.storage
                .database()
                .read(|c| {
                    c.query_row(
                        "SELECT count(*) FROM retained_events WHERE conversation=?1",
                        [parent.as_bytes().as_slice()],
                        |r| r.get::<_, i64>(0),
                    )
                    .map_err(Into::into)
                })
                .unwrap()
        };
        let physical_parent_rows = parent_rows();
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(matches!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .begin_retirement(&parent)
                .unwrap(),
            crate::eventlog::repository::retirement::RetirementBegin::Started(_)
        ));
        assert_eq!(
            parent_rows(),
            physical_parent_rows,
            "test the logical boundary before any physical deletion"
        );
        assert_eq!(
            node.file_scope_revision().unwrap(),
            scopes,
            "Book and fanout scopes remain unchanged"
        );
        assert_eq!(
            node.received_files.lock().unwrap().count(None).unwrap(),
            transcript_rows
        );
        NetworkSyncStore::validate_view(
            node.log.lock().unwrap().repository().unwrap(),
            &view.repository,
        )
        .unwrap();
        let metadata_stale = matches!(
            NetworkSyncStore::metadata(&*store, &view),
            Err(LogError::StaleCursor)
        );
        let have_stale = matches!(
            NetworkSyncStore::have_page(
                &*store,
                &view,
                None,
                IdPageBudget {
                    ids: 1,
                    examined: 1
                }
            ),
            Err(LogError::StaleCursor)
        );
        let export_stale = matches!(
            NetworkSyncStore::export_page(
                &*store,
                &view,
                &have,
                None,
                ExportPageBudget {
                    examined: 64,
                    events: 1,
                    encoded_bytes: 65536,
                    allocated_bytes: 262144
                }
            ),
            Err(LogError::StaleCursor)
        );
        assert!(
            metadata_stale && have_stale && export_stale,
            "retired parent must invalidate all ready FILES selectors: metadata={metadata_stale}, Have={have_stale}, export={export_stale}"
        );
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "destructive dependency validation must stay metadata-only"
        );
    }

    #[tokio::test]
    async fn participant_network_share_off_closure_and_source_revocation_are_fresh() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = ConversationId::new([193; 32]);
        let genesis = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            100,
            0,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "selection".into(),
                members: vec![node.identity.public(), peer.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        let share_off = Event::new(
            &node.identity,
            conversation,
            2,
            vec![genesis.id],
            1,
            0,
            EventKind::ChannelReplica,
            vec![0],
        );
        let third_party = Event::new(
            &peer,
            conversation,
            1,
            vec![share_off.id],
            2,
            0,
            EventKind::Message,
            vec![1],
        );
        let own = Event::new(
            &node.identity,
            conversation,
            3,
            vec![third_party.id],
            3,
            0,
            EventKind::Message,
            vec![2],
        );
        let tail = Event::new(
            &node.identity,
            conversation,
            4,
            vec![own.id],
            4,
            0,
            EventKind::Message,
            vec![3],
        );
        for e in [&genesis, &share_off, &third_party, &own, &tail] {
            node.log.lock().unwrap().append_durable(e.clone()).unwrap();
        }
        node.process_channel(conversation);
        crate::node::test_support::settle_group_metadata(&node, conversation);
        assert!(!node.shares_replica(&conversation).unwrap());
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        assert_eq!(
            collect(&store, &view, &HashSet::new()),
            vec![genesis.id, share_off.id]
        );
        let upload = store.open_history_upload(&conversation).unwrap();
        prepare_upload(&store, &upload);
        let upload_page = store
            .history_upload_page(&upload, -1, upload_page_budget())
            .unwrap();
        assert_eq!(
            upload_page
                .events
                .iter()
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            vec![genesis.id, share_off.id],
            "upload retains the existing share-off parent closure"
        );
        assert_eq!(
            collect(
                &store,
                &view,
                &HashSet::from([genesis.id, share_off.id, third_party.id])
            ),
            vec![own.id, tail.id]
        );
        let mut cursor = None;
        let mut have = Vec::new();
        loop {
            let page = NetworkSyncStore::have_page(
                &*store,
                &view,
                cursor.as_ref(),
                IdPageBudget {
                    ids: 2,
                    examined: 2,
                },
            )
            .unwrap();
            have.extend(page.items);
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(
            have,
            vec![genesis.id, share_off.id, third_party.id, own.id, tail.id],
            "share-off does not hide possession"
        );
        // Append directly, before ChannelBook catches up: durable authority wins.
        let removed = Event::new(
            &node.identity,
            conversation,
            5,
            vec![tail.id],
            5,
            0,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "selection".into(),
                members: vec![node.identity.public()],
                epoch: 1,
                owner: node.user_id(),
            }
            .encode(),
        );
        node.log.lock().unwrap().append_durable(removed).unwrap();
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &view),
            Err(LogError::StaleCursor)
        ));
    }

    #[test]
    fn participant_network_pending_invites_and_new_capabilities_invalidate_views() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = ConversationId::new([194; 32]);
        let owner = node.sync_store(&peer.public());
        let mut store = owner.lock().unwrap();
        let staged = Event::new(
            &peer,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::MembershipChange,
            vec![1],
        );
        store.pending_channels.insert(
            conversation,
            super::super::PendingChannel {
                events: vec![staged.clone()],
                ..Default::default()
            },
        );
        let view = open_prepared(&store, &conversation);
        assert_eq!(NetworkSyncStore::metadata(&*store, &view).unwrap().count, 1);
        let first = NetworkSyncStore::have_page(
            &*store,
            &view,
            None,
            IdPageBudget {
                ids: 1,
                examined: 1,
            },
        )
        .unwrap();
        let pending = NetworkSyncStore::have_page(
            &*store,
            &view,
            first.next.as_ref(),
            IdPageBudget {
                ids: 1,
                examined: 1,
            },
        )
        .unwrap();
        assert_eq!(pending.items, vec![staged.id]);
        assert!(pending.next.is_none());
        store.pending_channels.clear();
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &view),
            Err(LogError::StaleCursor)
        ));
        drop(view);
        let view = open_prepared(&store, &conversation);
        let capability = Event::new(
            &peer,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::ChannelReplica,
            vec![1],
        );
        node.log.lock().unwrap().append_durable(capability).unwrap();
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &view),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "capability invalidation uses fixed metadata"
        );
    }
    #[tokio::test]
    async fn participant_network_proof_rehome_invalidates_without_source_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let peer_account = Account::generate();
        let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.own_group_proof(),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&proof)
            .unwrap();
        node.set_invisible(true).await.unwrap();
        node.set_allowed(&peer_account.account_id(), true)
            .await
            .unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let event = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1],
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(event.clone())
            .unwrap();
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        assert_eq!(collect(&store, &view, &HashSet::new()), vec![event.id]);
        let source = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap();
        let generation = node.privacy.generation.load(Ordering::SeqCst);
        let rehomed = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        node.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record_explicit_account(&rehomed)
            .unwrap();
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .source_revision()
                .unwrap(),
            source
        );
        assert_eq!(node.privacy.generation.load(Ordering::SeqCst), generation);
        assert!(
            matches!(
                NetworkSyncStore::validate_view(&*store, &view),
                Err(LogError::StaleCursor)
            ),
            "fresh proof revision must prevent old account authorization from surviving a rehome"
        );
    }
    #[test]
    fn participant_network_authorized_capability_is_not_masked_by_rejected_raw_control() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = ConversationId::new([195; 32]);
        let genesis = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "capability".into(),
                members: vec![node.identity.public(), peer.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        let rejected = Event::new(
            &node.identity,
            conversation,
            2,
            vec![genesis.id],
            2,
            0,
            EventKind::ChannelAccountAuthority,
            b"invalid account authority".to_vec(),
        );
        for event in [&genesis, &rejected] {
            node.log
                .lock()
                .unwrap()
                .append_durable(event.clone())
                .unwrap();
        }
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        let metadata = NetworkSyncStore::metadata(&*store, &view).unwrap();
        assert!(
            !metadata.channel_v2
                && !metadata.channel_v3
                && !metadata.channel_v4
                && !metadata.channel_v5
        );
        let valid = Event::new(
            &node.identity,
            conversation,
            3,
            vec![genesis.id],
            3,
            0,
            EventKind::ChannelReplica,
            vec![1],
        );
        node.log.lock().unwrap().append_durable(valid).unwrap();
        assert!(
            matches!(
                NetworkSyncStore::validate_view(&*store, &view),
                Err(LogError::StaleCursor)
            ),
            "new authorized capability must invalidate the V1 view even when raw metadata already contains rejected V4"
        );
    }
}

#[cfg(test)]
mod control_publication_tests {
    use super::*;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

    #[test]
    fn participant_network_control_publication_invalidates_view_opened_after_privacy_bump() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let peer_account = Account::generate();
        let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let original_conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let original = Event::new(
            &peer,
            original_conversation,
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![1],
        );
        let accepted = super::super::super::received_log::ReceivedEntry {
            conversation: original_conversation,
            event_id: original.id,
            wall_clock: original.wall_clock,
            from: peer.public().user_id(),
            plaintext: b"accepted message".to_vec(),
        };
        let payload = super::super::super::delivery_receipt::ReceiptPayload::prepare(
            &node.identity.public(),
            &node.account_id(),
            &proof,
            &original,
            &accepted,
            2,
        )
        .unwrap();
        let conversation = payload.conversation();
        let receipt = Event::new(
            &node.identity,
            conversation,
            1,
            vec![],
            1,
            2,
            EventKind::Message,
            payload.seal(&node.identity).unwrap(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(receipt.clone())
            .unwrap();
        let binding = super::super::super::delivery_store::ControlBinding {
            device: peer.public(),
            account: Some(peer_account.account_id()),
        };
        node.delivery_control_ids
            .lock()
            .unwrap()
            .insert(receipt.id, binding.clone());
        // Pause cancellation at its real publication window: the generation is
        // already new, while the previously authorized control map is visible.
        node.privacy.generation.fetch_add(1, Ordering::SeqCst);
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        let read_counts = || {
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts()
        };
        let before = read_counts();
        NetworkSyncStore::validate_view(&*store, &view).unwrap();
        let have = NetworkSyncStore::have_page(
            &*store,
            &view,
            None,
            IdPageBudget {
                ids: 1,
                examined: 1,
            },
        )
        .unwrap();
        assert_eq!(have.items, vec![receipt.id]);
        assert_eq!(
            read_counts(),
            before,
            "unchanged control checks must not scan history"
        );
        let generation = node.privacy.generation.load(Ordering::SeqCst);
        let source = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .source_revision()
            .unwrap();
        node.delivery_control_ids
            .lock()
            .unwrap()
            .replace(Default::default());
        assert_eq!(node.privacy.generation.load(Ordering::SeqCst), generation);
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .source_revision()
                .unwrap(),
            source
        );
        assert!(
            matches!(
                NetworkSyncStore::validate_view(&*store, &view),
                Err(LogError::StaleCursor)
            ),
            "control publication after view capture must revoke the old SQL selection"
        );
        assert!(
            matches!(
                NetworkSyncStore::export_page(
                    &*store,
                    &view,
                    &HashSet::new(),
                    None,
                    ExportPageBudget {
                        examined: 1,
                        events: 1,
                        encoded_bytes: 65536,
                        allocated_bytes: 262144
                    }
                ),
                Err(LogError::StaleCursor)
            ),
            "erased control must not leave the old selection"
        );
        drop(view);
        let empty = open_prepared(&store, &conversation);
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &empty).unwrap().count,
            0
        );
        // Recovery publishes an individual binding without rewriting event source.
        node.delivery_control_ids
            .lock()
            .unwrap()
            .insert(receipt.id, binding);
        assert!(matches!(
            NetworkSyncStore::validate_view(&*store, &empty),
            Err(LogError::StaleCursor)
        ));
    }
    #[tokio::test]
    async fn participant_probe_metadata_rejects_cold_and_appended_finite_views() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let append = |seq| {
            node.log
                .lock()
                .unwrap()
                .append_durable(Event::new(
                    &node.identity,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        };
        append(1);
        // Model a legitimate pre-existing backfill backlog even when the fresh
        // fixture's insert trigger already advanced its initially empty index.
        node.storage
            .database()
            .transaction(|connection| {
                connection.execute("UPDATE participant_kind_scan SET ordinal=-1", [])?;
                Ok(())
            })
            .unwrap();
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let cold = open_prepared(&store, &conversation);
        assert!(
            cold.metadata_stamp.is_none(),
            "fixture must start without raw-kind evidence"
        );
        assert_eq!(NetworkSyncStore::metadata(&*store, &cold).unwrap().count, 1);
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_none(),
            "finishing preparation must never upgrade an initially cold stamp"
        );
        drop(cold);

        // Finish A without publishing metadata. An append before the first
        // preparation turn may fail closed when its newer order exceeds A's
        // cutoff; that existing contract is separate from cache publication.
        let finite = open_prepared(&store, &conversation);
        assert!(finite.metadata_stamp.is_some());
        append(2);
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &finite).unwrap().count,
            1
        );
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_none(),
            "an old finite cutoff cannot acquire the current appended history stamp"
        );
        let current = open_prepared(&store, &conversation);
        assert_eq!(
            NetworkSyncStore::metadata(&*store, &current).unwrap().count,
            2
        );
        assert_eq!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .unwrap()
                .count,
            2
        );
    }

    #[tokio::test]
    async fn participant_probe_metadata_scopes_full_peer_direction_controls_and_deletion() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(Event::new(
                &node.identity,
                conversation,
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                vec![1],
            ))
            .unwrap();
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        drop(open_prepared(&store, &conversation)); // settle the initially cold index
        let ready = open_prepared(&store, &conversation);
        NetworkSyncStore::metadata(&*store, &ready).unwrap();
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_some()
        );
        let mut other_transport = peer.public();
        other_transport.x25519_pub = DeviceIdentity::generate().public().x25519_pub;
        for other in [other_transport, DeviceIdentity::generate().public()] {
            assert!(
                NetworkSyncStore::probe_metadata(
                    &*node.sync_store(&other).lock().unwrap(),
                    &conversation
                )
                .unwrap()
                .is_none(),
                "both peer keys must qualify the scalar cache"
            );
        }
        for inbound in [
            node.serve_sync_store(&peer.public()),
            node.pull_sync_store(&peer.public()),
        ] {
            assert!(
                NetworkSyncStore::probe_metadata(&*inbound.lock().unwrap(), &conversation)
                    .unwrap()
                    .is_none(),
                "inbound and pull-only stores have separate authority"
            );
        }
        let control = Event::new(
            &node.identity,
            ConversationId::new([217; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::ChannelReplica,
            vec![0],
        );
        node.log.lock().unwrap().append_durable(control).unwrap();
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_none(),
            "even a rejected raw control invalidates qualified metadata"
        );
        let refreshed = open_prepared(&store, &conversation);
        NetworkSyncStore::metadata(&*store, &refreshed).unwrap();
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_some()
        );
        node.clear_conversation(conversation).unwrap();
        assert!(
            NetworkSyncStore::probe_metadata(&*store, &conversation)
                .unwrap()
                .is_none(),
            "destructive history changes must invalidate previously qualified inventory"
        );
    }

    #[test]
    fn participant_probe_metadata_shares_legacy_fifo_bound_without_touching_hits() {
        use super::super::super::channel_projection_cache::{
            ChannelProjectionCache, ProjectionKey, ProjectionMetadata,
        };
        let mut cache = ChannelProjectionCache::default();
        let own = DeviceIdentity::generate().public();
        let peer = DeviceIdentity::generate().public();
        let key = |index| ProjectionKey {
            conversation: ConversationId::new([index; 32]),
            raw: ([0; 32], 0),
            peer: peer.clone(),
            own: own.clone(),
            own_account: "owner".into(),
            established: None,
            privacy_generation: 0,
            privacy_configured: false,
            inbound: false,
            sharing: false,
        };
        let legacy = ProjectionMetadata {
            fingerprint: [0; 32],
            len: 0,
            requires_v2: false,
            requires_v3: false,
            requires_v4: false,
            requires_v5: false,
        };
        for index in 0..64 {
            cache.insert(key(index), legacy);
        }
        let metadata = SyncMetadata {
            fingerprint: Some([1; 32]),
            count: 1,
            ..Default::default()
        };
        for index in 0..64 {
            cache.insert_paged(
                HistorySyncStamp {
                    fingerprint: [index; 32],
                },
                metadata,
            );
        }
        assert_eq!(cache.len(), 128);
        let oldest = HistorySyncStamp {
            fingerprint: [0; 32],
        };
        for _ in 0..3 {
            assert!(cache.get(&key(0)).is_some());
            assert!(cache.get_paged(&oldest).is_some());
        }
        cache.insert_paged(
            HistorySyncStamp {
                fingerprint: [64; 32],
            },
            metadata,
        );
        assert_eq!(cache.len(), 128);
        assert!(
            cache.get(&key(0)).is_none(),
            "reads must not change FIFO eviction"
        );
        assert!(cache.get_paged(&oldest).is_some());
        for index in 65..128 {
            cache.insert_paged(
                HistorySyncStamp {
                    fingerprint: [index; 32],
                },
                metadata,
            );
        }
        cache.insert_paged(
            HistorySyncStamp {
                fingerprint: [128; 32],
            },
            metadata,
        );
        assert_eq!(cache.len(), 128);
        assert!(cache.get_paged(&oldest).is_none());
        cache.insert_paged(
            HistorySyncStamp {
                fingerprint: [129; 32],
            },
            SyncMetadata::default(),
        );
        assert_eq!(cache.len(), 128);
        assert!(
            cache
                .get_paged(&HistorySyncStamp {
                    fingerprint: [129; 32]
                })
                .is_none(),
            "history-only metadata without a fingerprint cannot enter the cache"
        );
    }

    /// Exercise fresh store wrappers on both sides of an authenticated equal-FP
    /// exchange. Only scalar metadata can survive between these independent views.
    #[tokio::test]
    async fn participant_probe_metadata_loopback_reuses_without_selection_commits() {
        use crate::{
            node::session::{Served, request_round_paged, serve_one_paged},
            transport::SecureChannel,
        };
        for history in [32, 128] {
            let left_dir = tempfile::tempdir().unwrap();
            let right_dir = tempfile::tempdir().unwrap();
            let left_identity = DeviceIdentity::generate();
            let right_identity = DeviceIdentity::generate();
            let left_account = Account::generate();
            let right_account = Account::generate();
            let left_proof = Announce::new_with_account(&left_identity, &left_account, "Left", 9);
            let right_proof =
                Announce::new_with_account(&right_identity, &right_account, "Right", 9);
            let (left, _, _) = super::super::super::test_support::node_with_files(
                left_dir.path(),
                left_identity,
                left_account,
                &right_proof,
            );
            let (right, _, _) = super::super::super::test_support::node_with_files(
                right_dir.path(),
                right_identity,
                right_account,
                &left_proof,
            );
            left.cached_peer_snapshot_async().await.unwrap();
            right.cached_peer_snapshot_async().await.unwrap();
            let conversation = super::super::super::conversation::dm_conversation_id(
                &left.identity.public(),
                &right.identity.public(),
            );
            let append_shared = |seq| {
                let event = Event::new(
                    &left.identity,
                    conversation,
                    seq,
                    vec![],
                    seq,
                    0,
                    EventKind::Message,
                    vec![7; 128],
                );
                for node in [&left, &right] {
                    node.log
                        .lock()
                        .unwrap()
                        .append_durable(event.clone())
                        .unwrap();
                }
            };
            for seq in 1..=history {
                append_shared(seq);
            }
            // A cold raw-kind index deliberately cannot qualify a reusable stamp.
            let settle = || {
                for node in [&left, &right] {
                    while !node
                        .log
                        .lock()
                        .unwrap()
                        .repository()
                        .unwrap()
                        .prepare_kind_metadata_step(PreparationBudget::default())
                        .unwrap()
                        .ready
                    {}
                }
            };
            settle();
            let commits = || {
                (
                    left.storage.database().successful_write_commits(),
                    right.storage.database().successful_write_commits(),
                )
            };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_node = right.clone();
            let remote = left.identity.public();
            let (served, mut completed) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(async move {
                let (server, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(server, &server_node.identity)
                    .await
                    .unwrap();
                loop {
                    // No selection or per-GuardedSyncStore cache crosses a round.
                    let store = server_node.serve_sync_store(&remote);
                    if matches!(
                        serve_one_paged(&mut channel, &store).await.unwrap(),
                        Served::Closed
                    ) {
                        break;
                    }
                    // The probe view's destructor has completed before baseline reads.
                    served.send(()).unwrap();
                }
            });
            let client = tokio::net::TcpStream::connect(address).await.unwrap();
            let mut channel =
                SecureChannel::connect(client, &left.identity, Some(&right.identity.public()))
                    .await
                    .unwrap();
            for phase in 0..3 {
                if phase == 1 {
                    append_shared(history + 1);
                    settle();
                } else if phase == 2 {
                    // Revocation generation changes invalidate even equal inventory.
                    for node in [&left, &right] {
                        node.privacy.generation.fetch_add(1, Ordering::SeqCst);
                    }
                }
                let before = commits();
                request_round_paged(
                    &mut channel,
                    &left.sync_store(&right.identity.public()),
                    conversation,
                )
                .await
                .unwrap();
                completed.recv().await.unwrap();
                let warmed = commits();
                assert!(
                    warmed.0 > before.0 && warmed.1 > before.1,
                    "new exact scope must prepare on both participants, history={history}, phase={phase}"
                );
                for _ in 0..2 {
                    request_round_paged(
                        &mut channel,
                        &left.sync_store(&right.identity.public()),
                        conversation,
                    )
                    .await
                    .unwrap();
                    completed.recv().await.unwrap();
                    assert_eq!(
                        commits(),
                        warmed,
                        "quiet equal-fingerprint probes must not rebuild selections, history={history}, phase={phase}"
                    );
                }
            }
            drop(channel);
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn participant_durable_have_connection_reuses_selection_and_rechecks_authority() {
        use crate::{
            node::session::{Served, request_durable_have, serve_one_paged},
            transport::SecureChannel,
        };
        async fn query(
            channel: &mut SecureChannel<tokio::net::TcpStream>,
            completed: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
            conversation: ConversationId,
            event: EventId,
        ) -> bool {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                request_durable_have(channel, conversation, event),
            )
            .await
            .expect("custody request stalled")
            .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(10), completed.recv())
                .await
                .expect("serving completion stalled")
                .expect("server exited");
            result
        }
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let peer_account = Account::generate();
        let proof = Announce::new_with_account(&peer, &peer_account, "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.configure_privacy(
            directory.path(),
            "pw",
            &node.own_group_proof(),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&proof)
            .unwrap();
        node.set_allowed(&peer_account.account_id(), true)
            .await
            .unwrap();
        node.set_invisible(true).await.unwrap();
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let append = |conversation, seq| {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![7; 2048],
            );
            let id = event.id;
            node.log.lock().unwrap().append_durable(event).unwrap();
            id
        };
        let mut selected = None;
        for seq in 1..=128 {
            selected = Some(append(conversation, seq));
        }
        let selected = selected.unwrap();
        let missing = EventId::new([93; 32]);
        let counters = || {
            (
                node.storage
                    .database()
                    .read(|c| Ok(c.total_changes()))
                    .unwrap(),
                node.storage.database().successful_write_commits(),
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts(),
            )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server_node = node.clone();
        let remote = peer.public();
        let (served, mut completed) = tokio::sync::mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let (server, _) = listener.accept().await.unwrap();
            let mut channel = SecureChannel::accept(server, &server_node.identity)
                .await
                .unwrap();
            // Production owns one store for the lifetime of this connection.
            let store = server_node.serve_sync_store(&remote);
            loop {
                if matches!(
                    serve_one_paged(&mut channel, &store).await.unwrap(),
                    Served::Closed
                ) {
                    break;
                }
                served.send(()).unwrap();
            }
        });
        let client = tokio::net::TcpStream::connect(address).await.unwrap();
        let mut channel = SecureChannel::connect(client, &peer, Some(&node.identity.public()))
            .await
            .unwrap();
        // First preparation may have an unqualified cold metadata stamp.
        for _ in 0..3 {
            assert!(query(&mut channel, &mut completed, conversation, selected).await);
        }
        let warm = counters();
        for _ in 0..3 {
            assert!(query(&mut channel, &mut completed, conversation, selected).await);
            assert!(!query(&mut channel, &mut completed, conversation, missing).await);
        }
        assert_eq!(
            counters(),
            warm,
            "warm present/missing custody requests must not rewrite selections or read historical bodies"
        );

        append(ConversationId::new([203; 32]), 1);
        let unrelated = counters();
        assert!(query(&mut channel, &mut completed, conversation, selected).await);
        assert_eq!(
            counters(),
            unrelated,
            "unrelated ordinary append must retain custody preparation"
        );

        let tail = append(conversation, 129);
        let changed = counters();
        assert!(
            query(&mut channel, &mut completed, conversation, tail).await,
            "newly appended retained events must not be hidden by the old finite selection"
        );
        let prepared = counters();
        assert!(
            prepared.0 > changed.0 && prepared.1 > changed.1,
            "changed conversation must prepare a current authorization selection"
        );
        for _ in 0..3 {
            assert!(query(&mut channel, &mut completed, conversation, tail).await);
        }
        assert_eq!(
            counters(),
            prepared,
            "refreshed custody preparation must become quiet again"
        );

        node.set_allowed(&peer_account.account_id(), false)
            .await
            .unwrap();
        assert!(
            !query(&mut channel, &mut completed, conversation, selected).await,
            "warm positive custody must not outlive current peer authority"
        );
        drop(channel);
        tokio::time::timeout(std::time::Duration::from_secs(10), task)
            .await
            .expect("connection owner failed to retire")
            .unwrap();
        let active: i64 = node
            .storage
            .database()
            .read(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM participant_sync_views WHERE retired=0",
                    [],
                    |r| r.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(
            active, 0,
            "closed connection must release its cached selection lease"
        );
    }

    #[tokio::test]
    async fn participant_durable_have_request_uses_no_historical_bodies() {
        use crate::{
            node::session::{request_durable_have, serve_one_paged},
            transport::SecureChannel,
        };
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let conversation = super::super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &peer.public(),
        );
        let mut selected = None;
        for seq in 1..=128 {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![7; 2048],
            );
            selected = Some(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let (client, server) = tokio::io::duplex(65536);
        let server_node = node.clone();
        let remote = peer.public();
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(server, &server_node.identity)
                .await
                .unwrap();
            let store = server_node.sync_store(&remote);
            for _ in 0..2 {
                serve_one_paged(&mut channel, &store).await.unwrap();
            }
        });
        let mut channel = SecureChannel::connect(client, &peer, Some(&node.identity.public()))
            .await
            .unwrap();
        assert!(
            request_durable_have(&mut channel, conversation, selected.unwrap())
                .await
                .unwrap()
        );
        assert!(
            !request_durable_have(&mut channel, conversation, EventId::new([93; 32]))
                .await
                .unwrap()
        );
        task.await.unwrap();
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "a selected custody probe must not decode retained history"
        );
    }
}

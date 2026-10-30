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

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Policy {
    privacy_generation: u64,
    pending: [u8; 32],
    pub(super) network_paused: bool,
    pub(super) sibling: bool,
    device_policy: u64,
}
#[derive(Clone, PartialEq, Eq)]
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
pub(in crate::node) struct ParticipantView {
    pub(super) conversation: ConversationId,
    pub(super) repository: NetworkView,
    pub(super) preparation: Mutex<super::principal::Preparation>,
    pub(super) policy: Policy,
    observed: Mutex<Observation>,
    // Staged invitations already have an 8 MiB owner budget.
    pub(super) pending: Vec<EventId>,
    export_have: Mutex<Option<[u8; 32]>>,
    sibling_closure: bool,
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
    fn open_view(&self, conversation: &ConversationId) -> Result<Self::View, LogError> {
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
        Ok(ParticipantView {
            conversation: *conversation,
            repository,
            preparation: Mutex::new(super::principal::Preparation::default()),
            policy,
            observed: Mutex::new(observed),
            pending,
            export_have: Mutex::new(None),
            sibling_closure,
        })
    }
    fn prepare_view_step(
        &self,
        view: &Self::View,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        self.validate_view(view)?;
        let before = self.network_observation(&view.conversation)?;
        let step = {
            let mut preparation = view.preparation.lock().expect("view preparation lock");
            preparation.step(self, view, budget)?
        };
        let after = self.network_observation(&view.conversation)?;
        let mut expected = before;
        // Bounded cold metadata backfill changes only derived kind readiness.
        expected.controls = after.controls;
        if expected != after || !self.policy_unchanged(&view.conversation, &view.policy) {
            return Err(LogError::StaleCursor);
        }
        if step.ready {
            *view.observed.lock().expect("view observation lock") = after;
        }
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
        let mut expected = observed.clone();
        if ready {
            // Ordinary appends preserve the finite cutoff. Raw control changes,
            // including rejected controls, and all mutable policy owners do not.
            expected.source = current.source;
        } else {
            expected.controls = current.controls;
        }
        if expected != current {
            return Err(LogError::StaleCursor);
        }
        if ready {
            *observed = current;
        }
        Ok(())
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError> {
        self.validate_view(view)?;
        let preparation = view.preparation.lock().expect("view preparation lock");
        if !preparation.ready() {
            return Err(LogError::StaleCursor);
        }
        Ok(preparation.metadata)
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
        SyncStore::ingest(self, event)
    }
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        SyncStore::durable_have(self, conversation, id)
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
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

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

    fn collect(
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
                "every rejected or admitted edge must fit the turn: {step:?}"
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
                    "every preparation turn must honor work budgets: {step:?}"
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

    #[test]
    fn participant_network_preparation_recovers_committed_row_before_cursor_failure() {
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
        assert!(NetworkSyncStore::prepare_view_step(&*store, &view, single_row).is_err());
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
            2,
            "the selected row commits before the injected cursor-save failure"
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
        assert!(!node.shares_replica(&conversation).unwrap());
        let owner = node.sync_store(&peer.public());
        let store = owner.lock().unwrap();
        let view = open_prepared(&store, &conversation);
        assert_eq!(
            collect(&store, &view, &HashSet::new()),
            vec![genesis.id, share_off.id]
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
        assert!(!metadata.channel_v2 && !metadata.channel_v3 && !metadata.channel_v4);
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
}

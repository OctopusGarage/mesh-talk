//! Production participant network adapter. A finite authorization selection lives
//! in the encrypted profile DB; unchanged pages only check scalar revisions.
//! The initial authority reducer remains an explicit temporary history collector.
use super::*;
use crate::eventlog::{
    repository::{
        network::{ExportCursor, HaveCursor, NetworkView},
        selection::Selection,
    },
    sync::{
        FileConsentStatus,
        network::{
            ExportPage, ExportPageBudget, IdPageBudget, NetworkSyncStore, SyncMetadata, SyncPage,
        },
    },
};
use std::sync::{Mutex, atomic::Ordering};

#[derive(Clone, PartialEq, Eq)]
struct Policy {
    privacy_generation: u64,
    pending: [u8; 32],
    outbound: ([bool; 3], u64),
}
#[derive(Clone, PartialEq, Eq)]
struct Observation {
    source: i64,
    roster: u64,
    proofs: Option<u64>,
    file_scopes: u64,
    delivery_controls: u64,
    established: Option<[u8; 32]>,
}
pub(in crate::node) struct ParticipantView {
    conversation: ConversationId,
    repository: NetworkView,
    selection: Selection,
    policy: Policy,
    observed: Mutex<Observation>,
    visible: [u8; 32],
    metadata: SyncMetadata,
    // Staged invitations already have an 8 MiB owner budget.
    pending: Vec<EventId>,
    export_have: Mutex<Option<[u8; 32]>>,
    sibling_closure: bool,
    own_closure: bool,
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
            outbound: (
                [
                    self.node.shares_replica(conversation)?,
                    self.node
                        .restricts_group_file_replica(conversation, &self.peer)?,
                    self.node.certified_sibling(&self.peer),
                ],
                self.node.device_sync_policy_snapshot().1,
            ),
        })
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
        let roster = *self
            .node
            .roster
            .lock()
            .expect("roster lock")
            .subscribe_changes()
            .borrow();
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
        let file_scopes = self.node.privacy.projection_revision.load(Ordering::SeqCst);
        Ok(Observation {
            source,
            roster,
            proofs,
            delivery_controls,
            file_scopes,
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
        let events = self.visible_projection(conversation, false)?;
        let pending = self.pending_ids(conversation);
        let visible = crate::eventlog::sync::fingerprint(events.iter().map(|e| e.id).collect());
        let mut metadata = SyncMetadata {
            count: (events.len() + pending.len()) as u64,
            ..Default::default()
        };
        for event in events.iter().chain(
            self.pending_channels
                .get(conversation)
                .into_iter()
                .flat_map(|p| p.events.iter()),
        ) {
            metadata.channel_v2 |= event.kind == EventKind::ChannelReplica;
            metadata.channel_v3 |= matches!(
                event.kind,
                EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
            );
            metadata.channel_v4 |= matches!(
                event.kind,
                EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
            );
        }
        metadata.fingerprint = Some(crate::eventlog::sync::fingerprint(
            events
                .iter()
                .map(|e| e.id)
                .chain(pending.iter().copied())
                .collect(),
        ));
        let own = self.node.identity.public().ed25519_pub;
        let device_policy = self.node.device_sync_policy();
        let selection = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .create_selection(
                &repository,
                &events,
                super::super::session_pool::MAX_TCP_CONNECTIONS,
                |event| {
                    let own = event.author.ed25519_pub() == &own;
                    let selected = match event.kind {
                        EventKind::Message => device_policy.history,
                        EventKind::FileManifest => device_policy.attachments,
                        _ => true,
                    };
                    (
                        !self.pull_only && (!policy.outbound.0[1] || own),
                        selected,
                        own,
                    )
                },
            )?;
        if policy != self.network_policy(conversation)?
            || observed != self.network_observation(conversation)?
        {
            return Err(LogError::StaleCursor);
        }
        let sibling_closure = policy.outbound.0[2]
            && *conversation
                == super::super::conversation::dm_conversation_id(
                    &self.node.identity.public(),
                    &self.peer,
                );
        let own_closure = !self.inbound && !policy.outbound.0[0];
        Ok(ParticipantView {
            conversation: *conversation,
            repository,
            selection,
            policy,
            observed: Mutex::new(observed),
            visible,
            metadata,
            pending,
            export_have: Mutex::new(None),
            sibling_closure,
            own_closure,
        })
    }
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError> {
        NetworkSyncStore::validate_view(
            self.node.log.lock().expect("log lock").repository()?,
            &view.repository,
        )?;
        if self.network_policy(&view.conversation)? != view.policy {
            return Err(LogError::StaleCursor);
        }
        let current = self.network_observation(&view.conversation)?;
        let mut observed = view.observed.lock().expect("view observation lock");
        if *observed != current {
            let events = self.visible_projection(&view.conversation, false)?;
            // Raw repository capability flags may include rejected controls.
            // Bind negotiated support to this principal's fresh authorization too.
            let v4 = view.metadata.channel_v4;
            let v3 = v4 || view.metadata.channel_v3;
            let v2 = v3 || view.metadata.channel_v2;
            if events.iter().any(|event| match event.kind {
                EventKind::ChannelReplica => !v2,
                EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory => !v3,
                EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance => !v4,
                _ => false,
            }) {
                return Err(LogError::StaleCursor);
            }
            let ids = self
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .finite_ids(&view.repository, &events)?;
            if crate::eventlog::sync::fingerprint(ids) != view.visible
                || self.network_policy(&view.conversation)? != view.policy
                || self.network_observation(&view.conversation)? != current
            {
                return Err(LogError::StaleCursor);
            }
            *observed = current;
        }
        Ok(())
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError> {
        self.validate_view(view)?;
        Ok(view.metadata)
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
        let page = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .selected_have_page(&view.selection, &view.repository, retained_after, budget)?;
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
        let mut prepared = view.export_have.lock().expect("view export lock");
        if *prepared != Some(fingerprint) {
            // Mark dirty before work: a failed rebuild can never reuse partial flags.
            *prepared = None;
            self.node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .prepare_selected_export(
                    &view.selection,
                    &view.repository,
                    have,
                    view.sibling_closure,
                    view.own_closure,
                )?;
            self.validate_view(view)?;
            *prepared = Some(fingerprint);
        }
        let page = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .selected_export_page(
                &view.selection,
                &view.repository,
                have,
                after.map(|a| &a.retained),
                budget,
            )?;
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
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

    fn collect(
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        have: &HashSet<EventId>,
    ) -> Vec<EventId> {
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let view = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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
        let empty = NetworkSyncStore::open_view(&*store, &conversation).unwrap();
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

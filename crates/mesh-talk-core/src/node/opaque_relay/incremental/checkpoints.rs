//! Optional signed transport hints on the existing encrypted profile.
use super::*;
use bincode::Options;

fn hint_key(mailbox: ConversationId, inner: EventId) -> Vec<u8> {
    let mut key = b"wrapper-v1".to_vec();
    key.extend_from_slice(mailbox.as_bytes());
    key.extend_from_slice(inner.as_bytes());
    key
}
fn bounded<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(16384)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}

// Preserve the deployed signed receive cursor when upgrading its hint envelope.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct PreviousReceipt {
    cursor: Option<MailboxCursor>,
    pending: Vec<EventId>,
    epoch: i64,
    source_floor: i64,
    runtime: [u8; 16],
    session_only: bool,
}

// The deployed pre-target encoding has an outgoing completion proof without pairs.
// Its receive cursor survives upgrade, but the older proof is not promoted.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct PreviousCompletedUpload {
    local: HistorySyncStamp,
    remote: RemoteBinding,
    upper: i64,
    revision: i64,
}
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct PreviousCompletedReceipt {
    cursor: Option<MailboxCursor>,
    pending: Vec<EventId>,
    epoch: i64,
    source_floor: i64,
    runtime: [u8; 16],
    session_only: bool,
    completed: Option<PreviousCompletedUpload>,
}

fn receipt_body(bytes: &[u8]) -> Option<Receipt> {
    bounded::<Receipt>(bytes)
        .filter(|receipt| {
            receipt
                .completed
                .as_ref()
                .is_none_or(|proof| proof.targets.len() <= MAX_RESULTS)
        })
        .or_else(|| {
            let old: PreviousCompletedReceipt = bounded(bytes)?;
            // Read the obsolete proof only as its exact old encoding. It has
            // no selected-membership authority in the upgraded cache.
            if let Some(proof) = old.completed {
                let _ = (proof.local, proof.remote, proof.upper, proof.revision);
            }
            Some(Receipt {
                cursor: old.cursor,
                pending: old.pending,
                epoch: old.epoch,
                source_floor: old.source_floor,
                runtime: old.runtime,
                session_only: old.session_only,
                completed: None,
            })
        })
        .or_else(|| {
            let old: PreviousReceipt = bounded(bytes)?;
            Some(Receipt {
                cursor: old.cursor,
                pending: old.pending,
                epoch: old.epoch,
                source_floor: old.source_floor,
                runtime: old.runtime,
                session_only: old.session_only,
                completed: None,
            })
        })
}

impl Node {
    pub(super) fn seal_mailbox_hint(&self, key: &[u8], body: Vec<u8>) -> Option<Vec<u8>> {
        let mut message = b"MeshTalkMailboxHintV1\0".to_vec();
        message.extend_from_slice(key);
        message.extend_from_slice(&body);
        let signature = self.identity.sign(&message).to_vec();
        bincode::serialize(&Sealed { body, signature }).ok()
    }
    pub(super) fn open_mailbox_hint(&self, key: &[u8], bytes: &[u8]) -> Option<Vec<u8>> {
        let sealed: Sealed = bounded(bytes)?;
        let signature: [u8; 64] = sealed.signature.try_into().ok()?;
        let mut message = b"MeshTalkMailboxHintV1\0".to_vec();
        message.extend_from_slice(key);
        message.extend_from_slice(&sealed.body);
        crate::identity::device::DeviceIdentity::verify(
            &self.identity.public().ed25519_pub,
            &message,
            &signature,
        )
        .then_some(sealed.body)
    }
    #[cfg(test)]
    pub(in crate::node::opaque_relay) fn clear_mailbox_targets_for_test(&self, key: &[u8]) {
        let sealed = self
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(key)
            .unwrap()
            .unwrap();
        let body = self.open_mailbox_hint(key, &sealed).unwrap();
        let mut receipt = receipt_body(&body).unwrap();
        receipt.completed.as_mut().unwrap().targets.clear();
        let sealed = self
            .seal_mailbox_hint(key, bincode::serialize(&receipt).unwrap())
            .unwrap();
        assert!(
            self.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(key, &sealed)
                .unwrap()
        );
    }

    pub(super) fn mailbox_local_binding(&self) -> Result<(i64, i64), LogError> {
        let log = self.log.lock().expect("log lock");
        let repo = log.repository()?;
        Ok((repo.destructive_policy_revision()?, repo.source_revision()?))
    }
    pub(super) fn load_mailbox_receipt(
        &self,
        key: &[u8],
        conv: ConversationId,
    ) -> Result<Receipt, LogError> {
        let (epoch, source) = self.mailbox_local_binding()?;
        let cache = self.opaque_receive_cache.lock().expect("opaque cache");
        let runtime = cache.session_epoch();
        if let Ok(Some(bytes)) = cache.get_checkpoint(key) {
            if let Some(body) = self.open_mailbox_hint(key, &bytes)
                && let Some(receipt) = receipt_body(&body)
            {
                let valid = receipt.epoch == epoch
                    && receipt.source_floor <= source
                    && receipt.source_floor >= 0
                    && receipt.pending.len() <= MAX_PENDING
                    && receipt.completed.as_ref().is_none_or(|completed| {
                        completed.remote.conversation == conv
                            && completed.remote.epoch >= 0
                            && completed.upper >= 0
                            && completed.revision >= 0
                    })
                    && (!receipt.session_only || receipt.runtime == runtime)
                    && receipt.cursor.as_ref().is_none_or(|c| {
                        c.conversation == conv && c.after >= 0 && c.epoch >= 0 && c.revision >= 0
                    });
                if valid {
                    return Ok(receipt);
                }
            }
            let _ = cache.remove_checkpoint(key);
        }
        Ok(Receipt {
            cursor: None,
            pending: Vec::new(),
            epoch,
            source_floor: source,
            runtime,
            session_only: false,
            completed: None,
        })
    }
    pub(super) fn save_mailbox_receipt(
        &self,
        p: &mut OpaqueMailboxProgress,
    ) -> Result<(), session::SessionError> {
        if state(p)
            .receipt
            .completed
            .as_ref()
            .is_some_and(|proof| proof.targets.len() > MAX_RESULTS)
        {
            return Err(session::SessionError::ResourceUnavailable);
        }
        if state(p).saved.as_ref() == Some(&state(p).receipt) {
            return Ok(());
        }
        let (epoch, source) = self.mailbox_local_binding()?;
        if state(p).receipt.epoch != epoch {
            return Err(LogError::StaleCursor.into());
        }
        self.log.lock().expect("log lock").sync()?;
        state_mut(p).receipt.source_floor = source;
        let body = bincode::serialize(&state(p).receipt)
            .map_err(|e| session::SessionError::Serialization(e.to_string()))?;
        if let Some(sealed) = self.seal_mailbox_hint(&state(p).key, body) {
            let cache = self.opaque_receive_cache.lock().expect("opaque cache");
            if let Err(error) = cache.put_checkpoint(&state(p).key, &sealed) {
                log::debug!("mailbox checkpoint unavailable: {error}");
            }
        }
        state_mut(p).saved = Some(state(p).receipt.clone());
        Ok(())
    }
    pub(super) fn cached_wrapper_id(
        &self,
        mailbox: ConversationId,
        inner: EventId,
    ) -> Option<EventId> {
        let key = hint_key(mailbox, inner);
        let bytes = self
            .opaque_receive_cache
            .lock()
            .expect("opaque cache")
            .get_wrapper_hint(&key)
            .ok()??;
        let body = self.open_mailbox_hint(&key, &bytes)?;
        let id: [u8; 32] = body.try_into().ok()?;
        Some(EventId::new(id))
    }
    pub(in crate::node::opaque_relay) fn remember_wrapper_id(
        &self,
        mailbox: ConversationId,
        inner: EventId,
        outer: EventId,
    ) {
        let key = hint_key(mailbox, inner);
        if let Some(value) = self.seal_mailbox_hint(&key, outer.as_bytes().to_vec())
            && let Err(error) = self
                .opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .put_wrapper_hint(&key, &value)
        {
            log::debug!("mailbox wrapper hint unavailable: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use crate::postoffice::IndexedPostOffice;
    use crate::storage::profile::ProfileStorageLease;

    #[test]
    fn previous_receipt_preserves_receive_progress_without_upload_completion() {
        let cursor = MailboxCursor {
            store_uuid: [3; 16],
            conversation: ConversationId::new([5; 32]),
            epoch: 4,
            first_seen: Some(10),
            revision: 8,
            after: 17,
            window: Some((20, 9)),
        };
        let pending = vec![EventId::new([8; 32])];
        let old = PreviousReceipt {
            cursor: Some(cursor.clone()),
            pending: pending.clone(),
            epoch: 2,
            source_floor: 7,
            runtime: [1; 16],
            session_only: true,
        };
        let decoded = receipt_body(&bincode::serialize(&old).unwrap()).unwrap();
        assert_eq!(decoded.cursor, Some(cursor));
        assert_eq!(decoded.pending, pending);
        assert_eq!(decoded.epoch, 2);
        assert_eq!(decoded.source_floor, 7);
        assert_eq!(decoded.runtime, [1; 16]);
        assert!(decoded.session_only);
        assert!(decoded.completed.is_none());
        let upgraded = receipt_body(&bincode::serialize(&decoded).unwrap()).unwrap();
        assert!(upgraded == decoded);
    }

    fn fixture_certificate() -> CompletedUpload {
        CompletedUpload {
            local: bincode::deserialize(&[7_u8; 32]).unwrap(),
            remote: RemoteBinding {
                store_uuid: [3; 16],
                conversation: ConversationId::new([5; 32]),
                epoch: 4,
                first_seen: Some(10),
            },
            upper: 20,
            revision: 9,
            targets: Vec::new(),
        }
    }

    #[test]
    fn previous_completed_receipt_keeps_receive_progress_and_drops_old_proof() {
        for complete in [false, true] {
            let proof = fixture_certificate();
            let old = PreviousCompletedReceipt {
                cursor: Some(MailboxCursor {
                    store_uuid: [3; 16],
                    conversation: ConversationId::new([5; 32]),
                    epoch: 4,
                    first_seen: Some(10),
                    revision: 8,
                    after: 17,
                    window: Some((20, 9)),
                }),
                pending: vec![EventId::new([8; 32])],
                epoch: 2,
                source_floor: 7,
                runtime: [1; 16],
                session_only: true,
                completed: complete.then_some(PreviousCompletedUpload {
                    local: proof.local,
                    remote: proof.remote,
                    upper: proof.upper,
                    revision: proof.revision,
                }),
            };
            let decoded = receipt_body(&bincode::serialize(&old).unwrap()).unwrap();
            assert_eq!(decoded.cursor, old.cursor);
            assert_eq!(decoded.pending, old.pending);
            assert_eq!(decoded.epoch, old.epoch);
            assert_eq!(decoded.source_floor, old.source_floor);
            assert_eq!(decoded.runtime, old.runtime);
            assert_eq!(decoded.session_only, old.session_only);
            assert!(decoded.completed.is_none());
        }
    }

    #[test]
    fn qualified_targets_fifo_is_bounded_and_receipt_fits_existing_envelope() {
        let id = |number: u64| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&number.to_be_bytes());
            EventId::new(bytes)
        };
        let mut proof = fixture_certificate();
        for number in 0..65 {
            proof.remember_target(id(number), id(number + 100));
        }
        assert_eq!(proof.targets.len(), MAX_RESULTS);
        assert_eq!(proof.targets.first(), Some(&(id(1), id(101))));
        let unchanged = proof.targets.clone();
        proof.remember_target(id(1), id(101));
        assert_eq!(
            proof.targets, unchanged,
            "hits cannot refresh FIFO position"
        );
        proof.remember_target(id(65), id(165));
        assert_eq!(proof.targets.first(), Some(&(id(2), id(102))));
        let mut receipt = Receipt {
            cursor: None,
            pending: (0..MAX_PENDING as u64).map(id).collect(),
            epoch: 0,
            source_floor: 0,
            runtime: [0; 16],
            session_only: false,
            completed: Some(proof),
        };
        let body = bincode::serialize(&receipt).unwrap();
        assert!(
            bincode::serialized_size(&Sealed {
                body: body.clone(),
                signature: vec![0; 64]
            })
            .unwrap()
                <= 16384
        );
        assert!(receipt_body(&body).is_some());
        receipt
            .completed
            .as_mut()
            .unwrap()
            .targets
            .push((id(66), id(166)));
        assert!(
            receipt_body(&bincode::serialize(&receipt).unwrap()).is_none(),
            "oversized new proof must not decode as a legacy receipt"
        );
    }

    async fn connection(
        node: &Node,
        office: IndexedPostOffice,
        relay: &DeviceIdentity,
    ) -> (
        crate::transport::SecureChannel<tokio::io::DuplexStream>,
        tokio::task::JoinHandle<usize>,
    ) {
        let keys = relay.secret_bytes();
        let (client, server) = tokio::io::duplex(256 * 1024);
        let task = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
            let mut channel = crate::transport::SecureChannel::accept(server, &identity)
                .await
                .unwrap();
            let store = Mutex::new(office);
            let budget = session::RemoteHaveBudget::default();
            let mut authorized = None;
            let mut custody = 0;
            while let Ok(bytes) = channel.recv().await {
                custody += usize::from(session::durable_have_query_for_test(&bytes).is_some());
                mailbox::serve(&mut channel, &store, &bytes, &budget, &mut authorized)
                    .await
                    .unwrap();
            }
            custody
        });
        let channel =
            crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
                .await
                .unwrap();
        (channel, task)
    }

    async fn completed_round(
        node: &Node,
        office: IndexedPostOffice,
        relay: &DeviceIdentity,
        remote: &DeviceIdentity,
        conversation: ConversationId,
    ) -> (Receipt, Vec<u8>, usize) {
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(),
            &remote.public(),
            Some(conversation),
            mailbox,
        );
        let (mut channel, task) = connection(node, office, relay).await;
        while !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
        {}
        let receipt = state(&progress).receipt.clone();
        let key = state(&progress).key.clone();
        drop(progress);
        drop(channel);
        (receipt, key, task.await.unwrap())
    }

    #[tokio::test]
    async fn completion_does_not_promote_new_append_or_cancelled_final_probe() {
        use crate::eventlog::sync::network::NetworkSyncStore;
        for cancel in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let relay_root = tempfile::tempdir().unwrap();
            let remote = DeviceIdentity::generate();
            let proof = crate::discovery::Announce::new_with_account(
                &remote,
                &Account::generate(),
                "Remote",
                47475,
            );
            let (node, _, _) = crate::node::test_support::node_with_files(
                root.path(),
                DeviceIdentity::generate(),
                Account::generate(),
                &proof,
            );
            let conversation = crate::node::conversation::dm_conversation_id(
                &node.identity.public(),
                &remote.public(),
            );
            let mut previous = None;
            for seq in 1..=32 {
                let event = Event::new(
                    &node.identity,
                    conversation,
                    seq,
                    previous.into_iter().collect(),
                    seq,
                    0,
                    EventKind::Message,
                    vec![7; 128],
                );
                previous = Some(event.id);
                node.log.lock().unwrap().append_durable(event).unwrap();
            }
            while !node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .prepare_kind_metadata_step(
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )
                .unwrap()
                .ready
            {}
            let relay = DeviceIdentity::generate();
            let office = IndexedPostOffice::open_with_lease(
                ProfileStorageLease::acquire(relay_root.path()).unwrap(),
                &relay_root.path().join("relay.log"),
                "pw",
                DeviceIdentity::generate(),
            )
            .unwrap();
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(
                &relay.public(),
                &remote.public(),
                Some(conversation),
                mailbox,
            );
            let (mut channel, task) = connection(&node, office.clone(), &relay).await;
            for _ in 0..2048 {
                assert!(
                    !node
                        .advance_opaque_mailbox(&mut channel, &mut progress)
                        .await
                        .unwrap()
                );
                if state(&progress).final_probe {
                    break;
                }
            }
            assert!(state(&progress).final_probe);
            assert!(
                state(&progress).receipt.completed.is_none(),
                "upload acknowledgments alone must not publish completion before the final probe"
            );
            let tail = Event::new(
                &node.identity,
                conversation,
                33,
                previous.into_iter().collect(),
                33,
                0,
                EventKind::Message,
                vec![8; 128],
            );
            node.log.lock().unwrap().append_durable(tail).unwrap();
            if !cancel {
                while !node
                    .advance_opaque_mailbox(&mut channel, &mut progress)
                    .await
                    .unwrap()
                {}
                assert!(
                    state(&progress).receipt.completed.is_none(),
                    "a finite old prefix cannot be qualified with a newly appended source stamp"
                );
            }
            drop(progress);
            drop(channel);
            task.await.unwrap();
            let mut converged = false;
            // Cancellation before the creation probe can legitimately reset
            // transport rank and finish the retained old finite window first.
            // A later operation resumes its append tail; no partial prefix may
            // qualify the new source as complete in the meantime.
            for _ in 0..3 {
                let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
                let mut progress = OpaqueMailboxProgress::new(
                    &relay.public(),
                    &remote.public(),
                    Some(conversation),
                    mailbox,
                );
                let (mut channel, task) = connection(&node, office.clone(), &relay).await;
                while !node
                    .advance_opaque_mailbox(&mut channel, &mut progress)
                    .await
                    .unwrap()
                {}
                let view = office.open_view(&progress.mailbox.id()).unwrap();
                let count = office.metadata(&view).unwrap().count;
                let completed = state(&progress).receipt.completed.is_some();
                if count < 33 {
                    assert!(
                        !completed,
                        "an old finite prefix cannot qualify the missing new tail"
                    );
                }
                converged = count == 33 && completed;
                drop(progress);
                drop(channel);
                task.await.unwrap();
                if converged {
                    break;
                }
            }
            assert!(
                converged,
                "bounded fresh operations must upload and qualify the appended tail"
            );

            for no_prior in [false, true] {
                if no_prior {
                    node.set_post_office_permissions(true, false);
                }
                let (mut receipt, key, _) =
                    completed_round(&node, office.clone(), &relay, &remote, conversation).await;
                assert!(receipt.completed.is_some());
                if no_prior {
                    // Same incarnation, but the observed source has rolled back
                    // below the outgoing proof even without a receive cursor.
                    receipt.cursor = None;
                    let completed = receipt.completed.as_mut().unwrap();
                    completed.upper += 1_000_000;
                    completed.revision += 1_000_000;
                } else {
                    // A future receive cursor is rejected with reset=true even
                    // though UUID, destructive epoch and first_seen all match.
                    let cursor = receipt.cursor.as_mut().unwrap();
                    cursor.after += 1_000_000;
                    cursor.revision += 1_000_000;
                    cursor.window = None;
                }
                let signed = node
                    .seal_mailbox_hint(&key, bincode::serialize(&receipt).unwrap())
                    .unwrap();
                node.opaque_receive_cache
                    .lock()
                    .unwrap()
                    .put_checkpoint(&key, &signed)
                    .unwrap();
                node.opaque_upload_cache.lock().unwrap().entries.clear();
                let (restored, _, custody) =
                    completed_round(&node, office.clone(), &relay, &remote, conversation).await;
                assert!(
                    custody >= 33,
                    "same-binding cursor or outgoing-floor rollback must requalify custody"
                );
                assert!(restored.completed.is_some());
                let (_, _, custody) =
                    completed_round(&node, office.clone(), &relay, &remote, conversation).await;
                assert_eq!(
                    custody, 0,
                    "the repaired proof must return to bounded quiet probes"
                );
                if no_prior {
                    let proof = restored.completed;
                    let mailbox =
                        RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
                    let unrelated = Event::new(
                        &node.identity,
                        ConversationId::new([211; 32]),
                        1,
                        vec![],
                        1,
                        0,
                        EventKind::Message,
                        vec![9; 128],
                    );
                    office
                        .clone()
                        .ingest(mailbox.wrap(&unrelated).unwrap())
                        .unwrap();
                    let (grown, _, custody) =
                        completed_round(&node, office.clone(), &relay, &remote, conversation).await;
                    assert_eq!(
                        custody, 0,
                        "ordinary remote growth must preserve outgoing qualification"
                    );
                    assert!(
                        grown.completed == proof,
                        "quiet remote growth must not rewrite a sufficient completed-upload proof"
                    );
                }
            }

            // Force the compact-only path, then retire privacy authority while
            // its exact target selection yields between bounded turns.
            node.opaque_upload_cache.lock().unwrap().entries.clear();
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(
                &relay.public(),
                &remote.public(),
                Some(conversation),
                mailbox,
            );
            let (mut channel, task) = connection(&node, office.clone(), &relay).await;
            while !node
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
            {}
            assert!(state(&progress).lease.0.lock().unwrap().view.is_none());
            let proof = state(&progress).receipt.completed.clone().unwrap();
            let (target, outer) = proof.targets[0];
            HISTORY_METADATA_ROWS.with(|rows| rows.set(0));
            let writes = node
                .storage
                .database()
                .read(|c| Ok(c.total_changes()))
                .unwrap();
            assert_eq!(
                node.resolve_opaque_confirmation(&mut progress, target, &|| true)
                    .await
                    .unwrap(),
                Some(outer)
            );
            assert!(state(&progress).lease.0.lock().unwrap().view.is_none());
            assert_eq!(HISTORY_METADATA_ROWS.with(|rows| rows.get()), 0);
            assert_eq!(
                node.storage
                    .database()
                    .read(|c| Ok(c.total_changes()))
                    .unwrap(),
                writes,
                "a qualified hit must perform no SQL writes"
            );
            let floor = state(&progress).remote_floor;
            state_mut(&mut progress).remote_floor = Some((0, 0));
            let mut invalid =
                Box::pin(node.resolve_opaque_confirmation(&mut progress, target, &|| true));
            assert!(
                futures_util::poll!(invalid.as_mut()).is_pending(),
                "remote rollback must force selected proof preparation"
            );
            drop(invalid);
            state_mut(&mut progress).remote_floor = floor;
            state(&progress).lease.0.lock().unwrap().view = None;
            node.log
                .lock()
                .unwrap()
                .append_durable(Event::new(
                    &node.identity,
                    conversation,
                    34,
                    vec![],
                    34,
                    0,
                    EventKind::Message,
                    vec![9; 128],
                ))
                .unwrap();
            let mut invalid =
                Box::pin(node.resolve_opaque_confirmation(&mut progress, target, &|| true));
            assert!(
                futures_util::poll!(invalid.as_mut()).is_pending(),
                "a changed local history stamp must not reuse target membership"
            );
            drop(invalid);
            assert!(state(&progress).receipt.completed.as_ref() == Some(&proof));
            state(&progress).lease.0.lock().unwrap().view = None;
            let mut prepare = Box::pin(node.resolve_opaque_confirmation(
                &mut progress,
                EventId::new([255; 32]),
                &|| true,
            ));
            assert!(futures_util::poll!(prepare.as_mut()).is_pending());
            node.privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            assert!(
                matches!(prepare.await, Err(session::SessionError::UnexpectedMessage)),
                "privacy retirement must reject target preparation resumed after a yield"
            );
            drop(progress);
            drop(channel);
            task.await.unwrap();
        }
    }
}

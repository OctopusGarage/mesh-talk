//! Optional signed transport hints on the existing encrypted profile.
use super::*;
use bincode::Options;

#[cfg(test)]
thread_local! {
    static HINT_OPEN_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn hint_key(mailbox: ConversationId, inner: EventId) -> Vec<u8> {
    let mut key = b"wrapper-v1".to_vec();
    key.extend_from_slice(mailbox.as_bytes());
    key.extend_from_slice(inner.as_bytes());
    key
}
fn semantic_key(account: &[u8; 32], mailbox: ConversationId, outer: EventId) -> Vec<u8> {
    let mut key = b"semantic-profile-v1".to_vec();
    key.extend_from_slice(account);
    key.extend_from_slice(mailbox.as_bytes());
    key.extend_from_slice(outer.as_bytes());
    key
}
fn consent_hint_key(account: &[u8; 32], mailbox: ConversationId, outer: EventId) -> Vec<u8> {
    let mut key = b"consent-v1".to_vec();
    key.extend_from_slice(account);
    key.extend_from_slice(mailbox.as_bytes());
    key.extend_from_slice(outer.as_bytes());
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

// Exact immediately preceding envelope, including qualified outgoing targets.
#[derive(Deserialize)]
#[cfg_attr(test, derive(Serialize))]
struct PreviousTargetReceipt {
    cursor: Option<MailboxCursor>,
    pending: Vec<EventId>,
    epoch: i64,
    source_floor: i64,
    runtime: [u8; 16],
    session_only: bool,
    completed: Option<CompletedUpload>,
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
            let old: PreviousTargetReceipt = bounded(bytes)?;
            if old
                .completed
                .as_ref()
                .is_some_and(|proof| proof.targets.len() > MAX_RESULTS)
            {
                return None;
            }
            Some(Receipt {
                cursor: old.cursor,
                pending: old.pending,
                epoch: old.epoch,
                source_floor: old.source_floor,
                runtime: old.runtime,
                session_only: old.session_only,
                completed: old.completed,
                repair: None,
            })
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
                repair: None,
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
                repair: None,
            })
        })
}

fn receipt_valid(
    receipt: &Receipt,
    conv: ConversationId,
    epoch: i64,
    source: i64,
    runtime: [u8; 16],
) -> bool {
    let pages_valid = receipt.repair.as_ref().is_none_or(|repair| {
        let upper = repair
            .captured
            .window
            .unwrap_or((repair.captured.after, repair.captured.revision));
        let cursor_valid = |cursor: &MailboxCursor| {
            RemoteBinding::from(cursor) == RemoteBinding::from(&repair.captured)
                && cursor.after >= 0
                && cursor.after <= upper.0
                && cursor.window.is_none_or(|window| window == upper)
        };
        repair.local_epoch >= 0
            && repair.global_epoch.is_none_or(|epoch| epoch >= 0)
            && (!repair.active || receipt.pending.contains(&repair.owner))
            && cursor_valid(&repair.scan)
            && repair.page.as_ref().is_none_or(|page| {
                page.ids.len() <= 64
                    && page.next <= page.ids.len()
                    && cursor_valid(&page.cursor)
                    && page.complete == page.cursor.window.is_none()
            })
    });
    pages_valid
        && (receipt.repair.as_ref().is_none_or(|repair| {
            let c = &repair.captured;
            c.conversation == conv
                && c.after >= 0
                && c.epoch >= 0
                && c.revision >= 0
                && c.window
                    .is_none_or(|(upper, revision)| upper >= c.after && revision >= 0)
        }) && receipt.epoch == epoch
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
            }))
}

impl Node {
    pub(in crate::node::opaque_relay) fn consent_transport_seen(
        &self,
        mailbox: ConversationId,
        outer: EventId,
    ) -> Result<bool, LogError> {
        let key = consent_hint_key(&self.account.public().ed25519_pub, mailbox, outer);
        let bytes = self
            .opaque_receive_cache
            .lock()
            .expect("opaque cache")
            .get_wrapper_hint(&key)
            .ok()
            .flatten();
        let Some(body) = bytes
            .as_deref()
            .and_then(|bytes| self.open_mailbox_hint(&key, bytes))
        else {
            return Ok(false);
        };
        let Some((manifest, inner, own, epoch)) = bounded::<([u8; 32], [u8; 32], bool, i64)>(&body)
        else {
            return Ok(false);
        };
        let current_epoch = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .receive_repair_revision()?;
        if current_epoch != epoch {
            return Ok(false);
        }
        // Even the unconditional own-recipient exclusion must cross the current
        // account/generation fence. A missing current fact is safe only for it.
        let current = self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_control(EventId::new(manifest))?;
        Ok(own || current.is_some_and(|event| event.id == EventId::new(inner)))
    }

    pub(in crate::node::opaque_relay) fn remember_consent_transport(
        &self,
        mailbox: ConversationId,
        outer: EventId,
        event: &Event,
    ) -> Result<(), LogError> {
        let Some(payload) = crate::node::file_consent::Decision::open(event) else {
            return Ok(());
        };
        // Decision::open proves sender != recipient. Full recipient equality
        // therefore proves this node can never apply its own echoed decision.
        let own = payload.recipient == self.identity.public();
        let current = self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_control(payload.manifest)?;
        if !own && current.is_none_or(|current| current.id != event.id) {
            return Ok(());
        }
        let epoch = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .receive_repair_revision()?;
        let key = consent_hint_key(&self.account.public().ed25519_pub, mailbox, outer);
        let body = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&(
                *payload.manifest.as_bytes(),
                *event.id.as_bytes(),
                own,
                epoch,
            ))
            .map_err(|error| LogError::Serialization(error.to_string()))?;
        if let Some(value) = self.seal_mailbox_hint(&key, body)
            && let Err(error) = self
                .opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .put_wrapper_hint(&key, &value)
        {
            log::debug!("consent transport hint unavailable: {error}");
        }
        Ok(())
    }

    /// A signed classification hint is transport evidence only. Its exact
    /// presentation fact and account owner remain authoritative at every hit.
    pub(in crate::node::opaque_relay) fn semantic_profile_seen(
        &self,
        mailbox: ConversationId,
        outer: EventId,
    ) -> Result<bool, LogError> {
        let key = semantic_key(&self.account.public().ed25519_pub, mailbox, outer);
        let bytes = {
            self.opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .get_wrapper_hint(&key)
                .ok()
                .flatten()
        };
        let Some(body) = bytes
            .as_deref()
            .and_then(|bytes| self.open_mailbox_hint(&key, bytes))
        else {
            return Ok(false);
        };
        let Some((inner, own, epoch)) = bounded::<([u8; 32], bool, i64)>(&body) else {
            return Ok(false);
        };
        let current = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .receive_repair_revision()?;
        if epoch != current {
            return Ok(false);
        }
        // Run the account/lifetime check even for own-author classifications,
        // which deliberately have no presentation processed fact.
        let processed = self
            .profile_receive_work
            .lock()
            .expect("profile receive work")
            .processed(EventId::new(inner))?;
        Ok(own || processed)
    }

    pub(in crate::node::opaque_relay) fn remember_semantic_profile(
        &self,
        mailbox: ConversationId,
        outer: EventId,
        inner: EventId,
        own: bool,
    ) -> Result<(), LogError> {
        let epoch = self
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .receive_repair_revision()?;
        let key = semantic_key(&self.account.public().ed25519_pub, mailbox, outer);
        let body = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&(*inner.as_bytes(), own, epoch))
            .map_err(|error| LogError::Serialization(error.to_string()))?;
        if let Some(value) = self.seal_mailbox_hint(&key, body)
            && let Err(error) = self
                .opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .put_wrapper_hint(&key, &value)
        {
            log::debug!("semantic Profile hint unavailable: {error}");
        }
        Ok(())
    }

    pub(super) fn seal_mailbox_hint(&self, key: &[u8], body: Vec<u8>) -> Option<Vec<u8>> {
        let mut message = b"MeshTalkMailboxHintV1\0".to_vec();
        message.extend_from_slice(key);
        message.extend_from_slice(&body);
        let signature = self.identity.sign(&message).to_vec();
        bincode::serialize(&Sealed { body, signature }).ok()
    }
    pub(super) fn open_mailbox_hint(&self, key: &[u8], bytes: &[u8]) -> Option<Vec<u8>> {
        #[cfg(test)]
        HINT_OPEN_ATTEMPTS.with(|count| count.set(count.get() + 1));
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
        Ok((repo.receive_repair_revision()?, repo.source_revision()?))
    }
    #[cfg(test)]
    pub(super) fn load_mailbox_receipt(
        &self,
        key: &[u8],
        conv: ConversationId,
    ) -> Result<Receipt, LogError> {
        self.load_mailbox_receipt_with_runtime(key, conv, None)
    }
    pub(super) fn load_mailbox_receipt_with_runtime(
        &self,
        key: &[u8],
        conv: ConversationId,
        hot: Option<Receipt>,
    ) -> Result<Receipt, LogError> {
        let (epoch, source) = self.mailbox_local_binding()?;
        let cache = self.opaque_receive_cache.lock().expect("opaque cache");
        let runtime = cache.session_epoch();
        if let Some(receipt) = hot
            && receipt.runtime == runtime
            && receipt_valid(&receipt, conv, epoch, source, runtime)
        {
            self.observe_mailbox_work(reason::RECEIPT_HOT, None);
            return Ok(receipt);
        }
        if let Ok(Some(bytes)) = cache.get_checkpoint(key) {
            if let Some(body) = self.open_mailbox_hint(key, &bytes)
                && let Some(receipt) = receipt_body(&body)
            {
                let valid = receipt_valid(&receipt, conv, epoch, source, runtime);
                if valid {
                    let mut receipt = receipt;
                    // Spent work qualifies only the current hot owner's exact
                    // optional knowledge. Cold load retains active scan progress
                    // and ordinary receive/upload proofs, but grants fresh work.
                    if receipt.repair.as_ref().is_some_and(|repair| !repair.active) {
                        receipt.repair = None;
                    } else if let Some(repair) = receipt.repair.as_mut() {
                        repair.spent.fill(0);
                    }
                    // Validation above rejects old session-only authority before
                    // attaching reusable durable progress to this runtime.
                    receipt.runtime = runtime;
                    self.observe_mailbox_work(reason::RECEIPT_DURABLE, None);
                    return Ok(receipt);
                }
            }
            self.observe_mailbox_work(reason::RECEIPT_REJECTED, None);
            let _ = cache.remove_checkpoint(key);
        } else {
            self.observe_mailbox_work(reason::RECEIPT_MISSING, None);
        }
        Ok(Receipt {
            cursor: None,
            pending: Vec::new(),
            epoch,
            source_floor: source,
            runtime,
            session_only: false,
            completed: None,
            repair: None,
        })
    }
    pub(super) fn save_mailbox_receipt(
        &self,
        p: &mut OpaqueMailboxProgress,
    ) -> Result<(), session::SessionError> {
        self.validate_mailbox_repair(p)?;
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
        state_mut(p).receipt.runtime = self
            .opaque_receive_cache
            .lock()
            .expect("opaque cache")
            .session_epoch();
        let body = bincode::serialize(&state(p).receipt)
            .map_err(|e| session::SessionError::Serialization(e.to_string()))?;
        if let Some(sealed) = self.seal_mailbox_hint(&state(p).key, body)
            && sealed.len() <= 16384
        {
            // Same bounded owner as upload views: optional durable cache failure
            // cannot throw away this runtime's already-classified finite work.
            {
                let mut entry = state(p).lease.0.lock().expect("upload entry");
                if let Some(generation) = state(p).repair_generation
                    && (self.privacy.generation.load(Ordering::Acquire) != generation
                        || entry.repair_generation != generation
                        || entry.repair_lease.is_none())
                {
                    return Err(LogError::StaleCursor.into());
                }
                entry.receipt = Some(state(p).receipt.clone());
            }
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
    fn previous_target_receipt_preserves_receive_cursor_and_qualified_upload_proof() {
        let mut proof = fixture_certificate();
        proof
            .targets
            .push((EventId::new([8; 32]), EventId::new([9; 32])));
        let old = PreviousTargetReceipt {
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
            session_only: false,
            completed: Some(proof),
        };
        let decoded = receipt_body(&bincode::serialize(&old).unwrap()).unwrap();
        assert_eq!(decoded.cursor, old.cursor);
        assert_eq!(decoded.pending, old.pending);
        assert!(decoded.completed == old.completed);
        assert!(decoded.repair.is_none());
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
            repair: Some(Repair {
                captured: MailboxCursor {
                    store_uuid: [3; 16],
                    conversation: ConversationId::new([5; 32]),
                    epoch: 4,
                    first_seen: Some(10),
                    revision: 8,
                    after: 17,
                    window: Some((20, 9)),
                },
                witness: [7; 32],
                active: true,
                scan: MailboxCursor {
                    store_uuid: [3; 16],
                    conversation: ConversationId::new([5; 32]),
                    epoch: 4,
                    first_seen: Some(10),
                    revision: 8,
                    after: 0,
                    window: Some((20, 9)),
                },
                page: Some(RepairPage {
                    cursor: MailboxCursor {
                        store_uuid: [3; 16],
                        conversation: ConversationId::new([5; 32]),
                        epoch: 4,
                        first_seen: Some(10),
                        revision: 8,
                        after: 17,
                        window: Some((20, 9)),
                    },
                    ids: (0..64).map(id).collect(),
                    next: 3,
                    complete: false,
                }),
                rewind: true,
                focus: Some((id(90), id(91))),
                conversation: ConversationId::new([6; 32]),
                local_epoch: 2,
                global_epoch: Some(4),
                owner: id(90),
                spent: [255; 32],
                scan_complete: false,
            }),
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

    #[tokio::test]
    async fn repair_entry_lease_survives_round_drop_and_releases_on_policy_idle_and_eviction() {
        let root = tempfile::tempdir().unwrap();
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
        node.set_post_office_permissions(false, true);
        let install = |key: &[u8]| {
            let (lease, retired) = {
                node.opaque_upload_cache
                    .lock()
                    .unwrap()
                    .acquire(
                        key,
                        UploadScope::ReceiveOnly,
                        node.privacy.generation.load(Ordering::Acquire),
                        std::time::Instant::now(),
                    )
                    .unwrap()
            };
            drop(retired);
            let guard = node.acquire_profile_compaction_lease().unwrap();
            {
                let mut entry = lease.0.lock().unwrap();
                entry.repair_generation = node.privacy.generation.load(Ordering::Acquire);
                entry.repair_progress = std::time::Instant::now();
                entry.repair_lease = Some(guard);
            }
            lease
        };
        let count = || *node.profile_compaction_leases.lock().unwrap();
        drop(install(b"repair"));
        assert_eq!(
            count(),
            1,
            "watchdog dropping an operation must preserve the active job lease"
        );
        node.set_post_office_permissions(false, false);
        assert_eq!(count(), 0);
        node.set_post_office_permissions(false, true);
        drop(install(b"repair"));
        node.set_post_office_enabled(false);
        assert_eq!(count(), 0);
        node.set_post_office_enabled(true);
        drop(install(b"repair"));
        node.privacy.generation.fetch_add(1, Ordering::AcqRel);
        node.drain_profile_compaction().unwrap();
        assert_eq!(
            count(),
            0,
            "broader authorization replacement must release abandoned protection"
        );
        let lease = install(b"repair");
        lease.0.lock().unwrap().repair_progress =
            std::time::Instant::now() - std::time::Duration::from_secs(31);
        node.drain_profile_compaction().unwrap();
        assert_eq!(
            count(),
            0,
            "even a busy stalled job must stop pinning compaction after its progress deadline"
        );
        drop(lease);
        let lease = install(b"repair");
        lease.0.lock().unwrap().used =
            std::time::Instant::now() - std::time::Duration::from_secs(31);
        node.drain_profile_compaction().unwrap();
        assert_eq!(
            count(),
            1,
            "old LRU/request time cannot expire recent meaningful repair progress"
        );
        drop(lease);
        node.set_post_office_permissions(false, false);
        node.set_post_office_permissions(false, true);
        let lease = install(b"repair");
        lease.0.lock().unwrap().used =
            std::time::Instant::now() - std::time::Duration::from_secs(1);
        drop(lease);
        for number in 0..MAX_VIEWS {
            let acquired = {
                node.opaque_upload_cache
                    .lock()
                    .unwrap()
                    .acquire(
                        &[number as u8],
                        UploadScope::ReceiveOnly,
                        node.privacy.generation.load(Ordering::Acquire),
                        std::time::Instant::now(),
                    )
                    .unwrap()
            };
            drop(acquired);
        }
        assert_eq!(
            count(),
            0,
            "bounded entry eviction must release the evicted job lease"
        );
    }

    #[test]
    fn repair_runtime_id_budget_rejects_overflow_and_recovers_after_reclamation() {
        let mut cache = UploadCache::default();
        let (lease, retired) = cache
            .acquire(
                b"repair",
                UploadScope::ReceiveOnly,
                0,
                std::time::Instant::now(),
            )
            .unwrap();
        drop(retired);
        let mut entry = lease.0.lock().unwrap();
        let id = |n: usize| {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(n as u64).to_le_bytes());
            EventId::new(bytes)
        };
        for number in 0..MAX_REPAIR_HINT_BYTES / 64 {
            entry.remember_repair_hint(id(number), id(number)).unwrap();
        }
        let next = id(MAX_REPAIR_HINT_BYTES);
        assert!(matches!(
            entry.remember_repair_hint(next, next),
            Err(session::SessionError::ResourceUnavailable)
        ));
        assert!(matches!(
            entry.remember_repair_declaration(next),
            Err(session::SessionError::ResourceUnavailable)
        ));
        entry.remember_repair_hint(id(0), next).unwrap();
        entry.repair_hints.remove(&id(1));
        assert!(entry.remember_repair_declaration(next).unwrap());
        assert!(!entry.remember_repair_declaration(next).unwrap());
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
    async fn validated_cold_receipt_warms_existing_owner_without_writes() {
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
        let relay = DeviceIdentity::generate();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_root.path()).unwrap(),
            &relay_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let (receipt, key, _) =
            completed_round(&node, office.clone(), &relay, &remote, conversation).await;
        assert!(receipt.completed.is_some());
        node.opaque_upload_cache.lock().unwrap().clear();
        let checkpoint = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap()
            .unwrap();
        let writes = node.storage.database().successful_write_commits();
        let parent_syncs =
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|count| count.get());
        HINT_OPEN_ATTEMPTS.with(|count| count.set(0));
        for round in 0..3 {
            // A fresh adapter and Noise channel, but the same bounded runtime owner.
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(
                &relay.public(),
                &remote.public(),
                Some(conversation),
                mailbox,
            );
            let (mut channel, task) = connection(&node, office.clone(), &relay).await;
            assert_eq!(
                node.advance_incremental_mailbox(&mut channel, &mut progress, &|| true,)
                    .await
                    .unwrap(),
                Some(false)
            );
            assert!(state(&progress).receipt == receipt);
            // Equal durable state must stay a no-op even with a new adapter.
            node.save_mailbox_receipt(&mut progress).unwrap();
            drop(progress);
            drop(channel);
            assert_eq!(task.await.unwrap(), 0);
            assert_eq!(
                HINT_OPEN_ATTEMPTS.with(|count| count.get()),
                1,
                "round {round}: unchanged validated receipt must not reopen its signature"
            );
            assert_eq!(node.storage.database().successful_write_commits(), writes);
            assert_eq!(
                crate::storage::profile::publication::tests::PARENT_SYNCS.with(|count| count.get()),
                parent_syncs,
                "unchanged initialization and save must not sync a directory"
            );
            assert!(
                node.opaque_receive_cache
                    .lock()
                    .unwrap()
                    .get_checkpoint(&key)
                    .unwrap()
                    .as_ref()
                    == Some(&checkpoint)
            );
            assert_eq!(node.opaque_upload_cache.lock().unwrap().observed_len(), 1);
        }
    }

    #[tokio::test]
    async fn cold_repair_receipt_warming_preserves_live_owner_spent_progress() {
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
        let relay = DeviceIdentity::generate();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_root.path()).unwrap(),
            &relay_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let conversation = crate::node::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let probe = OpaqueMailboxProgress::new(
            &relay.public(),
            &remote.public(),
            Some(conversation),
            mailbox,
        );
        let key = cache_key(&probe).unwrap();
        let (epoch, source) = node.mailbox_local_binding().unwrap();
        let runtime = node.opaque_receive_cache.lock().unwrap().session_epoch();
        let global_epoch = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .destructive_policy_revision()
            .unwrap();
        let owner = EventId::new([8; 32]);
        let cursor = MailboxCursor {
            store_uuid: [3; 16],
            conversation: probe.mailbox.id(),
            epoch: 4,
            first_seen: Some(10),
            revision: 8,
            after: 0,
            window: Some((20, 9)),
        };
        let receipt = Receipt {
            cursor: Some(cursor.clone()),
            pending: vec![owner],
            epoch,
            source_floor: source,
            runtime,
            session_only: false,
            completed: None,
            repair: Some(Repair {
                captured: cursor.clone(),
                witness: pending_repair_witness(&[owner]).0,
                active: true,
                scan: cursor,
                page: None,
                rewind: false,
                focus: None,
                conversation,
                local_epoch: 0,
                global_epoch: Some(global_epoch),
                owner,
                spent: [255; 32],
                scan_complete: false,
            }),
        };
        let sealed = node
            .seal_mailbox_hint(&key, bincode::serialize(&receipt).unwrap())
            .unwrap();
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .put_checkpoint(&key, &sealed)
            .unwrap();
        HINT_OPEN_ATTEMPTS.with(|count| count.set(0));
        let mut writes = None;
        let mut expected_spent = None;
        for round in 0..3 {
            let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
            let mut progress = OpaqueMailboxProgress::new(
                &relay.public(),
                &remote.public(),
                Some(conversation),
                mailbox,
            );
            let (mut channel, task) = connection(&node, office.clone(), &relay).await;
            assert_eq!(
                node.advance_incremental_mailbox(&mut channel, &mut progress, &|| true,)
                    .await
                    .unwrap(),
                Some(false)
            );
            let repair = state(&progress).receipt.repair.as_ref().unwrap();
            assert_eq!(repair.spent, expected_spent.unwrap_or([0; 32]));
            // This actual lease, not hint bytes, owns compaction protection.
            node.hold_mailbox_repair_lease(&mut progress).unwrap();
            if round == 0 {
                let pending = state(&progress).receipt.pending.clone();
                let repair = state_mut(&mut progress).receipt.repair.as_mut().unwrap();
                repair.mark_spent(&pending, owner);
                expected_spent = Some(repair.spent);
                assert_ne!(repair.spent, [0; 32]);
            }
            node.save_mailbox_receipt(&mut progress).unwrap();
            let actual_writes = node.storage.database().successful_write_commits();
            if let Some(writes) = writes {
                assert_eq!(
                    actual_writes, writes,
                    "hot repair progress must not be repaid and persisted"
                );
            } else {
                writes = Some(actual_writes);
            }
            drop(progress);
            drop(channel);
            assert_eq!(task.await.unwrap(), 0);
            assert_eq!(HINT_OPEN_ATTEMPTS.with(|count| count.get()), 1);
        }
        // Maintenance is allowed to rewind even a busy owner. Fence a receipt
        // captured before that same-generation rewind from restoring old work.
        let generation = node.privacy.generation.load(Ordering::Acquire);
        {
            let owner = node
                .opaque_upload_cache
                .lock()
                .unwrap()
                .entries
                .get(&key)
                .unwrap()
                .clone();
            let mut entry = owner.lock().unwrap();
            assert!(entry.repair_lease.is_some());
            entry.repair_progress = std::time::Instant::now() - std::time::Duration::from_secs(31);
            entry
                .receipt
                .as_mut()
                .unwrap()
                .repair
                .as_mut()
                .unwrap()
                .scan
                .after = 7;
        }
        let rewound = Arc::new(Mutex::new(None));
        let observed = rewound.clone();
        let weak = Arc::downgrade(&node);
        let hook_key = key.clone();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, stage| {
            if stage != "mailbox_receipt_before_warm" {
                return;
            }
            let node = weak.upgrade().unwrap();
            let mut cache = node.opaque_upload_cache.lock().unwrap();
            cache.clear_stale_receive_repair_leases(generation);
            let entry = cache.entries.get(&hook_key).unwrap().lock().unwrap();
            assert!(entry.busy);
            assert!(entry.repair_lease.is_none());
            let receipt = entry.receipt.clone().unwrap();
            let repair = receipt.repair.as_ref().unwrap();
            assert_eq!(repair.scan.after, 0);
            assert_eq!(repair.spent, [0; 32]);
            *observed.lock().unwrap() = Some(receipt);
        }));
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(),
            &remote.public(),
            Some(conversation),
            mailbox,
        );
        let (mut channel, task) = connection(&node, office, &relay).await;
        let result = node
            .advance_incremental_mailbox(&mut channel, &mut progress, &|| true)
            .await;
        *node.account_client_test_hook.lock().unwrap() = None;
        let owner = node
            .opaque_upload_cache
            .lock()
            .unwrap()
            .entries
            .get(&key)
            .unwrap()
            .clone();
        let (receipt, has_repair_lease, busy) = {
            let entry = owner.lock().unwrap();
            (
                entry.receipt.clone(),
                entry.repair_lease.is_some(),
                entry.busy,
            )
        };
        let expected = rewound.lock().unwrap().clone();
        // Failure must not poison a guard that Lease::drop needs for cleanup.
        assert!(
            receipt == expected,
            "warming must preserve the maintenance rewind"
        );
        assert!(!has_repair_lease);
        assert!(
            !busy,
            "stale initialization must release its acquired owner"
        );
        assert!(matches!(
            result,
            Err(session::SessionError::Store(LogError::StaleCursor))
        ));
        drop(progress);
        drop(channel);
        assert_eq!(task.await.unwrap(), 0);
    }

    #[tokio::test]
    async fn cold_receipt_runtime_normalization_preserves_validation_and_repair_reset() {
        let root = tempfile::tempdir().unwrap();
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
        let conversation = ConversationId::new([5; 32]);
        let (epoch, source) = node.mailbox_local_binding().unwrap();
        let runtime = node.opaque_receive_cache.lock().unwrap().session_epoch();
        let old_runtime = runtime.map(|byte| byte ^ 255);
        let owner = EventId::new([8; 32]);
        let cursor = MailboxCursor {
            store_uuid: [3; 16],
            conversation,
            epoch: 4,
            first_seen: Some(10),
            revision: 8,
            after: 0,
            window: Some((20, 9)),
        };
        let base = Receipt {
            cursor: Some(cursor.clone()),
            pending: vec![owner],
            epoch,
            source_floor: source,
            runtime: old_runtime,
            session_only: false,
            completed: None,
            repair: Some(Repair {
                captured: cursor.clone(),
                witness: [7; 32],
                active: true,
                scan: cursor,
                page: None,
                rewind: false,
                focus: None,
                conversation,
                local_epoch: epoch,
                global_epoch: None,
                owner,
                spent: [255; 32],
                scan_complete: false,
            }),
        };
        for variant in 0..5 {
            let key = [variant];
            let mut receipt = base.clone();
            match variant {
                1 => receipt.session_only = true,
                2 => receipt.epoch += 1,
                3 => receipt.source_floor += 1,
                4 => receipt.repair.as_mut().unwrap().active = false,
                _ => {}
            }
            let sealed = node
                .seal_mailbox_hint(&key, bincode::serialize(&receipt).unwrap())
                .unwrap();
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(&key, &sealed)
                .unwrap();
            let loaded = node
                .load_mailbox_receipt_with_runtime(&key, conversation, None)
                .unwrap();
            assert_eq!(loaded.runtime, runtime);
            if matches!(variant, 1..=3) {
                assert!(
                    loaded.cursor.is_none(),
                    "invalid durable receipt must not be normalized into validity"
                );
                assert!(loaded.pending.is_empty());
                assert!(loaded.repair.is_none());
            } else {
                assert!(loaded.cursor == base.cursor);
                if variant == 4 {
                    assert!(loaded.repair.is_none());
                } else {
                    let repair = loaded.repair.unwrap();
                    assert!(repair.active);
                    assert_eq!(repair.spent, [0; 32]);
                    assert!(repair.scan == base.repair.as_ref().unwrap().scan);
                }
            }
        }
    }

    #[tokio::test]
    async fn pending_children_cannot_consume_each_others_finite_repair_window() {
        assert_pending_children_repair_ownership(false).await;
    }

    #[tokio::test]
    async fn pending_children_prioritize_active_owner_without_stealing_its_page() {
        assert_pending_children_repair_ownership(true).await;
    }

    async fn assert_pending_children_repair_ownership(active_b: bool) {
        use crate::eventlog::sync::network::NetworkSyncStore;
        let root = tempfile::tempdir().unwrap();
        let relay_root = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let account = Account::generate();
        let proof =
            crate::discovery::Announce::new_with_account(&remote, &account, "Remote", 47475);
        let (node, _, _) = crate::node::test_support::node_with_files(
            root.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(false, true);
        let relay = DeviceIdentity::generate();
        let mut office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_root.path()).unwrap(),
            &relay_root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let conversation = crate::node::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let mut profiles = Vec::new();
        for revision in 1..=12 {
            let payload = crate::node::profile::ProfilePayload::new(&account, None, revision);
            let event = Event::new(
                &remote,
                conversation,
                revision,
                vec![],
                revision,
                0,
                EventKind::Profile,
                crate::dm::seal(
                    &remote,
                    &node.identity.public().x25519_pub,
                    &payload.encode(),
                )
                .unwrap(),
            );
            profiles.push(event.id);
            office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
        }
        let (mut channel, task) = connection(&node, office.clone(), &relay).await;
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(),
            &remote.public(),
            None,
            RelayMailbox::federated(&node.identity, &remote.public()).unwrap(),
        );
        while !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
        {}
        let key = state(&progress).key.clone();
        drop(progress);
        drop(channel);
        task.await.unwrap();
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .compact_superseded_profiles(&conversation)
                .unwrap(),
            10
        );
        let parent_b = profiles[9];
        assert!(!node.log.lock().unwrap().has(&parent_b).unwrap());
        let child_a = Event::new(
            &remote,
            conversation,
            13,
            vec![EventId::new([249; 32])],
            13,
            0,
            EventKind::Message,
            vec![13; 128],
        );
        let child_b = Event::new(
            &remote,
            conversation,
            14,
            vec![parent_b],
            14,
            0,
            EventKind::Message,
            vec![14; 128],
        );
        let outer_a = mailbox.wrap(&child_a).unwrap();
        let outer_b = mailbox.wrap(&child_b).unwrap();
        office.ingest(outer_a.clone()).unwrap();
        office.ingest(outer_b.clone()).unwrap();
        let wrapper_key = hint_key(mailbox.id(), parent_b);
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_wrapper_hint(&wrapper_key, b"unavailable parent mapping")
                .unwrap()
        );
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_pending_wrapper_insert BEFORE INSERT ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;
                CREATE TRIGGER fail_pending_wrapper_update BEFORE UPDATE ON opaque_wrapper_ids BEGIN SELECT RAISE(ABORT,'optional wrapper hint unavailable'); END;")?;
            Ok(())
        }).unwrap();
        // This signed state is reachable after an earlier interrupted B repair:
        // both wrappers are durable pending intent, and their finite IDs have
        // already been received. Neither child has been admitted.
        let mut receipt = node.load_mailbox_receipt(&key, mailbox.id()).unwrap();
        receipt.cursor = Some(
            office
                .mailbox_delta_page(&mailbox.id(), None, 64)
                .unwrap()
                .cursor,
        );
        receipt.pending = vec![outer_a.id, outer_b.id];
        receipt.repair = if active_b {
            let page = office.mailbox_delta_page(&mailbox.id(), None, 64).unwrap();
            let mut scan = page.cursor.clone();
            scan.after = 0;
            scan.window = Some((page.cursor.after, page.cursor.revision));
            let (local_epoch, global_epoch) = {
                let log = node.log.lock().unwrap();
                let repository = log.repository().unwrap();
                (
                    repository
                        .conversation_destructive_epoch(&conversation)
                        .unwrap(),
                    repository.destructive_policy_revision().unwrap(),
                )
            };
            Some(Repair {
                captured: page.cursor.clone(),
                witness: pending_repair_witness(&receipt.pending).0,
                active: true,
                scan,
                page: Some(RepairPage {
                    cursor: page.cursor,
                    ids: page.ids,
                    next: 9,
                    complete: true,
                }),
                rewind: false,
                focus: None,
                conversation,
                local_epoch,
                global_epoch: Some(global_epoch),
                owner: outer_b.id,
                spent: [0; 32],
                scan_complete: false,
            })
        } else {
            None
        };
        let signed = node
            .seal_mailbox_hint(&key, bincode::serialize(&receipt).unwrap())
            .unwrap();
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(&key, &signed)
                .unwrap()
        );
        node.opaque_upload_cache.lock().unwrap().clear();
        let (mut channel, task) = connection(&node, office.clone(), &relay).await;
        let mut progress = OpaqueMailboxProgress::new(
            &relay.public(),
            &remote.public(),
            None,
            RelayMailbox::federated(&node.identity, &remote.public()).unwrap(),
        );
        let mut cut_after_a = false;
        for _ in 0..8 {
            node.advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap();
            if state(&progress)
                .receipt
                .repair
                .as_ref()
                .is_some_and(|repair| !repair.active)
            {
                if active_b {
                    assert!(
                        node.log.lock().unwrap().has(&child_b.id).unwrap(),
                        "the active child must advance first using its own saved page and required parents"
                    );
                    assert!(!node.log.lock().unwrap().has(&child_a.id).unwrap());
                    drop(progress);
                    drop(channel);
                    task.await.unwrap();
                    return;
                }
                assert!(state(&progress).receipt.pending.contains(&outer_a.id));
                assert!(state(&progress).receipt.pending.contains(&outer_b.id));
                cut_after_a = true;
                break;
            }
        }
        assert!(
            cut_after_a,
            "the fixture must interrupt after A's failed finite scan, before B retry"
        );
        assert!(!node.log.lock().unwrap().has(&child_b.id).unwrap());
        drop(progress);
        drop(channel);
        task.await.unwrap();
        let (mut channel, task) = connection(&node, office, &relay).await;
        let mut progress =
            OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
        let mut finished = false;
        for _ in 0..16 {
            if node
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
            {
                finished = true;
                break;
            }
        }
        assert!(
            finished,
            "the fresh finite round must complete without a protocol/setup failure"
        );
        assert!(
            node.log.lock().unwrap().has(&child_b.id).unwrap(),
            "A's failed repair cannot spend B's window or discard B's only recoverable parent mapping"
        );
        assert!(!node.log.lock().unwrap().has(&child_a.id).unwrap());
        drop(progress);
        drop(channel);
        task.await.unwrap();
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
                node.opaque_upload_cache.lock().unwrap().clear();
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
            node.opaque_upload_cache.lock().unwrap().clear();
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

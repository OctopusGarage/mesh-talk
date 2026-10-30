//! Original preparation and local data binding for account-owned File effects.
use super::{DeliveryTransaction, OperationReference, RequestRecord, invalid};
use crate::{
    discovery::Announce,
    eventlog::{LogError, persist::PersistentEventLog},
    node::account_group::operations::file_preparation::{GroupFileAssetsReceipt, GroupFilePayload},
};
use serde::{Deserialize, Serialize};

/// Appended WAL wrapper data. This never enters a public group event or status DTO.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct GroupFileWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) payload: GroupFilePayload,
    pub(in crate::node) ready: GroupFileAssetsReceipt,
}

pub(super) fn validate_bound(
    operation: &OperationReference,
    request: &RequestRecord,
    witness: &GroupFileWitness,
    transaction: &DeliveryTransaction,
) -> Result<(), LogError> {
    use crate::node::{AccountGroupOperation, AccountGroupOperationKind};
    let descriptor = witness
        .payload
        .descriptor
        .as_ref()
        .ok_or_else(|| invalid("file preparation missing"))?;
    let own_account = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("file source account missing"))?;
    if matches!(&operation.executor.execution, crate::node::AccountGroupExecution::ViaDevice(device) if *device != operation.executor.device)
        || operation.kind != AccountGroupOperationKind::File
        || !witness.own.verify()
        || witness.own.post_office
        || witness.own.public() != operation.executor.device
        || !operation.valid(&own_account.account_ed25519_pub)
        || !descriptor.valid_for(operation, &witness.payload.logical)
        || !witness.ready.valid_for(&witness.payload)
    {
        return Err(invalid(
            "invalid original file preparation or actual source binding",
        ));
    }
    let input = AccountGroupOperation::Send {
        message: witness
            .payload
            .prepared()
            .ok_or_else(|| invalid("file preparation exceeds body bounds"))?,
    };
    super::group_message::validate_prepared(
        operation,
        request,
        transaction,
        &input,
        &descriptor.manifest,
        crate::eventlog::EventKind::FileManifest,
    )
}

pub(super) fn validate_parent_view(
    operation: &OperationReference,
    request: &RequestRecord,
    witness: &GroupFileWitness,
    transaction: &DeliveryTransaction,
    log: &PersistentEventLog,
) -> Result<(), LogError> {
    use crate::node::{Node, channel::ChannelBook};
    use std::collections::{HashMap, HashSet};
    validate_bound(operation, request, witness, transaction)?;
    let DeliveryTransaction::ChannelOutgoing { events, .. } = transaction else {
        return Err(invalid("file outgoing batch missing"));
    };
    let first = events
        .first()
        .ok_or_else(|| invalid("file outgoing effect missing"))?;
    let mut ancestors = HashMap::new();
    let mut pending = first.parents.clone();
    let effects: HashSet<_> = events.iter().map(|event| event.id).collect();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let event = log
            .get(&id)
            .ok_or_else(|| invalid("file causal parent missing"))?;
        if event.conversation_id != operation.group || effects.contains(&id) {
            return Err(invalid("invalid file causal parent"));
        }
        pending.extend(event.parents.iter().copied());
        ancestors.insert(id, event);
    }
    let parents: Vec<_> = ancestors.values().copied().collect();
    let projection = ChannelBook::project_membership(operation.group, &parents);
    let mut source = Node::public_account_group_source(
        operation.group,
        &projection,
        &witness.own.public(),
        &witness.own,
    )
    .ok_or_else(|| invalid("file source not accepted in saved causal view"))?;
    source.execution = operation.executor.execution.clone();
    if source != operation.executor {
        return Err(invalid("file source lifetime or authority changed"));
    }
    let max_clock = parents.iter().map(|event| event.lamport).max().unwrap_or(0);
    let max_seq = parents
        .iter()
        .filter(|event| event.author == first.author)
        .map(|event| event.seq)
        .max()
        .unwrap_or(0);
    if max_clock.checked_add(1) != Some(first.lamport) || max_seq.checked_add(1) != Some(first.seq)
    {
        return Err(invalid("invalid original file causal allocation"));
    }
    let mut heads: Vec<_> = ancestors.keys().copied().collect();
    let referenced: HashSet<_> = parents
        .iter()
        .flat_map(|event| event.parents.iter().copied())
        .collect();
    heads.retain(|id| !referenced.contains(id));
    heads.sort();
    if heads != first.parents {
        return Err(invalid("file causal frontier changed"));
    }
    if let Some(rotation) = events
        .first()
        .filter(|event| event.kind == crate::eventlog::EventKind::KeyRotation)
    {
        let keys = crate::node::channel::SealedKeys::decode(&rotation.ciphertext)
            .ok_or_else(|| invalid("file sender distribution missing"))?;
        let actual: HashSet<_> = keys
            .entries
            .iter()
            .map(|entry| entry.user_id.clone())
            .collect();
        let expected: HashSet<_> = projection
            .state
            .as_ref()
            .ok_or_else(|| invalid("file channel state missing"))?
            .recipient_members()
            .iter()
            .map(|member| member.user_id())
            .collect();
        if actual != expected {
            return Err(invalid(
                "file sender recipients differ from saved membership",
            ));
        }
    }
    let receipt = &witness.ready;
    if log.fingerprint_and_len(&receipt.file_conv)
        != (receipt.fingerprint, receipt.event_count as usize)
    {
        return Err(invalid("file ciphertext differs from ready receipt"));
    }
    if log.events(&receipt.file_conv).iter().any(|event| {
        event.kind != crate::eventlog::EventKind::Message
            || event.author.ed25519_pub() != &operation.executor.device.ed25519_pub
    }) {
        return Err(invalid("file ciphertext belongs to another signer"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::{ChannelMeta, ChannelState},
        discovery::Roster,
        eventlog::{ConversationId, Event, EventKind},
        file::{
            FileKey, FileKind, FileManifestV2, FileManifestV3, chunk_hash, file_checksum,
            seal_chunk_indexed,
        },
        identity::{account::Account, device::DeviceIdentity},
        node::{
            AccountGroupExecution, AccountGroupOperationKind, AccountGroupRole, AccountGroupSource,
            AuthorityExpectation, MembershipLifetime, MessageDestination, Node,
            account_group::operations::file_preparation::{
                GroupFileDescriptor, GroupFileLogical, group_file_asset_digest,
            },
        },
    };
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct Fixture {
        _dir: tempfile::TempDir,
        operation: OperationReference,
        request: RequestRecord,
        witness: GroupFileWitness,
        body: DeliveryTransaction,
        log: PersistentEventLog,
    }
    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = DeviceIdentity::generate();
        let owner = DeviceIdentity::generate();
        let (dm, _) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let origin = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &dir.path().join("origin-events"),
            &dir.path().join("origin-sent"),
            "pw",
        )
        .unwrap();
        let group = ConversationId::new([0x91; 32]);
        let meta = ChannelMeta {
            name: "File WAL".into(),
            members: vec![owner.public(), source.public()],
            epoch: 0,
            owner: owner.user_id(),
        };
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            meta.encode(),
        );
        let file = ConversationId::new([0x92; 32]);
        let plaintext = b"actual explicit file bytes";
        let ciphertext =
            seal_chunk_indexed(&FileKey::from_bytes([0x93; 32]), &[0x94; 8], 0, plaintext).unwrap();
        let chunk = Event::new(
            &source,
            file,
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            ciphertext.clone(),
        );
        let manifest = FileManifestV3 {
            v2: FileManifestV2 {
                name: "explicit.bin".into(),
                size: plaintext.len() as u64,
                mime: "application/octet-stream".into(),
                checksum: file_checksum(plaintext),
                file_key: [0x93; 32],
                file_nonce: [0x94; 8],
                file_conv: file,
                chunk_size: crate::file::CHUNK_SIZE as u32,
                chunk_count: 1,
                chunk_hashes: vec![chunk_hash(plaintext)],
            },
            kind: FileKind::File,
        }
        .encode();
        let logical = GroupFileLogical::from_manifest(&manifest).unwrap();
        let mut payload = GroupFilePayload {
            logical,
            descriptor: None,
        };
        let operation = OperationReference::new(
            &origin,
            origin
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &payload.logical_operation().unwrap(),
            AccountGroupSource {
                device: source.public(),
                role: AccountGroupRole::Member,
                lifetime: MembershipLifetime {
                    member: source.public(),
                    membership: genesis.id,
                    acceptance: None,
                },
                authority: AuthorityExpectation {
                    authority: None,
                    epoch: 0,
                },
                execution: AccountGroupExecution::Local,
            },
        );
        payload.descriptor = Some(GroupFileDescriptor::new(
            &origin,
            operation.id,
            group,
            operation.digest,
            manifest.clone(),
            group_file_asset_digest([ciphertext.as_slice()]),
        ));
        let mut log = PersistentEventLog::open(&dir.path().join("source-log"), "pw").unwrap();
        log.append(genesis.clone()).unwrap();
        log.append(chunk).unwrap();
        log.sync().unwrap();
        let (fingerprint, event_count) = log.fingerprint_and_len(&file);
        let ready = GroupFileAssetsReceipt {
            file_conv: file,
            fingerprint,
            event_count: event_count as u32,
            asset_digest: payload.descriptor.as_ref().unwrap().asset_ciphertext_digest,
        };
        let mut state = ChannelState::from_meta(group, meta);
        state.set_identity(source.user_id());
        let (sealed, distribution, senders) = state.prepare_sender_message(&manifest).unwrap();
        let rotation = crate::node::channel::seal_keys_for(
            &source,
            &state.recipient_members(),
            &distribution.unwrap(),
            0,
        )
        .unwrap();
        let rotation = Event::new(
            &source,
            group,
            1,
            vec![genesis.id],
            2,
            2,
            EventKind::KeyRotation,
            rotation.encode(),
        );
        let event = Event::new(
            &source,
            group,
            2,
            vec![rotation.id],
            3,
            2,
            EventKind::FileManifest,
            sealed,
        );
        let request = RequestRecord {
            request_id: *operation.id.as_bytes(),
            digest: operation.digest,
            destination: MessageDestination::Channel(group),
            message_id: event.id,
            accepted_at_ms: 2,
        };
        let body = DeliveryTransaction::ChannelOutgoing {
            events: vec![rotation, event],
            sent: crate::node::sentlog::SentEntry {
                conversation: group,
                seq: 2,
                wall_clock: 2,
                plaintext: manifest,
            },
            senders,
        };
        let witness = GroupFileWitness {
            own: Announce::new_with_account(&source, &account, "Source", 9),
            payload,
            ready,
        };
        assert_eq!(operation.kind, AccountGroupOperationKind::File);
        Fixture {
            _dir: dir,
            operation,
            request,
            witness,
            body,
            log,
        }
    }
    fn wrapped(f: &Fixture) -> DeliveryTransaction {
        DeliveryTransaction::GroupRequested {
            operation: f.operation.clone(),
            transaction: Box::new(DeliveryTransaction::Requested {
                request: f.request.clone(),
                transaction: Box::new(DeliveryTransaction::AccountGroupFile {
                    witness: Box::new(f.witness.clone()),
                    transaction: Box::new(
                        bincode::deserialize(&bincode::serialize(&f.body).unwrap()).unwrap(),
                    ),
                }),
            }),
        }
    }
    fn store(f: &Fixture) -> super::super::DeliveryStore {
        let mut store = super::super::DeliveryStore::open(f._dir.path(), "pw").unwrap();
        store
            .bind_profile(
                &f._dir.path().join("source-log"),
                "pw",
                &f.operation.executor.device,
                &f.witness.own.account_id().unwrap(),
            )
            .unwrap();
        store
    }
    #[test]
    fn typed_file_enters_original_requested_wal_without_changing_the_effect() {
        let f = fixture();
        let mut store = store(&f);
        let id = f.request.message_id;
        assert_eq!(
            store
                .begin(wrapped(&f))
                .expect("the typed original File preparation must enter the existing WAL"),
            id
        );
        assert_eq!(store.pending_transactions().len(), 1);
        assert_eq!(store.pending_transactions()[0].id(), id);
    }
    #[test]
    fn typed_file_terminal_failure_reopens_and_reuses_exact_manifest_and_sidecars() {
        use crate::node::{
            channel_senders::ChannelSenderStore, dm_ratchet::DmRatchet,
            ratchet_sessions::RatchetSessions, received_log::ReceivedLog, sentlog::SentLog,
        };
        let mut f = fixture();
        let mut delivery = store(&f);
        delivery
            .begin(wrapped(&f))
            .expect("prepared File must enter WAL before exercising recovery");
        let root = f._dir.path();
        let mut ratchet =
            DmRatchet::new(RatchetSessions::open(&root.join("sessions"), "pw").unwrap());
        let mut sent = SentLog::open(&root.join("sent"), "pw").unwrap();
        let mut received = ReceivedLog::open(&root.join("received"), "pw").unwrap();
        let mut files = ReceivedLog::open_file_cards(&root.join("files"), "pw").unwrap();
        let mut senders = ChannelSenderStore::open(&root.join("senders"), "pw").unwrap();
        let mut failed =
            |_: &OperationReference, _: &[Event]| Err(invalid("forced terminal write failure"));
        assert!(
            delivery
                .recover_next_with_group_operations(
                    &mut ratchet,
                    &mut f.log,
                    &mut sent,
                    &mut received,
                    Some(&mut files),
                    Some(&mut senders),
                    Some(&mut failed)
                )
                .is_err()
        );
        assert!(f.log.get(&f.request.message_id).is_some());
        assert!(files.entry(f.request.message_id).is_some_and(
            |row| row.plaintext == f.witness.payload.descriptor.as_ref().unwrap().manifest
        ));
        assert_eq!(delivery.pending_transactions().len(), 1);
        drop(delivery);
        let mut delivery = store(&f);
        let mut completed = false;
        let mut complete = |reference: &OperationReference, effects: &[Event]| {
            assert!(
                serde_json::to_vec(reference).unwrap() == serde_json::to_vec(&f.operation).unwrap()
            );
            assert_eq!(effects.last().unwrap().id, f.request.message_id);
            completed = true;
            Ok(())
        };
        assert_eq!(
            delivery
                .recover_next_with_group_operations(
                    &mut ratchet,
                    &mut f.log,
                    &mut sent,
                    &mut received,
                    Some(&mut files),
                    Some(&mut senders),
                    Some(&mut complete)
                )
                .unwrap(),
            Some(f.request.message_id)
        );
        assert!(completed);
        assert!(delivery.pending_transactions().is_empty());
        assert_eq!(
            f.log
                .events(&f.operation.group)
                .iter()
                .filter(|event| event.kind == EventKind::FileManifest)
                .count(),
            1
        );
        assert_eq!(
            f.log.fingerprint_and_len(&f.witness.ready.file_conv),
            (
                f.witness.ready.fingerprint,
                f.witness.ready.event_count as usize
            )
        );
    }
    #[test]
    fn member_file_preparation_binds_the_exact_original_manifest_and_sender_batch() {
        let f = fixture();
        validate_bound(&f.operation, &f.request, &f.witness, &f.body)
            .expect("a real member File batch must bind the original independent preparation");
    }
    #[test]
    fn member_file_saved_parent_view_validates_exact_source_and_ready_ciphertext() {
        let f = fixture();
        validate_parent_view(&f.operation, &f.request, &f.witness, &f.body, &f.log).expect(
            "recovery must validate the saved actual membership and exact local data receipt",
        );
    }
    #[test]
    fn file_binding_rejects_changed_descriptor_manifest_input_and_receipt() {
        let f = fixture();
        let mut witness = f.witness.clone();
        witness.payload.descriptor.as_mut().unwrap().signature[0] ^= 1;
        assert!(validate_bound(&f.operation, &f.request, &witness, &f.body).is_err());
        let mut witness = f.witness.clone();
        witness.ready.asset_digest[0] ^= 1;
        assert!(validate_bound(&f.operation, &f.request, &witness, &f.body).is_err());
        let mut body: DeliveryTransaction =
            bincode::deserialize(&bincode::serialize(&f.body).unwrap()).unwrap();
        let DeliveryTransaction::ChannelOutgoing { sent, .. } = &mut body else {
            unreachable!()
        };
        sent.plaintext.push(0);
        assert!(validate_bound(&f.operation, &f.request, &f.witness, &body).is_err());
    }
    #[test]
    fn file_parent_view_rejects_missing_data_or_a_different_receipt_fingerprint() {
        let f = fixture();
        let mut witness = f.witness.clone();
        witness.ready.fingerprint[0] ^= 1;
        assert!(validate_parent_view(&f.operation, &f.request, &witness, &f.body, &f.log).is_err());
        let mut request = f.request.clone();
        request.request_id[0] ^= 1;
        assert!(validate_bound(&f.operation, &request, &f.witness, &f.body).is_err());
    }
}

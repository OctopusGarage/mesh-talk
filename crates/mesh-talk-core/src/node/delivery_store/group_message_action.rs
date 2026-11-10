//! Original account intent binding for existing sealed React/Delete effects.
//!
//! Store admission calls validate_bound. Before publishing an event, recovery
//! calls validate_parent_view and the message-action owner's validate_self_payload
//! with the source identity. Replay installs the exact saved RecallMark alongside
//! the event before completing the original RequestRecord; it never manufactures
//! a new event or rechecks current clock/caches for an accepted WAL entry.

use super::{OperationReference, RequestRecord, invalid};
use crate::{
    eventlog::{Event, EventId, LogError, persist::PersistentEventLog},
    identity::device::PublicIdentity,
    node::{
        AccountGroupRole, MessageDestination, Node,
        channel::{ChannelBook, SealedPayload},
        message_actions::{MessageActionInput, MessageActionWitness, expected_payload},
        recall::RecallMark,
    },
};
use std::collections::{HashMap, HashSet};

pub(super) fn validate_bound(
    operation: &OperationReference,
    request: &RequestRecord,
    input: &MessageActionInput,
    witness: &MessageActionWitness,
    event: &Event,
    recall: Option<&RecallMark>,
) -> Result<(), LogError> {
    let certificate = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("message action source has no account"))?;
    if !witness.own.valid_presence()
        || witness.own.post_office
        || !operation.valid(&certificate.account_ed25519_pub)
        || matches!(&operation.executor.execution, crate::node::AccountGroupExecution::ViaDevice(device) if *device != operation.executor.device)
        || operation.executor.device != witness.own.public()
        || !matches!(
            operation.executor.role,
            AccountGroupRole::Member | AccountGroupRole::Owner
        )
        || operation.kind != input.operation().kind()
        || crate::node::account_group::operations::journal::operation_digest(
            operation.group,
            &input.operation(),
        ) != operation.digest
        || request.request_id != *operation.id.as_bytes()
        || request.digest != operation.digest
        || request.message_id != event.id
        || request.destination != MessageDestination::Channel(operation.group)
        || event.conversation_id != operation.group
        || event.author.ed25519_pub() != &witness.own.ed25519_pub
        || event.seq == 0
        || event.lamport == 0
        || event.parents.is_empty()
        || event.parents != witness.parents
        || event.wall_clock != witness.admitted_at_ms
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
        || !crate::node::session::event_fits_frame(event)
    {
        return Err(invalid("invalid original message action binding"));
    }
    let (kind, _, expected_recall) = expected_payload(operation.group, input, witness)
        .map_err(|_| invalid("invalid message action decision"))?;
    let sealed = SealedPayload::decode(&event.ciphertext)
        .ok_or_else(|| invalid("invalid sealed message action"))?;
    let mut recipients = HashSet::new();
    if event.kind != kind
        || expected_recall.as_ref() != recall
        || sealed.encode() != event.ciphertext
        || sealed.sender != witness.own.public().user_id()
        || sealed.entries.is_empty()
        || sealed
            .entries
            .iter()
            .any(|entry| entry.sealed.is_empty() || !recipients.insert(entry.user_id.clone()))
        || !recipients.contains(&witness.own.public().user_id())
    {
        return Err(invalid("invalid sealed message action effect"));
    }
    Ok(())
}

/// Only the saved causal cut establishes authority/recipients on replay. Later
/// revocation, expiry, sibling presence and recipient cache eviction are irrelevant.
pub(super) fn validate_parent_view(
    operation: &OperationReference,
    request: &RequestRecord,
    input: &MessageActionInput,
    witness: &MessageActionWitness,
    event: &Event,
    recall: Option<&RecallMark>,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    validate_bound(operation, request, input, witness, event, recall)?;
    let mut ancestors: HashMap<EventId, Event> = HashMap::new();
    let mut pending = witness.parents.clone();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let parent = log
            .get(&id)?
            .ok_or_else(|| invalid("message action causal parent missing"))?;
        if parent.conversation_id != operation.group || parent.id == event.id {
            return Err(invalid("invalid message action causal parent"));
        }
        pending.extend(parent.parents.iter().copied());
        ancestors.insert(id, parent);
    }
    let events: Vec<_> = ancestors.values().collect();
    let projection = ChannelBook::project_membership(operation.group, &events);
    let mut source = Node::public_account_group_source(
        operation.group,
        &projection,
        &witness.own.public(),
        &witness.own,
    )
    .ok_or_else(|| invalid("message action source not authorized in saved view"))?;
    source.execution = operation.executor.execution.clone();
    if source != operation.executor {
        return Err(invalid("message action pinned source changed"));
    }
    let state = projection
        .state
        .as_ref()
        .ok_or_else(|| invalid("message action group missing"))?;
    let sealed = SealedPayload::decode(&event.ciphertext)
        .ok_or_else(|| invalid("invalid sealed message action"))?;
    if sealed
        .entries
        .iter()
        .map(|entry| entry.user_id.clone())
        .collect::<Vec<_>>()
        != state
            .recipient_members()
            .iter()
            .map(|public| public.user_id())
            .collect::<Vec<_>>()
    {
        return Err(invalid("message action recipients changed"));
    }
    if let Some(target) = &witness.target
        && ancestors.get(&target.original.id) != Some(&target.original)
    {
        return Err(invalid("recall target missing from saved causal view"));
    }
    let max_clock = events
        .iter()
        .map(|parent| parent.lamport)
        .max()
        .unwrap_or(0);
    let max_seq = events
        .iter()
        .filter(|parent| parent.author == event.author)
        .map(|parent| parent.seq)
        .max()
        .unwrap_or(0);
    let parent_ids: HashSet<_> = events
        .iter()
        .flat_map(|parent| parent.parents.iter().copied())
        .collect();
    let mut heads: Vec<_> = ancestors
        .keys()
        .copied()
        .filter(|id| !parent_ids.contains(id))
        .collect();
    heads.sort();
    if heads != witness.parents
        || max_clock.checked_add(1) != Some(event.lamport)
        || max_seq.checked_add(1) != Some(event.seq)
    {
        return Err(invalid("invalid message action causal allocation"));
    }
    Ok(state.recipient_members())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        eventlog::{ConversationId, EventKind},
        identity::{account::Account, device::DeviceIdentity},
        node::{AccountGroupOperationId, message_actions::MessageActionTarget},
    };

    struct Fixture {
        own: DeviceIdentity,
        origin: DeviceIdentity,
        operation: OperationReference,
        request: RequestRecord,
        input: MessageActionInput,
        witness: MessageActionWitness,
        event: Event,
        recall: Option<RecallMark>,
        log: PersistentEventLog,
        _dir: tempfile::TempDir,
    }
    fn fixture(recall: bool) -> Fixture {
        let own = DeviceIdentity::generate();
        let origin = DeviceIdentity::generate();
        let account = Account::generate();
        let group = ConversationId::new([81; 32]);
        let metadata = ChannelMeta {
            name: "Actions".into(),
            members: vec![own.public()],
            epoch: 0,
            owner: own.user_id(),
        };
        let genesis = Event::new(
            &own,
            group,
            1,
            vec![],
            1,
            900,
            EventKind::MembershipChange,
            metadata.encode(),
        );
        let target = Event::new(
            &own,
            group,
            2,
            vec![genesis.id],
            2,
            1_000,
            EventKind::Message,
            vec![1],
        );
        let own_proof = Announce::new_with_account(&own, &account, "source", 9);
        let projection = ChannelBook::project_membership(group, &[&genesis, &target]);
        let source =
            Node::public_account_group_source(group, &projection, &own.public(), &own_proof)
                .unwrap();
        let input = if recall {
            MessageActionInput::Recall { target: target.id }
        } else {
            MessageActionInput::React {
                target: target.id,
                emoji: "👍".into(),
                remove: false,
            }
        };
        let witness = MessageActionWitness {
            own: own_proof,
            target: recall.then(|| MessageActionTarget {
                original: target.clone(),
                expires_at_ms: None,
                protected_file: false,
            }),
            parents: vec![target.id],
            admitted_at_ms: 1_001,
        };
        let (kind, payload, mark) = expected_payload(group, &input, &witness).unwrap();
        let sealed = SealedPayload::seal(&own, &[own.public()], &payload).unwrap();
        let event = Event::new(
            &own,
            group,
            3,
            witness.parents.clone(),
            3,
            1_001,
            kind,
            sealed.encode(),
        );
        let mut operation = OperationReference {
            id: AccountGroupOperationId::for_origin(&origin.public(), [82; 16]),
            group,
            kind: input.operation().kind(),
            digest: crate::node::account_group::operations::journal::operation_digest(
                group,
                &input.operation(),
            ),
            origin: Announce::new_with_account(&origin, &account, "origin", 9),
            executor: source,
            signature: vec![],
        };
        operation.signature = origin.sign(&operation.message()).to_vec();
        let request = RequestRecord {
            request_id: *operation.id.as_bytes(),
            digest: operation.digest,
            destination: MessageDestination::Channel(group),
            message_id: event.id,
            accepted_at_ms: 1_001,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        log.append_durable(genesis).unwrap();
        log.append_durable(target).unwrap();
        Fixture {
            own,
            origin,
            operation,
            request,
            input,
            witness,
            event,
            recall: mark,
            log,
            _dir: dir,
        }
    }
    fn validate(f: &Fixture) -> Result<(), LogError> {
        validate_parent_view(
            &f.operation,
            &f.request,
            &f.input,
            &f.witness,
            &f.event,
            f.recall.as_ref(),
            &f.log,
        )
        .map(|_| ())
    }
    #[test]
    fn action_replay_uses_original_causal_view_and_saved_clock() {
        for recall in [false, true] {
            let f = fixture(recall);
            validate(&f).unwrap();
            crate::node::message_actions::validate_self_payload(
                &f.own,
                &f.input,
                &f.witness,
                &f.event,
                &[f.own.public()],
            )
            .unwrap();
            // Timestamp 1001 is deliberately far older than the current clock.
            // A valid accepted WAL effect remains recoverable after its window.
            assert_eq!(f.witness.admitted_at_ms, 1_001);
        }
    }
    #[test]
    fn reaction_binding_rejects_changed_emoji_removal_or_target() {
        for case in 0..3 {
            let mut f = fixture(false);
            let MessageActionInput::React {
                target,
                emoji,
                remove,
            } = &mut f.input
            else {
                unreachable!()
            };
            match case {
                0 => *emoji = "🔥".into(),
                1 => *remove = true,
                _ => *target = EventId::new([84; 32]),
            }
            assert!(validate(&f).is_err());
        }
    }
    #[test]
    fn action_binding_rejects_changed_original_request_or_effect() {
        for case in 0..4 {
            let mut f = fixture(true);
            match case {
                0 => f.request.request_id = [85; 32],
                1 => f.request.digest = [86; 32],
                2 => f.request.message_id = EventId::new([87; 32]),
                _ => f.witness.admitted_at_ms += 1,
            }
            assert!(validate(&f).is_err());
        }
    }
    #[test]
    fn action_binding_rejects_signed_route_to_another_executor() {
        let mut f = fixture(false);
        f.operation.executor.execution =
            crate::node::AccountGroupExecution::ViaDevice(DeviceIdentity::generate().public());
        f.operation.signature = f.origin.sign(&f.operation.message()).to_vec();
        assert!(validate(&f).is_err());
    }

    #[test]
    fn recall_requires_exact_original_tombstone() {
        let mut f = fixture(true);
        f.recall.as_mut().unwrap().wall_clock += 1;
        assert!(validate(&f).is_err());
    }
    #[test]
    fn changed_signed_intent_cannot_reuse_different_sealed_payload() {
        let mut f = fixture(false);
        let MessageActionInput::React { remove, .. } = &mut f.input else {
            unreachable!()
        };
        *remove = true;
        f.operation.digest = crate::node::account_group::operations::journal::operation_digest(
            f.operation.group,
            &f.input.operation(),
        );
        f.operation.signature = f.origin.sign(&f.operation.message()).to_vec();
        f.request.digest = f.operation.digest;
        // Public structure alone cannot open sealed payloads; runtime confirmation
        // is mandatory and catches the changed original intent before publication.
        validate(&f).unwrap();
        assert!(
            crate::node::message_actions::validate_self_payload(
                &f.own,
                &f.input,
                &f.witness,
                &f.event,
                &[f.own.public()]
            )
            .is_err()
        );
    }
    fn transaction(f: &Fixture) -> super::super::DeliveryTransaction {
        use super::super::DeliveryTransaction;
        DeliveryTransaction::GroupRequested {
            operation: f.operation.clone(),
            transaction: Box::new(DeliveryTransaction::Requested {
                request: f.request.clone(),
                transaction: Box::new(DeliveryTransaction::ChannelMessageAction {
                    input: f.input.clone(),
                    witness: f.witness.clone(),
                    events: vec![f.event.clone()],
                    recall: f.recall.clone(),
                }),
            }),
        }
    }
    fn store(f: &Fixture) -> super::super::DeliveryStore {
        let mut store = super::super::DeliveryStore::open(f._dir.path(), "pw").unwrap();
        store
            .bind_profile(
                &f._dir.path().join("events"),
                "pw",
                &f.own.public(),
                &f.witness.own.account_id().unwrap(),
            )
            .unwrap();
        store
    }
    fn sides(
        f: &Fixture,
    ) -> (
        crate::node::dm_ratchet::DmRatchet,
        crate::node::sentlog::SentLog,
        crate::node::received_log::ReceivedLog,
    ) {
        (
            crate::node::dm_ratchet::DmRatchet::new(
                crate::node::RatchetSessions::open(&f._dir.path().join("sessions"), "pw").unwrap(),
            ),
            crate::node::sentlog::SentLog::open(&f._dir.path().join("sent"), "pw").unwrap(),
            crate::node::received_log::ReceivedLog::open(&f._dir.path().join("received"), "pw")
                .unwrap(),
        )
    }
    #[test]
    fn bound_actions_enter_real_wal_and_recover_exact_effect_after_reopen() {
        for recall in [false, true] {
            let mut f = fixture(recall);
            let mut delivery = store(&f);
            delivery.begin(transaction(&f)).unwrap();
            drop(delivery);
            let mut delivery = store(&f);
            assert_eq!(delivery.pending_transactions()[0].id(), f.event.id);
            let (mut ratchet, mut sent, mut received) = sides(&f);
            let expected = f.recall.clone();
            let operation_id = f.operation.id;
            let event_id = f.event.id;
            let recall_path = f._dir.path().join("recall");
            let mut recalls = crate::node::recall::RecallLog::open(&recall_path, "pw").unwrap();
            let terminal_path = f._dir.path().join("terminal");
            let (mut terminal, _): (
                crate::storage::record_log::EncryptedRecordLog<Vec<EventId>>,
                _,
            ) = crate::storage::record_log::EncryptedRecordLog::open(
                &terminal_path,
                "pw",
                b"MTACTN",
            )
            .unwrap();
            let mut complete = |operation: &OperationReference, events: &[Event]| {
                assert_eq!(operation.id, operation_id);
                assert_eq!(
                    events.iter().map(|event| event.id).collect::<Vec<_>>(),
                    vec![event_id]
                );
                if let Some(mark) = &expected {
                    recalls.record(mark.conversation, mark.target, mark.wall_clock)?;
                }
                terminal.append_durable(&vec![event_id])
            };
            assert_eq!(
                delivery
                    .recover_next_with_group_operations(
                        &mut ratchet,
                        &mut f.log,
                        &mut sent,
                        &mut received,
                        None,
                        None,
                        Some(&mut complete)
                    )
                    .unwrap(),
                Some(event_id)
            );
            assert!(delivery.pending_transactions().is_empty());
            assert_eq!(
                f.log
                    .events(&f.operation.group)
                    .unwrap()
                    .iter()
                    .filter(|event| event.id == event_id)
                    .count(),
                1
            );
            assert!(sent.entries(&f.operation.group).is_empty());
            drop(terminal);
            let (_, rows): (
                crate::storage::record_log::EncryptedRecordLog<Vec<EventId>>,
                _,
            ) = crate::storage::record_log::EncryptedRecordLog::open(
                &terminal_path,
                "pw",
                b"MTACTN",
            )
            .unwrap();
            assert_eq!(rows, vec![vec![event_id]]);
            let recalls = crate::node::recall::RecallLog::open(&recall_path, "pw").unwrap();
            assert_eq!(
                recalls.targets(&f.operation.group),
                expected.map(|mark| vec![mark.target]).unwrap_or_default()
            );
        }
    }
    #[test]
    fn action_completion_failure_retains_same_wal_event_for_restart_retry() {
        let mut f = fixture(true);
        let mut delivery = store(&f);
        delivery.begin(transaction(&f)).unwrap();
        let (mut ratchet, mut sent, mut received) = sides(&f);
        let event_id = f.event.id;
        let operation_id = f.operation.id;
        let mark = f.recall.clone().unwrap();
        let recall_path = f._dir.path().join("recall");
        let mut recalls = crate::node::recall::RecallLog::open(&recall_path, "pw").unwrap();
        let terminal_path = f._dir.path().join("terminal");
        let (mut terminal, _): (
            crate::storage::record_log::EncryptedRecordLog<Vec<EventId>>,
            _,
        ) = crate::storage::record_log::EncryptedRecordLog::open(&terminal_path, "pw", b"MTACTN")
            .unwrap();
        terminal.fail_appends_for_test(true).unwrap();
        let mut complete = |operation: &OperationReference, events: &[Event]| {
            assert_eq!(operation.id, operation_id);
            assert_eq!(events[0].id, event_id);
            recalls.record(mark.conversation, mark.target, mark.wall_clock)?;
            terminal.append_durable(&vec![event_id])
        };
        assert!(
            delivery
                .recover_next_with_group_operations(
                    &mut ratchet,
                    &mut f.log,
                    &mut sent,
                    &mut received,
                    None,
                    None,
                    Some(&mut complete)
                )
                .is_err()
        );
        assert_eq!(delivery.pending_transactions()[0].id(), event_id);
        assert!(f.log.get(&event_id).unwrap().is_some());
        drop((delivery, terminal, recalls, ratchet, sent, received));
        let mut delivery = store(&f);
        f.log = PersistentEventLog::open(&f._dir.path().join("events"), "pw").unwrap();
        let (mut ratchet, mut sent, mut received) = sides(&f);
        let mut recalls = crate::node::recall::RecallLog::open(&recall_path, "pw").unwrap();
        let (mut terminal, rows): (
            crate::storage::record_log::EncryptedRecordLog<Vec<EventId>>,
            _,
        ) = crate::storage::record_log::EncryptedRecordLog::open(&terminal_path, "pw", b"MTACTN")
            .unwrap();
        assert!(rows.is_empty());
        let mut complete = |operation: &OperationReference, events: &[Event]| {
            assert_eq!(operation.id, operation_id);
            assert_eq!(events[0].id, event_id);
            recalls.record(mark.conversation, mark.target, mark.wall_clock)?;
            terminal.append_durable(&vec![event_id])
        };
        assert_eq!(
            delivery
                .recover_next_with_group_operations(
                    &mut ratchet,
                    &mut f.log,
                    &mut sent,
                    &mut received,
                    None,
                    None,
                    Some(&mut complete)
                )
                .unwrap(),
            Some(event_id)
        );
        assert!(delivery.pending_transactions().is_empty());
        assert_eq!(
            f.log
                .events(&f.operation.group)
                .unwrap()
                .iter()
                .filter(|event| event.kind == crate::eventlog::EventKind::Delete)
                .count(),
            1
        );
        assert_eq!(
            f.log
                .next_sequence(&f.operation.group, &f.event.author)
                .unwrap(),
            4
        );
        assert_eq!(recalls.snapshot_marks(), vec![mark]);
    }
    #[test]
    fn bound_action_requires_completion_adapter_before_public_append() {
        let mut f = fixture(true);
        let mut delivery = store(&f);
        delivery.begin(transaction(&f)).unwrap();
        let (mut ratchet, mut sent, mut received) = sides(&f);
        assert!(
            delivery
                .recover_next_with_group_operations(
                    &mut ratchet,
                    &mut f.log,
                    &mut sent,
                    &mut received,
                    None,
                    None,
                    None
                )
                .is_err()
        );
        assert!(f.log.get(&f.event.id).unwrap().is_none());
        assert_eq!(delivery.pending_transactions()[0].id(), f.event.id);
    }
    #[test]
    fn recall_outside_original_causal_cut_is_rejected_before_public_append() {
        let mut f = fixture(true);
        let original = &f.witness.target.as_ref().unwrap().original;
        f.witness.parents = original.parents.clone();
        f.event = Event::new(
            &f.own,
            f.operation.group,
            2,
            f.witness.parents.clone(),
            2,
            f.witness.admitted_at_ms,
            f.event.kind,
            f.event.ciphertext.clone(),
        );
        f.request.message_id = f.event.id;
        let mut delivery = store(&f);
        let transaction = transaction(&f);
        // Admission may reject the saved view eagerly; recovery must always reject
        // it before publishing any effect if it was accepted into the local WAL.
        if delivery.begin(transaction).is_ok() {
            let (mut ratchet, mut sent, mut received) = sides(&f);
            let mut complete = |_: &OperationReference, _: &[Event]| -> Result<(), LogError> {
                panic!("invalid target reached completion")
            };
            assert!(
                delivery
                    .recover_next_with_group_operations(
                        &mut ratchet,
                        &mut f.log,
                        &mut sent,
                        &mut received,
                        None,
                        None,
                        Some(&mut complete)
                    )
                    .is_err()
            );
            assert_eq!(delivery.pending_transactions()[0].id(), f.event.id);
        }
        assert!(f.log.get(&f.event.id).unwrap().is_none());
    }
}

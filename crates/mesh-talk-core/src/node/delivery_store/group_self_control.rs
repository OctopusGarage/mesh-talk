//! Original-intent and saved causal validation for modern account self controls.
use super::{OperationReference, invalid};
use crate::{
    eventlog::{Event, EventId, LogError, persist::PersistentEventLog},
    identity::device::PublicIdentity,
    node::{
        AccountGroupExecution, Node,
        channel::ChannelBook,
        channels::account_self_control::{
            AccountSelfControlInput, AccountSelfControlWitness, expected_self_control,
        },
    },
};

pub(super) fn validate_bound(
    operation: &OperationReference,
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    validate_common(Some(operation), input, witness, event)
}
pub(super) fn validate_direct(
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    validate_common(None, input, witness, event)
}
fn validate_common(
    operation: Option<&OperationReference>,
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    let account = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("self-control source has no account"))?;
    if let Some(operation) = operation
        && (!operation.valid(&account.account_ed25519_pub)
            || operation.executor.device != witness.own.public()
            || matches!(&operation.executor.execution, AccountGroupExecution::ViaDevice(device) if *device != operation.executor.device)
            || operation.executor.role != input.required_role()
            || operation.executor.authority.authority.is_none()
            || operation.kind != input.operation().kind()
            || crate::node::account_group::operations::journal::operation_digest(
                operation.group,
                &input.operation(),
            ) != operation.digest
            || event.conversation_id != operation.group)
    {
        return Err(invalid("invalid original account self-control binding"));
    }
    if !witness.own.valid_presence()
        || witness.own.post_office
        || witness.grant.account_pub != account.account_ed25519_pub
        || event.author.ed25519_pub() != &witness.own.ed25519_pub
        || event.kind != input.event_kind()
        || event.ciphertext != input.event_payload(witness.grant)
        || event.parents != witness.parents
        || event.parents.is_empty()
        || event.seq == 0
        || event.lamport == 0
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
        || !crate::node::session::event_fits_frame(event)
    {
        return Err(invalid("invalid account self-control envelope"));
    }
    Ok(())
}

/// Recover only the original source/grant incarnation and original recipient set
/// from the saved causal cut. Current role/caches cannot veto accepted WAL replay.
pub(super) fn validate_parent_view(
    operation: &OperationReference,
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    validate_parent_common(Some(operation), input, witness, event, log)
}
pub(super) fn validate_direct_parent_view(
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    validate_parent_common(None, input, witness, event, log)
}
fn validate_parent_common(
    operation: Option<&OperationReference>,
    input: AccountSelfControlInput,
    witness: &AccountSelfControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    use std::collections::{HashMap, HashSet};
    validate_common(operation, input, witness, event)?;
    let group = event.conversation_id;
    let mut ancestors: HashMap<EventId, &Event> = HashMap::new();
    let mut pending = witness.parents.clone();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let parent = log
            .get(&id)
            .ok_or_else(|| invalid("self-control causal parent missing"))?;
        if parent.conversation_id != group || parent.id == event.id {
            return Err(invalid("invalid self-control causal parent"));
        }
        pending.extend(parent.parents.iter().copied());
        ancestors.insert(id, parent);
    }
    let events: Vec<_> = ancestors.values().copied().collect();
    let projection = ChannelBook::project_membership(group, &events);
    if let Some(operation) = operation {
        let mut source = Node::public_account_group_source(
            group,
            &projection,
            &witness.own.public(),
            &witness.own,
        )
        .ok_or_else(|| invalid("self-control source not authorized in saved view"))?;
        source.execution = operation.executor.execution.clone();
        if source != operation.executor {
            return Err(invalid("self-control pinned source lifetime changed"));
        }
    }
    let (_, recipients) = expected_self_control(group, &projection, input, witness)
        .map_err(|_| invalid("invalid saved account self-control decision"))?;
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
    let parents: HashSet<_> = events
        .iter()
        .flat_map(|parent| parent.parents.iter().copied())
        .collect();
    let mut heads: Vec<_> = ancestors
        .keys()
        .copied()
        .filter(|id| !parents.contains(id))
        .collect();
    heads.sort();
    if heads != witness.parents
        || max_clock.checked_add(1) != Some(event.lamport)
        || max_seq.checked_add(1) != Some(event.seq)
    {
        return Err(invalid("invalid self-control original causal allocation"));
    }
    Ok(recipients)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::Announce,
        eventlog::EventId,
        identity::device::DeviceIdentity,
        node::{AccountGroupOperationId, Node, channels::account_self_control::tests::fixture},
    };
    fn bound_fixture(
        pending: bool,
    ) -> (
        OperationReference,
        AccountSelfControlInput,
        AccountSelfControlWitness,
        Event,
        Vec<Event>,
    ) {
        bound_fixture_for(if pending {
            AccountSelfControlInput::Accept
        } else {
            AccountSelfControlInput::Leave
        })
    }
    fn bound_fixture_for(
        input: AccountSelfControlInput,
    ) -> (
        OperationReference,
        AccountSelfControlInput,
        AccountSelfControlWitness,
        Event,
        Vec<Event>,
    ) {
        let pending = input != AccountSelfControlInput::Leave;
        let f = fixture(pending);
        let witness = f.witness();
        let operation = input.operation();
        let origin = DeviceIdentity::generate();
        let source = Node::public_account_group_source(
            f.group,
            &f.projection(),
            &f.member.public(),
            &f.proof,
        )
        .unwrap();
        let mut reference = OperationReference {
            id: AccountGroupOperationId::for_origin(&origin.public(), [116; 16]),
            group: f.group,
            kind: operation.kind(),
            digest: crate::node::account_group::operations::journal::operation_digest(
                f.group, &operation,
            ),
            origin: Announce::new_with_account(&origin, &f.account, "origin", 1),
            executor: source,
            signature: vec![],
        };
        reference.signature = origin.sign(&reference.message()).to_vec();
        let event = Event::new(
            &f.member,
            f.group,
            1,
            witness.parents.clone(),
            3,
            3,
            input.event_kind(),
            input.event_payload(witness.grant),
        );
        (reference, input, witness, event, f.events)
    }
    #[test]
    fn bound_self_control_rejects_another_original_digest() {
        let (mut reference, input, witness, event, _) = bound_fixture(true);
        reference.digest = [9; 32];
        assert!(validate_bound(&reference, input, &witness, &event).is_err());
    }
    #[test]
    fn bound_self_control_rejects_another_effect_kind_or_parent_cut() {
        let (reference, input, mut witness, event, _) = bound_fixture(false);
        witness.parents = vec![EventId::new([9; 32])];
        assert!(validate_bound(&reference, input, &witness, &event).is_err());
        let (reference, _, witness, event, _) = bound_fixture(true);
        assert!(
            validate_bound(&reference, AccountSelfControlInput::Leave, &witness, &event).is_err()
        );
    }
    #[test]
    fn self_control_rejects_missing_saved_causal_parents_before_any_effect() {
        let (reference, input, witness, event, _) = bound_fixture(true);
        let dir = tempfile::tempdir().unwrap();
        let log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        assert!(
            validate_parent_view(&reference, input, &witness, &event, &log).is_err(),
            "an original intent cannot authorize a self-control effect without its real saved grant ancestry"
        );
        assert!(log.get(&event.id).is_none());
    }

    #[test]
    fn bound_modern_self_control_accepts_the_original_signed_envelope() {
        for pending in [false, true] {
            let (reference, input, witness, event, _) = bound_fixture(pending);
            validate_bound(&reference, input, &witness, &event).unwrap();
        }
    }
    #[test]
    fn self_control_saved_cut_replays_after_its_public_role_effect_is_installed() {
        for pending in [false, true] {
            let (reference, input, witness, event, parents) = bound_fixture(pending);
            let dir = tempfile::tempdir().unwrap();
            let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
            for parent in parents {
                log.append_durable(parent).unwrap();
            }
            assert_eq!(
                validate_parent_view(&reference, input, &witness, &event, &log).unwrap(),
                witness.recipients
            );
            log.append_durable(event.clone()).unwrap();
            assert_eq!(
                validate_parent_view(&reference, input, &witness, &event, &log).unwrap(),
                witness.recipients,
                "installed acceptance/departure must not veto recovery of the original exact WAL effect"
            );
            let mut changed = witness.clone();
            changed.grant.incarnation = [8; 16];
            assert!(validate_parent_view(&reference, input, &changed, &event, &log).is_err());
        }
    }
    #[test]
    fn direct_self_control_uses_the_same_original_saved_decision() {
        for pending in [false, true] {
            let (_, input, witness, event, parents) = bound_fixture(pending);
            let dir = tempfile::tempdir().unwrap();
            let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
            for parent in parents {
                log.append_durable(parent).unwrap();
            }
            validate_direct(input, &witness, &event).unwrap();
            assert_eq!(
                validate_direct_parent_view(input, &witness, &event, &log).unwrap(),
                witness.recipients
            );
            log.append_durable(event.clone()).unwrap();
            validate_direct_parent_view(input, &witness, &event, &log).unwrap();
            let mut stale = witness.clone();
            stale.grant.incarnation = [7; 16];
            assert!(validate_direct_parent_view(input, &stale, &event, &log).is_err());
        }
    }
    #[test]
    fn direct_self_control_rejects_changed_envelope_and_missing_parent_view() {
        let (_, input, mut witness, mut event, _) = bound_fixture(true);
        let dir = tempfile::tempdir().unwrap();
        let log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        assert!(validate_direct_parent_view(input, &witness, &event, &log).is_err());
        witness.parents = vec![EventId::new([7; 32])];
        assert!(validate_direct(input, &witness, &event).is_err());
        let (_, input, witness, clean, _) = bound_fixture(false);
        event = clean;
        event.ciphertext.push(1);
        assert!(validate_direct(input, &witness, &event).is_err());
    }
    #[test]
    fn bound_self_control_wal_reopens_and_retries_exact_event_after_completion_failure() {
        use super::super::{DeliveryStore, DeliveryTransaction, RequestRecord};
        use crate::node::{
            MessageDestination, RatchetSessions, dm_ratchet::DmRatchet, received_log::ReceivedLog,
            sentlog::SentLog,
        };
        use crate::storage::record_log::EncryptedRecordLog;
        for selected in [
            AccountSelfControlInput::Accept,
            AccountSelfControlInput::Leave,
            AccountSelfControlInput::Decline,
        ] {
            let (reference, input, witness, event, parents) = bound_fixture_for(selected);
            let dir = tempfile::tempdir().unwrap();
            let log_path = dir.path().join("events");
            let mut log = PersistentEventLog::open(&log_path, "pw").unwrap();
            for parent in parents {
                log.append_durable(parent).unwrap();
            }
            let open = || {
                let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
                store
                    .bind_profile(
                        &log_path,
                        "pw",
                        &witness.own.public(),
                        &witness.own.account_id().unwrap(),
                    )
                    .unwrap();
                store
            };
            let mut store = open();
            let transaction = DeliveryTransaction::GroupRequested {
                operation: reference.clone(),
                transaction: Box::new(DeliveryTransaction::Requested {
                    request: RequestRecord {
                        request_id: *reference.id.as_bytes(),
                        digest: reference.digest,
                        destination: MessageDestination::Channel(reference.group),
                        message_id: event.id,
                        accepted_at_ms: 3,
                    },
                    transaction: Box::new(DeliveryTransaction::AccountSelfControl {
                        input,
                        witness: witness.clone(),
                        events: vec![event.clone()],
                    }),
                }),
            };
            store
                .preflight_group_self_control_transaction(&transaction, &log)
                .unwrap();
            store.begin(transaction).unwrap();
            let sides = || {
                (
                    DmRatchet::new(
                        RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap(),
                    ),
                    SentLog::open(&dir.path().join("sent"), "pw").unwrap(),
                    ReceivedLog::open(&dir.path().join("received"), "pw").unwrap(),
                )
            };
            let (mut ratchet, mut sent, mut received) = sides();
            let terminal_path = dir.path().join("terminal");
            let (mut terminal, _): (EncryptedRecordLog<Vec<EventId>>, _) =
                EncryptedRecordLog::open(&terminal_path, "pw", b"MTSLFC").unwrap();
            terminal.fail_appends_for_test(true).unwrap();
            let mut complete = |operation: &OperationReference, events: &[Event]| {
                assert_eq!(operation.id, reference.id);
                assert_eq!(
                    events.iter().map(|event| event.id).collect::<Vec<_>>(),
                    vec![event.id]
                );
                terminal.append_durable(&vec![event.id])
            };
            assert!(
                store
                    .recover_next_with_group_operations(
                        &mut ratchet,
                        &mut log,
                        &mut sent,
                        &mut received,
                        None,
                        None,
                        Some(&mut complete)
                    )
                    .is_err()
            );
            assert_eq!(log.get(&event.id), Some(&event));
            assert_eq!(store.pending_transactions()[0].id(), event.id);
            drop((store, log, ratchet, sent, received, terminal));
            let mut store = open();
            let mut log = PersistentEventLog::open(&log_path, "pw").unwrap();
            let (mut ratchet, mut sent, mut received) = sides();
            let (mut terminal, rows): (EncryptedRecordLog<Vec<EventId>>, _) =
                EncryptedRecordLog::open(&terminal_path, "pw", b"MTSLFC").unwrap();
            assert!(rows.is_empty());
            let mut complete = |operation: &OperationReference, events: &[Event]| {
                assert_eq!(operation.id, reference.id);
                terminal.append_durable(&events.iter().map(|event| event.id).collect())
            };
            assert_eq!(
                store
                    .recover_next_with_group_operations(
                        &mut ratchet,
                        &mut log,
                        &mut sent,
                        &mut received,
                        None,
                        None,
                        Some(&mut complete)
                    )
                    .unwrap(),
                Some(event.id)
            );
            assert!(store.pending_transactions().is_empty());
            assert_eq!(
                log.events(&reference.group)
                    .into_iter()
                    .filter(|candidate| candidate.kind == input.event_kind())
                    .map(|candidate| candidate.id)
                    .collect::<Vec<_>>(),
                vec![event.id]
            );
            assert_eq!(
                log.next_sequence(&reference.group, &event.author).unwrap(),
                event.seq + 1
            );
            drop(terminal);
            let (_, rows): (EncryptedRecordLog<Vec<EventId>>, _) =
                EncryptedRecordLog::open(&terminal_path, "pw", b"MTSLFC").unwrap();
            assert_eq!(rows, vec![vec![event.id]]);
        }
    }
    #[test]
    fn self_control_wal_rejects_same_signing_profile_with_another_transport_key() {
        use super::super::{DeliveryStore, DeliveryTransaction, RequestRecord};
        use crate::node::MessageDestination;
        for bound in [false, true] {
            let (reference, input, witness, event, parents) = bound_fixture(false);
            let dir = tempfile::tempdir().unwrap();
            let log_path = dir.path().join("events");
            let mut log = PersistentEventLog::open(&log_path, "pw").unwrap();
            for parent in parents {
                log.append_durable(parent).unwrap();
            }
            let mut another_profile = witness.own.public();
            another_profile.x25519_pub[0] ^= 1;
            let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
            store
                .bind_profile(
                    &log_path,
                    "pw",
                    &another_profile,
                    &witness.own.account_id().unwrap(),
                )
                .unwrap();
            let effect = DeliveryTransaction::AccountSelfControl {
                input,
                witness,
                events: vec![event.clone()],
            };
            let transaction = if bound {
                DeliveryTransaction::GroupRequested {
                    operation: reference.clone(),
                    transaction: Box::new(DeliveryTransaction::Requested {
                        request: RequestRecord {
                            request_id: *reference.id.as_bytes(),
                            digest: reference.digest,
                            destination: MessageDestination::Channel(reference.group),
                            message_id: event.id,
                            accepted_at_ms: 3,
                        },
                        transaction: Box::new(effect),
                    }),
                }
            } else {
                effect
            };
            assert!(
                store
                    .preflight_group_self_control_transaction(&transaction, &log)
                    .is_err()
            );
            assert!(store.begin(transaction).is_err());
            assert!(store.pending_transactions().is_empty());
            assert!(log.get(&event.id).is_none());
        }
    }
}

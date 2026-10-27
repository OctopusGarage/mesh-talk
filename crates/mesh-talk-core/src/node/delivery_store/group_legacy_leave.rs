//! Original intent and saved physical membership for legacy self-signed leaves.
use super::{OperationReference, invalid};
use crate::{
    eventlog::{Event, EventId, EventKind, LogError, persist::PersistentEventLog},
    identity::device::PublicIdentity,
    node::{
        AccountGroupExecution, AccountGroupOperation, AccountGroupOperationKind, AccountGroupRole,
        Node,
        channel::ChannelBook,
        channels::account_self_control::{LegacyMemberLeaveWitness, expected_legacy_member_leave},
    },
};

pub(super) fn validate_common(
    operation: Option<&OperationReference>,
    witness: &LegacyMemberLeaveWitness,
    event: &Event,
) -> Result<(), LogError> {
    let account = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("legacy departure source has no account"))?;
    if let Some(operation) = operation
        && (!operation.valid(&account.account_ed25519_pub)
            || operation.executor.device != witness.own.public()
            || matches!(&operation.executor.execution, AccountGroupExecution::ViaDevice(device) if *device != operation.executor.device)
            || !matches!(
                operation.executor.execution,
                AccountGroupExecution::Local | AccountGroupExecution::ViaDevice(_)
            )
            || operation.executor.role != AccountGroupRole::Member
            || operation.executor.authority.authority.is_some()
            || operation.executor.lifetime != witness.lifetime
            || operation.kind != AccountGroupOperationKind::LeaveAccount
            || crate::node::account_group::operations::journal::operation_digest(
                operation.group,
                &AccountGroupOperation::LeaveAccount,
            ) != operation.digest
            || operation.group != event.conversation_id)
    {
        return Err(invalid("invalid original legacy departure binding"));
    }
    if !witness.own.valid_presence()
        || witness.own.post_office
        || witness.lifetime.member != witness.own.public()
        || event.author.ed25519_pub() != &witness.own.ed25519_pub
        || event.kind != EventKind::ChannelLeave
        || !event.ciphertext.is_empty()
        || event.parents != witness.parents
        || event.parents.is_empty()
        || event.seq == 0
        || event.lamport == 0
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
        || !crate::node::session::event_fits_frame(event)
    {
        return Err(invalid("invalid legacy departure envelope"));
    }
    Ok(())
}

pub(super) fn validate_parent_view(
    operation: Option<&OperationReference>,
    witness: &LegacyMemberLeaveWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    use std::collections::{HashMap, HashSet};
    validate_common(operation, witness, event)?;
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
        // Other physical departures advance the legacy epoch without changing
        // this exact source's membership incarnation. Never relax identity,
        // lifetime, role, format, or backward epoch movement.
        if source.authority.authority.is_some()
            || operation.executor.authority.authority.is_some()
            || source.authority.epoch < operation.executor.authority.epoch
        {
            return Err(invalid("legacy departure source authority changed"));
        }
        source.authority.epoch = operation.executor.authority.epoch;
        if source != operation.executor {
            return Err(invalid("self-control pinned source lifetime changed"));
        }
    }
    let recipients = expected_legacy_member_leave(group, &projection, witness)
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
        channel::{ChannelMeta, new_channel_id},
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
        node::AccountGroupOperationId,
    };

    fn fixture(
        owner_leaves: bool,
    ) -> (
        OperationReference,
        LegacyMemberLeaveWitness,
        Event,
        Vec<Event>,
    ) {
        fixture_with_identities(
            owner_leaves,
            &DeviceIdentity::generate(),
            &DeviceIdentity::generate(),
            &DeviceIdentity::generate(),
        )
    }

    fn fixture_with_identities(
        owner_leaves: bool,
        member: &DeviceIdentity,
        owner: &DeviceIdentity,
        origin: &DeviceIdentity,
    ) -> (
        OperationReference,
        LegacyMemberLeaveWitness,
        Event,
        Vec<Event>,
    ) {
        let account = Account::generate();
        let group = new_channel_id();
        let genesis = Event::new(
            owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "legacy departure".into(),
                members: vec![owner.public(), member.public(), origin.public()],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        let actor = if owner_leaves { owner } else { member };
        let own = Announce::new_with_account(actor, &account, "legacy physical source", 1);
        let projection = ChannelBook::project_membership(group, &[&genesis]);
        let executor =
            Node::public_account_group_source(group, &projection, &actor.public(), &own).unwrap();
        let witness = LegacyMemberLeaveWitness {
            own,
            lifetime: executor.lifetime.clone(),
            parents: vec![genesis.id],
            recipients: projection.state.as_ref().unwrap().members().to_vec(),
        };
        let mut reference = OperationReference {
            id: AccountGroupOperationId::for_origin(&origin.public(), [115; 16]),
            group,
            kind: AccountGroupOperationKind::LeaveAccount,
            digest: crate::node::account_group::operations::journal::operation_digest(
                group,
                &AccountGroupOperation::LeaveAccount,
            ),
            origin: Announce::new_with_account(origin, &account, "original account requester", 1),
            executor,
            signature: vec![],
        };
        reference.signature = origin.sign(&reference.message()).to_vec();
        let event = Event::new(
            actor,
            group,
            if owner_leaves { 2 } else { 1 },
            witness.parents.clone(),
            2,
            2,
            EventKind::ChannelLeave,
            vec![],
        );
        (reference, witness, event, vec![genesis])
    }

    #[test]
    fn legacy_departure_saved_cut_authorizes_only_exact_nonowner_physical_lifetime() {
        let (reference, witness, event, parents) = fixture(false);
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        assert!(validate_parent_view(Some(&reference), &witness, &event, &log).is_err());
        for parent in parents {
            log.append_durable(parent).unwrap();
        }
        assert_eq!(
            validate_parent_view(Some(&reference), &witness, &event, &log).unwrap(),
            witness.recipients
        );
        log.append_durable(event.clone()).unwrap();
        assert!(
            validate_parent_view(Some(&reference), &witness, &event, &log).is_ok(),
            "accepted WAL replay uses the original saved Member cut after departure"
        );
        let mut wrong = witness.clone();
        wrong.lifetime.membership = EventId::new([7; 32]);
        assert!(validate_common(Some(&reference), &wrong, &event).is_err());
        assert!(validate_parent_view(None, &wrong, &event, &log).is_err());
        let mut wrong_reference = reference.clone();
        wrong_reference.executor.device = DeviceIdentity::generate().public();
        assert!(validate_common(Some(&wrong_reference), &witness, &event).is_err());
        let mut wrong_event = event.clone();
        wrong_event.ciphertext = vec![1];
        assert!(validate_common(Some(&reference), &witness, &wrong_event).is_err());
        let mut wrong_account = witness.clone();
        let stranger = DeviceIdentity::generate();
        wrong_account.own =
            Announce::new_with_account(&stranger, &Account::generate(), "other source", 1);
        assert!(validate_common(Some(&reference), &wrong_account, &event).is_err());
        let (owner_reference, owner_witness, owner_event, owner_parents) = fixture(true);
        assert!(validate_common(Some(&owner_reference), &owner_witness, &owner_event).is_err());
        let projection = ChannelBook::project_membership(
            owner_event.conversation_id,
            &owner_parents.iter().collect::<Vec<_>>(),
        );
        assert!(
            expected_legacy_member_leave(owner_event.conversation_id, &projection, &owner_witness)
                .is_err()
        );
    }

    #[test]
    fn legacy_departure_rejects_recipient_mismatch_without_epoch_overflow() {
        let (reference, witness, event, parents) = fixture(false);
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        for parent in parents {
            assert!(parent.verify_integrity() && parent.verify_signature());
            log.append_durable(parent).unwrap();
        }
        let projection = ChannelBook::project_membership(
            event.conversation_id,
            &log.events(&event.conversation_id),
        );
        assert!(
            projection
                .state
                .as_ref()
                .unwrap()
                .epoch()
                .checked_add(1)
                .is_some()
        );
        assert!(validate_common(Some(&reference), &witness, &event).is_ok());
        assert_eq!(
            validate_parent_view(Some(&reference), &witness, &event, &log).unwrap(),
            witness.recipients,
        );
        let mut missing_recipient = witness.clone();
        assert!(missing_recipient.recipients.pop().is_some());
        // The original signed request, departure and causal parents still match;
        // the saved audience alone differs from canonical physical membership.
        assert!(validate_common(Some(&reference), &missing_recipient, &event).is_ok());
        assert!(
            expected_legacy_member_leave(event.conversation_id, &projection, &missing_recipient)
                .is_err()
        );
        assert!(validate_parent_view(Some(&reference), &missing_recipient, &event, &log).is_err());
    }

    #[test]
    fn legacy_departure_allows_other_departure_epoch_drift_but_not_reinvitation() {
        let member = DeviceIdentity::generate();
        let owner = DeviceIdentity::generate();
        let other = DeviceIdentity::generate();
        let (reference, mut witness, _, parents) =
            fixture_with_identities(false, &member, &owner, &other);
        let group = reference.group;
        let dir = tempfile::tempdir().unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        for parent in parents {
            log.append_durable(parent).unwrap();
        }
        let other_leave = Event::new(
            &other,
            group,
            1,
            witness.parents.clone(),
            2,
            2,
            EventKind::ChannelLeave,
            vec![],
        );
        log.append_durable(other_leave.clone()).unwrap();
        let projection = ChannelBook::project_membership(group, &log.events(&group));
        let current =
            Node::public_account_group_source(group, &projection, &member.public(), &witness.own)
                .unwrap();
        assert_eq!(current.lifetime, reference.executor.lifetime);
        assert!(current.authority.epoch > reference.executor.authority.epoch);
        witness.parents = vec![other_leave.id];
        witness.recipients = projection.state.as_ref().unwrap().members().to_vec();
        let leave = Event::new(
            &member,
            group,
            1,
            witness.parents.clone(),
            3,
            3,
            EventKind::ChannelLeave,
            vec![],
        );
        assert!(validate_parent_view(Some(&reference), &witness, &leave, &log).is_ok());
        log.append_durable(leave.clone()).unwrap();
        let reinvite = Event::new(
            &owner,
            group,
            2,
            vec![leave.id],
            4,
            4,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "new legacy lifetime".into(),
                members: vec![owner.public(), member.public()],
                epoch: 3,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        log.append_durable(reinvite.clone()).unwrap();
        let rejoined = ChannelBook::project_membership(group, &log.events(&group));
        let new_source =
            Node::public_account_group_source(group, &rejoined, &member.public(), &witness.own)
                .expect("owner snapshot causally re-invites the physical member");
        assert_ne!(new_source.lifetime, reference.executor.lifetime);
        witness.parents = vec![reinvite.id];
        witness.recipients = rejoined.state.as_ref().unwrap().members().to_vec();
        let fresh_leave = Event::new(
            &member,
            group,
            2,
            witness.parents.clone(),
            5,
            5,
            EventKind::ChannelLeave,
            vec![],
        );
        assert!(
            validate_parent_view(Some(&reference), &witness, &fresh_leave, &log).is_err(),
            "epoch drift cannot authorize a different membership incarnation"
        );
    }

    #[test]
    fn legacy_departure_wal_replays_exact_effect_after_completion_failure() {
        use super::super::{DeliveryStore, DeliveryTransaction, RequestRecord};
        use crate::node::{
            MessageDestination, RatchetSessions, dm_ratchet::DmRatchet, received_log::ReceivedLog,
            sentlog::SentLog,
        };
        use crate::storage::record_log::EncryptedRecordLog;
        let (reference, witness, event, parents) = fixture(false);
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
                transaction: Box::new(DeliveryTransaction::LegacyMemberLeave {
                    witness: witness.clone(),
                    events: vec![event.clone()],
                }),
            }),
        };
        store
            .preflight_legacy_member_leave_transaction(&transaction, &log)
            .unwrap();
        store.begin(transaction).unwrap();
        let sides = || {
            (
                DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap()),
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
                .filter(|candidate| candidate.kind == EventKind::ChannelLeave)
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

//! Original-intent and saved causal binding of the existing modern invitation batch.
use super::{DeliveryTransaction, OperationReference, RequestRecord, invalid};
use crate::{
    eventlog::{Event, EventId, EventKind, LogError, persist::PersistentEventLog},
    identity::device::PublicIdentity,
    node::{
        AccountGroupExecution, AccountGroupRole, MessageDestination, Node,
        channel::ChannelBook,
        channel_account_authority::AccountAuthorityControl,
        channels::account_invitation::{InvitationInput, InvitationWitness, expected_invitation},
    },
};
fn batch(body: &DeliveryTransaction) -> Result<&[Event], LogError> {
    let DeliveryTransaction::ChannelOutgoing { events, sent, .. } = body else {
        return Err(invalid("invalid invitation batch kind"));
    };
    if !crate::node::join_history::valid_invitation_transaction(events)
        || !(2..=3).contains(&events.len())
        || events[1].kind != EventKind::ChannelAccountAuthority
        || !sent.plaintext.is_empty()
        || sent.conversation != events[0].conversation_id
        || sent.seq != events.last().expect("checked invitation batch").seq
        || sent.wall_clock != events[0].wall_clock
    {
        return Err(invalid("invalid invitation batch"));
    }
    Ok(events)
}
pub(super) fn validate_bound(
    operation: &OperationReference,
    request: &RequestRecord,
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
) -> Result<(), LogError> {
    validate_common(Some((operation, request)), input, witness, body)
}
pub(super) fn validate_direct(
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
) -> Result<(), LogError> {
    validate_common(None, input, witness, body)
}
fn validate_common(
    bound: Option<(&OperationReference, &RequestRecord)>,
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
) -> Result<(), LogError> {
    let events = batch(body)?;
    let invitation = &events[0];
    let own = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("invitation source has no account"))?;
    if !input.canonical()
        || !witness.own.valid_presence()
        || witness.own.post_office
        || !witness.target.valid_presence()
        || witness.target.post_office
        || witness.target.account_cert.is_none()
        || witness.target.account_id().as_deref() != Some(input.target_account_id.as_str())
        || invitation.author.ed25519_pub() != &witness.own.ed25519_pub
        || invitation.parents != witness.parents
        || invitation.parents.is_empty()
        || invitation.seq == 0
        || invitation.lamport == 0
    {
        return Err(invalid("invalid invitation source or target envelope"));
    }
    for (offset, event) in events.iter().enumerate() {
        if event.conversation_id != invitation.conversation_id
            || event.author != invitation.author
            || invitation.seq.checked_add(offset as u64) != Some(event.seq)
            || invitation.lamport.checked_add(offset as u64) != Some(event.lamport)
            || event.wall_clock != invitation.wall_clock
            || (offset > 0 && event.parents != [events[offset - 1].id])
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
            || !crate::node::session::event_fits_frame(event)
        {
            return Err(invalid("invalid signed invitation event chain"));
        }
    }
    let control = AccountAuthorityControl::decode(&events[1].ciphertext)
        .ok_or_else(|| invalid("invalid invitation authority"))?;
    if control.issuer != witness.own
        || control.owner_account != own.account_ed25519_pub
        || !control.verify_account_signature(invitation.conversation_id, &own.account_ed25519_pub)
        || !control.grants.iter().any(|grant| {
            grant.original == witness.target
                && grant.incarnation == witness.incarnation
                && grant.pending
                && grant.membership_event == invitation.id
                && grant.acceptance_event.is_none()
        })
    {
        return Err(invalid("invalid invitation grant binding"));
    }
    if let Some((operation, request)) = bound
        && (!operation.valid(&own.account_ed25519_pub)
            || operation.kind != input.operation().kind()
            || operation.executor.role != AccountGroupRole::Owner
            || operation.executor.device != witness.own.public()
            || operation.executor.authority.authority.is_none()
            || matches!(&operation.executor.execution,AccountGroupExecution::ViaDevice(device) if *device!=operation.executor.device)
            || operation.group != invitation.conversation_id
            || operation.digest
                != crate::node::account_group::operations::journal::operation_digest(
                    operation.group,
                    &input.operation(),
                )
            || request.request_id != *operation.id.as_bytes()
            || request.digest != operation.digest
            || request.destination != MessageDestination::Channel(operation.group)
            || request.message_id != invitation.id)
    {
        return Err(invalid("invalid original invitation operation binding"));
    }
    Ok(())
}
pub(super) fn validate_parent_view(
    operation: &OperationReference,
    request: &RequestRecord,
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    validate_parent_common(Some((operation, request)), input, witness, body, log)
}
pub(super) fn validate_direct_parent_view(
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    validate_parent_common(None, input, witness, body, log)
}
fn validate_parent_common(
    bound: Option<(&OperationReference, &RequestRecord)>,
    input: &InvitationInput,
    witness: &InvitationWitness,
    body: &DeliveryTransaction,
    log: &PersistentEventLog,
) -> Result<Vec<PublicIdentity>, LogError> {
    use std::collections::{HashMap, HashSet};
    validate_common(bound, input, witness, body)?;
    let batch = batch(body)?;
    let first = &batch[0];
    let group = first.conversation_id;
    let mut ancestors: HashMap<EventId, &Event> = HashMap::new();
    let mut pending = witness.parents.clone();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let event = log
            .get(&id)
            .ok_or_else(|| invalid("invitation causal parent missing"))?;
        if event.conversation_id != group || batch.iter().any(|effect| effect.id == id) {
            return Err(invalid("invalid invitation causal parent"));
        }
        pending.extend(event.parents.iter().copied());
        ancestors.insert(id, event);
    }
    let events: Vec<_> = ancestors.values().copied().collect();
    let projection = ChannelBook::project_membership(group, &events);
    if let Some((operation, _)) = bound {
        let mut source = Node::public_account_group_source(
            group,
            &projection,
            &witness.own.public(),
            &witness.own,
        )
        .ok_or_else(|| invalid("invitation source not authorized in saved cut"))?;
        source.execution = operation.executor.execution.clone();
        if source != operation.executor {
            return Err(invalid("invitation original source lifetime changed"));
        }
    }
    let decision = expected_invitation(group, &projection, input, witness)
        .map_err(|_| invalid("invalid saved invitation decision"))?
        .ok_or_else(|| invalid("invitation already existed in saved cut"))?;
    if first.ciphertext != decision.meta.encode_invitation() {
        return Err(invalid("invitation membership delta changed"));
    }
    let mut expected = decision.control;
    expected
        .grants
        .last_mut()
        .expect("new invitation grant")
        .membership_event = first.id;
    let mut actual = AccountAuthorityControl::decode(&batch[1].ciphertext)
        .ok_or_else(|| invalid("invalid invitation authority"))?;
    actual.account_signature.clear();
    if actual != expected {
        return Err(invalid("invitation authority delta changed"));
    }
    let nonheads: HashSet<_> = events
        .iter()
        .flat_map(|event| event.parents.iter().copied())
        .collect();
    let mut heads: Vec<_> = ancestors
        .keys()
        .copied()
        .filter(|id| !nonheads.contains(id))
        .collect();
    heads.sort();
    if heads != witness.parents
        || events
            .iter()
            .map(|event| event.lamport)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            != Some(first.lamport)
        || events
            .iter()
            .filter(|event| event.author == first.author)
            .map(|event| event.seq)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            != Some(first.seq)
        || !crate::node::join_history::valid_saved_invitation_history(&events, batch)
    {
        return Err(invalid(
            "invalid invitation original causal allocation or history",
        ));
    }
    let mut installed = events;
    installed.extend(batch.iter());
    let installed = ChannelBook::project_membership(group, &installed);
    if !installed.authorized.contains(&first.id)
        || installed
            .authority
            .account_grant(
                &witness
                    .target
                    .account_cert
                    .as_ref()
                    .expect("validated target")
                    .account_ed25519_pub,
            )
            .is_none_or(|grant| {
                grant.original != witness.target
                    || grant.incarnation != witness.incarnation
                    || grant.membership_event != first.id
            })
    {
        return Err(invalid("invitation effect not admitted in original view"));
    }
    Ok(decision.recipients)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::Announce,
        eventlog::{Event, EventId, EventKind},
        identity::device::DeviceIdentity,
        node::{
            AccountGroupOperationId, MessageDestination, Node,
            channel_account_authority::{AccountAuthorityControl, AccountGrant},
            channels::account_invitation::tests::fixture,
        },
    };
    struct BoundFixture {
        operation: OperationReference,
        request: RequestRecord,
        input: InvitationInput,
        witness: InvitationWitness,
        body: DeliveryTransaction,
        parents: Vec<Event>,
    }
    fn events(body: &DeliveryTransaction) -> &[Event] {
        let DeliveryTransaction::ChannelOutgoing { events, .. } = body else {
            panic!("invitation fixture must retain original ChannelOutgoing shape");
        };
        events
    }
    fn bound_fixture() -> BoundFixture {
        bound_fixture_with_allocation(3, 3)
    }
    fn bound_fixture_with_allocation(seq: u64, lamport: u64) -> BoundFixture {
        let f = fixture();
        let projection = f.base.projection();
        let state = projection.state.as_ref().unwrap();
        let mut members = f.witness.recipients.clone();
        members.push(f.witness.target.public());
        members.sort_by_key(PublicIdentity::user_id);
        let meta = ChannelMeta {
            name: state.name().into(),
            members,
            epoch: state.epoch() + 1,
            owner: state.owner().into(),
        };
        let invitation = Event::new(
            &f.base.owner,
            f.base.group,
            seq,
            f.witness.parents.clone(),
            lamport,
            3,
            EventKind::MembershipChange,
            meta.encode_invitation(),
        );
        let mut grants = projection.authority.grants();
        grants.push(AccountGrant {
            incarnation: f.witness.incarnation,
            pending: true,
            original: f.witness.target.clone(),
            membership_event: invitation.id,
            acceptance_event: None,
        });
        let authority = AccountAuthorityControl {
            version: 1,
            previous: projection.authority.current_event(),
            issuer: f.witness.own.clone(),
            owner_account: f.base.owner_account.public().ed25519_pub,
            grants,
            revoked: vec![],
            legacy_bindings: vec![],
            account_signature: vec![],
        };
        let authority = Event::new(
            &f.base.owner,
            f.base.group,
            seq + 1,
            vec![invitation.id],
            lamport + 1,
            3,
            EventKind::ChannelAccountAuthority,
            authority.signed_encode(f.base.group, &f.base.owner_account),
        );
        let origin = DeviceIdentity::generate();
        let mut operation = OperationReference {
            id: AccountGroupOperationId::for_origin(&origin.public(), [93; 16]),
            group: f.base.group,
            kind: f.input.operation().kind(),
            digest: crate::node::account_group::operations::journal::operation_digest(
                f.base.group,
                &f.input.operation(),
            ),
            origin: Announce::new_with_account(&origin, &f.base.owner_account, "requester", 1),
            executor: Node::public_account_group_source(
                f.base.group,
                &projection,
                &f.base.owner.public(),
                &f.witness.own,
            )
            .unwrap(),
            signature: vec![],
        };
        operation.signature = origin.sign(&operation.message()).to_vec();
        let request = RequestRecord {
            request_id: *operation.id.as_bytes(),
            digest: operation.digest,
            destination: MessageDestination::Channel(f.base.group),
            message_id: invitation.id,
            accepted_at_ms: 3,
        };
        let body = DeliveryTransaction::ChannelOutgoing {
            events: vec![invitation, authority],
            senders: vec![],
            sent: crate::node::sentlog::SentEntry {
                conversation: f.base.group,
                seq: seq + 1,
                wall_clock: 3,
                plaintext: vec![],
            },
        };
        BoundFixture {
            operation,
            request,
            input: f.input,
            witness: f.witness,
            body,
            parents: f.base.events,
        }
    }
    #[test]
    fn bound_invitation_accepts_exact_existing_signed_batch_with_first_event_request_id() {
        let f = bound_fixture();
        assert!(crate::node::join_history::valid_invitation_transaction(
            events(&f.body)
        ));
        validate_bound(&f.operation, &f.request, &f.input, &f.witness, &f.body).unwrap();
    }
    #[test]
    fn direct_invitation_accepts_same_original_batch_without_operation_wrapper() {
        let f = bound_fixture();
        validate_direct(&f.input, &f.witness, &f.body).unwrap();
    }
    #[test]
    fn invitation_saved_cut_replays_exact_incarnation_after_public_install_and_reopen() {
        let f = bound_fixture();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        for event in &f.parents {
            log.append_durable(event.clone()).unwrap();
        }
        let mut recipients = f.witness.recipients.clone();
        recipients.push(f.witness.target.public());
        recipients.sort_by_key(PublicIdentity::user_id);
        assert_eq!(
            validate_parent_view(
                &f.operation,
                &f.request,
                &f.input,
                &f.witness,
                &f.body,
                &log
            )
            .unwrap(),
            recipients
        );
        for event in events(&f.body) {
            log.append_durable(event.clone()).unwrap();
        }
        drop(log);
        let log = PersistentEventLog::open(&path, "pw").unwrap();
        assert_eq!(
            validate_direct_parent_view(&f.input, &f.witness, &f.body, &log).unwrap(),
            recipients
        );
    }
    #[test]
    fn invitation_rejects_authority_id_as_request_id_and_another_original_digest() {
        let mut f = bound_fixture();
        f.request.message_id = events(&f.body)[1].id;
        assert!(validate_bound(&f.operation, &f.request, &f.input, &f.witness, &f.body).is_err());
        let mut f = bound_fixture();
        f.input.target_account_id = "ab".repeat(16);
        assert!(validate_bound(&f.operation, &f.request, &f.input, &f.witness, &f.body).is_err());
    }
    #[test]
    fn invitation_causal_allocation_rejects_independent_lamport_and_sequence_drift() {
        for (seq, lamport, accepted) in [(3, 3, true), (3, 4, false), (4, 3, false)] {
            let f = bound_fixture_with_allocation(seq, lamport);
            // Every batch still carries authentic, contiguous event/account
            // signatures and the exact original request. Only its allocation
            // against the saved signed parents can reject the drift cases.
            validate_bound(&f.operation, &f.request, &f.input, &f.witness, &f.body).unwrap();
            validate_direct(&f.input, &f.witness, &f.body).unwrap();
            let dir = tempfile::tempdir().unwrap();
            let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
            for parent in &f.parents {
                assert!(parent.verify_integrity() && parent.verify_signature());
                log.append_durable(parent.clone()).unwrap();
            }
            assert_eq!(
                validate_parent_view(
                    &f.operation,
                    &f.request,
                    &f.input,
                    &f.witness,
                    &f.body,
                    &log
                )
                .is_ok(),
                accepted
            );
            assert_eq!(
                validate_direct_parent_view(&f.input, &f.witness, &f.body, &log).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn invitation_rejects_missing_causal_parent_before_public_append() {
        let f = bound_fixture();
        let dir = tempfile::tempdir().unwrap();
        let log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        assert!(
            validate_parent_view(
                &f.operation,
                &f.request,
                &f.input,
                &f.witness,
                &f.body,
                &log
            )
            .is_err()
        );
        assert!(log.get(&f.request.message_id).is_none());
        assert_ne!(f.request.message_id, EventId::new([0; 32]));
    }
}

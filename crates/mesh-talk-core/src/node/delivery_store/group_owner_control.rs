//! Original-intent binding and causal replay validation for owner controls.
use super::{OperationReference, invalid};
use crate::{
    eventlog::{Event, EventKind, LogError, persist::PersistentEventLog},
    node::{
        AccountGroupExecution, AccountGroupOperation, AccountGroupRole, Node,
        channel::ChannelBook,
        channel_account_authority::AccountAuthorityControl,
        channels::owner_control::{
            OwnerControlEffect, OwnerControlInput, OwnerControlWitness, expected_effect,
            witness_rows_match,
        },
    },
};
use std::collections::{HashMap, HashSet};

pub(in crate::node) fn operation_input(input: &OwnerControlInput) -> AccountGroupOperation {
    match input {
        OwnerControlInput::Rename(name) => AccountGroupOperation::Rename { name: name.clone() },
        OwnerControlInput::HistoryPolicy(policy) => {
            AccountGroupOperation::SetHistoryPolicy { policy: *policy }
        }
        OwnerControlInput::Dissolve => AccountGroupOperation::Dissolve,
        OwnerControlInput::RemoveAccount(target_account_id) => {
            AccountGroupOperation::RemoveAccount {
                target_account_id: target_account_id.clone(),
            }
        }
        OwnerControlInput::Transfer(target_account_id) => {
            AccountGroupOperation::TransferOwnership {
                target_account_id: target_account_id.clone(),
            }
        }
    }
}

/// Admission/open can verify the immutable envelope without acquiring a second
/// event log. Causal authorization is mandatory in validate_parent_view before
/// replay publishes any public event.
pub(super) fn validate_bound(
    operation: &OperationReference,
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    validate_common(Some(operation), input, witness, event)
}

pub(super) fn validate_direct(
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    validate_common(None, input, witness, event)
}

fn validate_common(
    operation: Option<&OperationReference>,
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
) -> Result<(), LogError> {
    let account = witness
        .own
        .account_cert
        .as_ref()
        .ok_or_else(|| invalid("owner control has no signed source account"))?;
    if let Some(operation) = operation
        && (matches!(&operation.executor.execution, AccountGroupExecution::ViaDevice(device) if *device != operation.executor.device)
            || operation.kind != operation_input(input).kind()
            || operation.executor.role != AccountGroupRole::Owner
            || operation.executor.device != witness.own.public()
            || !operation.valid(&account.account_ed25519_pub)
            || crate::node::account_group::operations::journal::operation_digest(
                operation.group,
                &operation_input(input),
            ) != operation.digest
            || event.conversation_id != operation.group)
    {
        return Err(invalid("invalid original owner operation binding"));
    }
    if !input.canonical()
        || !witness.own.valid_presence()
        || witness.own.post_office
        || event.author.ed25519_pub() != &witness.own.ed25519_pub
        || event.seq == 0
        || event.lamport == 0
        || event.parents.is_empty()
        || event.parents != witness.parents
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
        || !crate::node::session::event_fits_frame(event)
    {
        return Err(invalid("invalid owner control envelope"));
    }
    let mut last = None;
    let mut identities = HashSet::new();
    for row in &witness.members {
        let public = &row.lifetime.member;
        let key = (public.user_id(), public.ed25519_pub, public.x25519_pub);
        if last.as_ref().is_some_and(|previous| previous >= &key)
            || !identities.insert(public.ed25519_pub)
            || row.current.as_ref().is_some_and(|proof| {
                proof.public() != *public || !proof.valid_presence() || proof.post_office
            })
        {
            return Err(invalid("invalid owner control membership witness"));
        }
        last = Some(key);
    }
    match input {
        OwnerControlInput::Rename(name) => {
            if witness.target_grant.is_some()
                || event.kind != EventKind::ChannelRename
                || event.ciphertext != name.as_bytes()
            {
                return Err(invalid("invalid owner rename effect"));
            }
        }
        OwnerControlInput::HistoryPolicy(policy) => {
            if witness.target_grant.is_some()
                || event.kind != EventKind::ChannelHistoryPolicy
                || event.ciphertext != crate::node::join_history::encode_policy(*policy)
            {
                return Err(invalid("invalid owner history policy effect"));
            }
        }
        OwnerControlInput::Dissolve => {
            if witness.target_grant.is_some()
                || event.kind != EventKind::ChannelDissolve
                || !event.ciphertext.is_empty()
            {
                return Err(invalid("invalid owner dissolve effect"));
            }
        }
        OwnerControlInput::RemoveAccount(target) | OwnerControlInput::Transfer(target) => {
            let transfer = matches!(input, OwnerControlInput::Transfer(_));
            if let Some(grant) = witness.target_grant {
                if (crate::identity::account::AccountPublic {
                    ed25519_pub: grant.account_pub,
                })
                .account_id()
                    != *target
                    || operation
                        .is_some_and(|operation| operation.executor.authority.authority.is_none())
                    || event.kind != EventKind::ChannelAccountAuthority
                {
                    return Err(invalid("invalid owner target grant"));
                }
                let control = AccountAuthorityControl::decode(&event.ciphertext)
                    .ok_or_else(|| invalid("invalid owner account effect"))?;
                if control.encode() != event.ciphertext
                    || control.issuer != witness.own
                    || control.previous.is_none()
                    || operation.is_some_and(|operation| {
                        control.previous != operation.executor.authority.authority
                    })
                    || !control.verify_account_signature(
                        event.conversation_id,
                        &account.account_ed25519_pub,
                    )
                    || (transfer
                        && (control.owner_account != grant.account_pub
                            || !control.revoked.is_empty()))
                    || (!transfer
                        && (control.revoked != [grant]
                            || control.owner_account != account.account_ed25519_pub))
                {
                    return Err(invalid("invalid signed owner account effect"));
                }
            } else if operation
                .is_some_and(|operation| operation.executor.authority.authority.is_some())
                || (transfer && event.kind != EventKind::ChannelOwnerTransfer)
                || (!transfer && event.kind != EventKind::MembershipChange)
            {
                return Err(invalid("invalid legacy account owner effect"));
            }
        }
    }
    Ok(())
}

/// Rebuild the exact immutable parent cut. Later arrivals, target account
/// adoption, grant changes and roster changes cannot retarget a pending effect.
pub(super) fn validate_parent_view(
    operation: &OperationReference,
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<(), LogError> {
    validate_parent_common(Some(operation), input, witness, event, log)
}

pub(super) fn validate_direct_parent_view(
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<(), LogError> {
    validate_parent_common(None, input, witness, event, log)
}

fn validate_parent_common(
    operation: Option<&OperationReference>,
    input: &OwnerControlInput,
    witness: &OwnerControlWitness,
    event: &Event,
    log: &PersistentEventLog,
) -> Result<(), LogError> {
    validate_common(operation, input, witness, event)?;
    let group = event.conversation_id;
    let mut ancestors = HashMap::new();
    let mut pending = witness.parents.clone();
    while let Some(id) = pending.pop() {
        if ancestors.contains_key(&id) {
            continue;
        }
        let parent = log
            .get(&id)
            .ok_or_else(|| invalid("owner control causal parent missing"))?;
        if parent.conversation_id != group || parent.id == event.id {
            return Err(invalid("invalid owner control causal parent"));
        }
        pending.extend(parent.parents.iter().copied());
        ancestors.insert(id, parent);
    }
    let events: Vec<_> = ancestors.values().copied().collect();
    let projection = ChannelBook::project_membership(group, &events);
    let mut source =
        Node::public_account_group_source(group, &projection, &witness.own.public(), &witness.own)
            .ok_or_else(|| invalid("owner control source no longer authorized in saved view"))?;
    // Availability was selected by the origin; it is not public authority.
    if let Some(operation) = operation {
        source.execution = operation.executor.execution.clone();
        if source != operation.executor {
            return Err(invalid("owner control original source changed"));
        }
    }
    if source.role != AccountGroupRole::Owner || !witness_rows_match(&projection, witness) {
        return Err(invalid(
            "owner control source or complete membership lifetime changed",
        ));
    }
    let max_clock = events.iter().map(|event| event.lamport).max().unwrap_or(0);
    let max_seq = events
        .iter()
        .filter(|parent| parent.author == event.author)
        .map(|parent| parent.seq)
        .max()
        .unwrap_or(0);
    let mut heads: Vec<_> = ancestors.keys().copied().collect();
    let parents: HashSet<_> = events
        .iter()
        .flat_map(|parent| parent.parents.iter().copied())
        .collect();
    heads.retain(|id| !parents.contains(id));
    heads.sort();
    if heads != witness.parents
        || max_clock.checked_add(1) != Some(event.lamport)
        || max_seq.checked_add(1) != Some(event.seq)
    {
        return Err(invalid("invalid owner control original causal allocation"));
    }
    let effect = expected_effect(
        group,
        &projection,
        input,
        witness,
        crate::node::join_history::policy(&events),
    )
    .map_err(|_| invalid("invalid canonical owner control decision"))?
    .ok_or_else(|| invalid("owner control carries an effect for an unchanged decision"))?;
    validate_effect(effect, event)
}

fn validate_effect(effect: OwnerControlEffect, event: &Event) -> Result<(), LogError> {
    let valid = match effect {
        OwnerControlEffect::Public(kind, payload) => {
            event.kind == kind && event.ciphertext == payload
        }
        OwnerControlEffect::Account(mut expected) => {
            let Some(mut actual) = AccountAuthorityControl::decode(&event.ciphertext) else {
                return Err(invalid("invalid canonical account control"));
            };
            actual.account_signature.clear();
            expected.account_signature.clear();
            event.kind == EventKind::ChannelAccountAuthority && actual == *expected
        }
    };
    if !valid {
        return Err(invalid(
            "owner control effect differs from original canonical decision",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::ChannelMeta,
        discovery::{Announce, Roster},
        eventlog::ConversationId,
        identity::{account::Account, device::DeviceIdentity},
        node::{AccountGroupOperationId, channels::owner_control::PreparedOwnerControl},
    };
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct Fixture {
        _dir: tempfile::TempDir,
        node: Arc<Node>,
        target: Account,
        devices: Vec<DeviceIdentity>,
        group: ConversationId,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let (dm, _) = mpsc::unbounded_channel();
            let (channels, _) = mpsc::unbounded_channel();
            let (files, _) = mpsc::unbounded_channel();
            let node = Node::open_with_account(
                DeviceIdentity::generate(),
                Account::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channels,
                files,
                &dir.path().join("events"),
                &dir.path().join("sent"),
                "pw",
            )
            .unwrap();
            let target = Account::generate();
            let foreign = Account::generate();
            let devices: Vec<_> = (0..3).map(|_| DeviceIdentity::generate()).collect();
            for (index, device) in devices.iter().enumerate() {
                let proof = Announce::new_with_account(
                    device,
                    if index < 2 { &target } else { &foreign },
                    "Same name",
                    1,
                );
                node.roster.lock().unwrap().update(
                    &proof,
                    "127.0.0.1".parse().unwrap(),
                    &node.user_id(),
                );
            }
            let group = ConversationId::new([0xb8; 32]);
            let meta = ChannelMeta {
                name: "Owner controls".into(),
                members: std::iter::once(node.identity.public())
                    .chain(devices.iter().map(DeviceIdentity::public))
                    .collect(),
                epoch: 0,
                owner: node.user_id(),
            };
            let genesis = Event::new(
                &node.identity,
                group,
                1,
                vec![],
                1,
                1,
                EventKind::MembershipChange,
                meta.encode(),
            );
            node.log.lock().unwrap().append(genesis).unwrap();
            node.process_channel(group);
            Self {
                _dir: dir,
                node,
                target,
                devices,
                group,
            }
        }
        fn prepare(&self, input: &OwnerControlInput) -> (OperationReference, PreparedOwnerControl) {
            let proofs = self.node.owner_control_proof_snapshot();
            let log = self.node.log.lock().unwrap();
            let projection = ChannelBook::project_membership(self.group, &log.events(&self.group));
            let prepared = self
                .node
                .prepare_owner_control(self.group, &log, &projection, input, &proofs)
                .unwrap()
                .unwrap();
            let source = Node::public_account_group_source(
                self.group,
                &projection,
                &self.node.identity.public(),
                &prepared.witness.own,
            )
            .unwrap();
            let reference = OperationReference::new(
                &self.node,
                AccountGroupOperationId::for_origin(&self.node.identity.public(), [0xb9; 16]),
                self.group,
                &operation_input(input),
                source,
            );
            (reference, prepared)
        }
        fn validate(
            &self,
            reference: &OperationReference,
            input: &OwnerControlInput,
            prepared: &PreparedOwnerControl,
        ) -> Result<(), LogError> {
            validate_parent_view(
                reference,
                input,
                &prepared.witness,
                &prepared.event,
                &self.node.log.lock().unwrap(),
            )
        }
        fn resign(&self, prepared: &mut PreparedOwnerControl, payload: Vec<u8>) {
            let old = &prepared.event;
            prepared.event = Event::new(
                &self.node.identity,
                self.group,
                old.seq,
                old.parents.clone(),
                old.lamport,
                old.wall_clock,
                old.kind,
                payload,
            );
        }
    }

    #[test]
    fn offline_legacy_targets_keep_their_verified_identity_without_a_live_route() {
        let fixture = Fixture::new();
        for device in &fixture.devices[..2] {
            fixture.node.roster.lock().unwrap().forget(&device.public());
        }
        let input = OwnerControlInput::RemoveAccount(fixture.target.account_id());
        let (reference, prepared) = fixture.prepare(&input);
        fixture.validate(&reference, &input, &prepared).unwrap();
        let metadata = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
        assert_eq!(
            metadata.members.len(),
            2,
            "offline availability does not erase the target account's signed identity"
        );
    }

    #[test]
    fn completed_link_never_reuses_the_members_retired_account_proof() {
        let fixture = Fixture::new();
        fixture
            .node
            .configure_privacy(
                fixture._dir.path(),
                "pw",
                &fixture.node.signed_announce("Owner", 1),
                Arc::new(crate::discovery::DiscoveryVisibility::default()),
            )
            .unwrap();
        fixture.node.cache_discovered_peers().unwrap();
        let linked = crate::node::AccountDevice {
            public: fixture.devices[0].public(),
            certificate: fixture
                .node
                .account
                .certify(&fixture.devices[0].public().ed25519_pub),
        };
        fixture
            .node
            .account_devices
            .lock()
            .unwrap()
            .insert(linked.clone())
            .unwrap();
        fixture
            .node
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .authorize_linked_device(linked, &fixture.node.account_id())
            .unwrap();
        assert_eq!(
            fixture
                .node
                .account_id_for_device(&fixture.devices[0].public()),
            Some(fixture.node.account_id())
        );
        let input = OwnerControlInput::RemoveAccount(fixture.target.account_id());
        let (_, prepared) = fixture.prepare(&input);
        let metadata = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
        assert!(
            metadata.members.contains(&fixture.devices[0].public()),
            "a newly linked sibling must not be removed through its retired account"
        );
        assert!(
            !metadata.members.contains(&fixture.devices[1].public()),
            "the remaining device still belonging to the target account is removed"
        );
    }

    #[test]
    fn legacy_remove_binds_all_certified_target_devices_and_saved_parent_view() {
        let f = Fixture::new();
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (reference, prepared) = f.prepare(&input);
        f.validate(&reference, &input, &prepared).unwrap();
        let meta = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
        assert_eq!(meta.epoch, 1);
        assert_eq!(meta.members.len(), 2);
        assert!(meta.members.contains(&f.node.identity.public()));
        assert!(meta.members.contains(&f.devices[2].public()));
        // A later accepted effect cannot turn saved replay into a new election.
        let later = Event::new(
            &f.devices[2],
            f.group,
            1,
            prepared.witness.parents.clone(),
            2,
            2,
            EventKind::Profile,
            b"Later".to_vec(),
        );
        f.node.log.lock().unwrap().append(later).unwrap();
        f.validate(&reference, &input, &prepared).unwrap();
    }

    #[test]
    fn legacy_transfer_uses_deterministic_full_public_target() {
        let f = Fixture::new();
        let input = OwnerControlInput::Transfer(f.target.account_id());
        let (reference, prepared) = f.prepare(&input);
        f.validate(&reference, &input, &prepared).unwrap();
        let mut candidates: Vec<_> = f.devices[..2].iter().map(DeviceIdentity::public).collect();
        candidates.sort_by_key(|public| (public.user_id(), public.ed25519_pub, public.x25519_pub));
        assert_eq!(
            prepared.event.ciphertext,
            candidates[0].user_id().into_bytes()
        );
    }

    #[test]
    fn owner_control_envelope_rejects_unsorted_distinct_members_before_parent_replay() {
        let f = Fixture::new();
        let input = OwnerControlInput::Rename("Envelope ordering".into());
        let (reference, mut prepared) = f.prepare(&input);
        validate_direct(&input, &prepared.witness, &prepared.event).unwrap();
        validate_bound(&reference, &input, &prepared.witness, &prepared.event).unwrap();
        let identities = prepared
            .witness
            .members
            .iter()
            .map(|row| row.lifetime.member.ed25519_pub)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(identities.len(), prepared.witness.members.len());
        assert!(prepared.witness.members.len() >= 2);
        prepared.witness.members.swap(0, 1);
        // Keep every signed proof and the original event/intent intact. Only
        // canonical ordering is invalid; uniqueness cannot mask its rejection.
        assert!(prepared.event.verify_integrity() && prepared.event.verify_signature());
        assert!(reference.valid(&f.node.account.public().ed25519_pub));
        assert!(validate_direct(&input, &prepared.witness, &prepared.event).is_err());
        assert!(validate_bound(&reference, &input, &prepared.witness, &prepared.event).is_err());
    }

    #[test]
    fn owner_witness_rejects_omitted_physical_lifetime_and_changed_source() {
        let f = Fixture::new();
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (reference, mut prepared) = f.prepare(&input);
        prepared.witness.members.remove(0);
        assert!(f.validate(&reference, &input, &prepared).is_err());
        let (mut reference, prepared) = f.prepare(&input);
        reference.executor.lifetime.acceptance = Some(prepared.event.id);
        reference.signature = f.node.identity.sign(&reference.message()).to_vec();
        assert!(f.validate(&reference, &input, &prepared).is_err());
    }

    #[test]
    fn owner_control_rejects_resigned_partial_or_foreign_account_removal() {
        for foreign in [false, true] {
            let f = Fixture::new();
            let input = OwnerControlInput::RemoveAccount(f.target.account_id());
            let (reference, mut prepared) = f.prepare(&input);
            let mut meta = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
            if foreign {
                meta.members
                    .retain(|public| *public != f.devices[2].public());
            } else {
                meta.members.push(f.devices[0].public());
            }
            f.resign(&mut prepared, meta.encode());
            assert!(f.validate(&reference, &input, &prepared).is_err());
        }
    }

    #[test]
    fn owner_control_rejects_target_change_and_unavailable_causal_parents() {
        let f = Fixture::new();
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (reference, mut prepared) = f.prepare(&input);
        let renamed = OwnerControlInput::Transfer(f.target.account_id());
        assert!(f.validate(&reference, &renamed, &prepared).is_err());
        let parent = crate::eventlog::EventId::new([0xbf; 32]);
        let old = &prepared.event;
        prepared.event = Event::new(
            &f.node.identity,
            f.group,
            old.seq,
            vec![parent],
            old.lamport,
            old.wall_clock,
            old.kind,
            old.ciphertext.clone(),
        );
        prepared.witness.parents = vec![parent];
        assert!(f.validate(&reference, &input, &prepared).is_err());
    }
    #[test]
    fn owner_operation_preserves_original_waiting_availability() {
        let f = Fixture::new();
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (reference, prepared) = f.prepare(&input);
        for execution in [
            AccountGroupExecution::WaitingForDevice,
            AccountGroupExecution::RequiresUpgrade,
            AccountGroupExecution::Reconciling,
        ] {
            let mut original = reference.clone();
            original.executor.execution = execution;
            original.signature = f.node.identity.sign(&original.message()).to_vec();
            f.validate(&original, &input, &prepared).unwrap();
        }
    }

    #[test]
    fn direct_owner_control_uses_same_saved_causal_validation() {
        let f = Fixture::new();
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (_, mut prepared) = f.prepare(&input);
        validate_direct(&input, &prepared.witness, &prepared.event).unwrap();
        validate_direct_parent_view(
            &input,
            &prepared.witness,
            &prepared.event,
            &f.node.log.lock().unwrap(),
        )
        .unwrap();
        let mut meta = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
        meta.members.push(f.devices[0].public());
        f.resign(&mut prepared, meta.encode());
        assert!(
            validate_direct_parent_view(
                &input,
                &prepared.witness,
                &prepared.event,
                &f.node.log.lock().unwrap()
            )
            .is_err()
        );
    }

    #[test]
    fn legacy_transfer_to_owner_account_is_unchanged() {
        let f = Fixture::new();
        let proofs = f.node.owner_control_proof_snapshot();
        let log = f.node.log.lock().unwrap();
        let projection = ChannelBook::project_membership(f.group, &log.events(&f.group));
        let input = OwnerControlInput::Transfer(f.node.account_id());
        assert!(
            f.node
                .prepare_owner_control(f.group, &log, &projection, &input, &proofs)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_remove_also_closes_certified_pending_account_devices() {
        let f = Fixture::new();
        let invited = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&invited, &f.target, "Same name", 1);
        f.node.roster.lock().unwrap().update(
            &proof,
            "127.0.0.1".parse().unwrap(),
            &f.node.user_id(),
        );
        {
            let mut log = f.node.log.lock().unwrap();
            let mut members = vec![f.node.identity.public()];
            members.extend(f.devices.iter().map(DeviceIdentity::public));
            members.push(invited.public());
            let meta = ChannelMeta {
                name: "Owner controls".into(),
                members,
                epoch: 1,
                owner: f.node.user_id(),
            };
            let (parents, lamport) = log.prepare(&f.group).unwrap();
            let event = Event::new(
                &f.node.identity,
                f.group,
                2,
                parents,
                lamport,
                2,
                EventKind::MembershipChange,
                meta.encode_invitation(),
            );
            log.append(event).unwrap();
        }
        let input = OwnerControlInput::RemoveAccount(f.target.account_id());
        let (reference, prepared) = f.prepare(&input);
        assert!(
            prepared
                .witness
                .members
                .iter()
                .any(|row| row.lifetime.member == invited.public() && !row.accepted)
        );
        f.validate(&reference, &input, &prepared).unwrap();
        let meta = ChannelMeta::decode(&prepared.event.ciphertext).unwrap();
        assert_eq!(meta.members.len(), 2);
        assert!(!meta.members.contains(&invited.public()));
    }
}

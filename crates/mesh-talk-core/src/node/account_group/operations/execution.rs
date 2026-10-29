//! Source execution uses the existing delivery allocation and recovery owner.
use super::{
    journal::{self, Entry},
    wire::*,
    *,
};
use crate::node::delivery_store::{DeliveryTransaction, RequestRecord};
use std::{collections::BTreeSet, sync::Arc};

fn requested_effect(
    reference: &journal::OperationReference,
    effect: DeliveryTransaction,
    message_id: EventId,
) -> DeliveryTransaction {
    DeliveryTransaction::GroupRequested {
        operation: reference.clone(),
        transaction: Box::new(DeliveryTransaction::Requested {
            request: RequestRecord {
                request_id: *reference.id.as_bytes(),
                digest: reference.digest,
                destination: crate::node::MessageDestination::Channel(reference.group),
                message_id,
                accepted_at_ms: crate::node::node::now_millis(),
            },
            transaction: Box::new(effect),
        }),
    }
}

impl Node {
    #[cfg(test)]
    pub(in crate::node) fn group_operation_journal_revision_for_test(
        &self,
        id: AccountGroupOperationId,
    ) -> Option<u64> {
        self.group_operations
            .lock()
            .unwrap()
            .entries
            .get(&id)
            .map(|entry| entry.sequence)
    }

    #[cfg(test)]
    pub(in crate::node) fn admit_account_group_request_for_test(
        &self,
        request: RequestProjection,
    ) -> Result<(), NodeError> {
        self.admit_source_group_request(request)
    }

    #[cfg(test)]
    pub(in crate::node) async fn maintain_account_group_operations(self: &Arc<Self>) {
        for group in self.maintain_account_group_operation_effects() {
            let members = self.channel_members(group);
            self.distribute_account_group_effects(group, &members).await;
        }
    }

    /// Distribute every control while confirming the latest exact source
    /// message on that same bounded sync exchange, without a second fanout.
    /// Coalesced earlier messages remain eligible for ordinary custody repair.
    pub(in crate::node) async fn distribute_account_group_effects(
        &self,
        group: ConversationId,
        members: &[crate::identity::device::PublicIdentity],
    ) -> bool {
        if let Some(event) = self.account_group_fanout_message(group) {
            let projection = self.account_group_projection(group);
            let eligible = self
                .channels
                .lock()
                .expect("channels mutex")
                .state(&group)
                .is_some_and(|state| {
                    !state.is_dissolved()
                        && !state.pending_invite()
                        && self.channel_identity_matches_account(state)
                        && self.channel_identity_is_current_in(state, &projection)
                });
            if eligible {
                return self
                    .distribute_channel_confirming(group, members, Some(event))
                    .await;
            }
        }
        // Controls still reach every member, including departure/dissolution
        // effects and coalesced controls in groups larger than message fanout.
        self.distribute_channel_confirming(group, members, None)
            .await
    }

    /// Exercise the real admitted effect with the existing completion-write fault.
    #[cfg(test)]
    pub(in crate::node) fn interrupt_account_group_completion_for_test(
        &self,
        group: ConversationId,
        id: AccountGroupOperationId,
    ) -> Result<Option<ConversationId>, NodeError> {
        let request = self
            .operation_projection(group)
            .requests
            .into_iter()
            .find(|request| request.reference.id == id)
            .expect("a submitted original request is required");
        self.admit_source_group_request(request)?;
        self.fail_account_group_operation_journal_appends_for_test(true)
            .unwrap();
        self.execute_source_control(id)
    }

    /// Commit bounded local work and publish its private metadata without waiting
    /// for public distribution. The runtime owns scheduling that network work.
    pub(in crate::node) fn maintain_account_group_operation_effects(
        self: &Arc<Self>,
    ) -> Vec<ConversationId> {
        // Link the separately signed child namespace before ordinary admission
        // can interpret a physical departure as a whole-account coordinator.
        for (key, hash, frame) in self.unseen_account_group_legacy_inputs() {
            if self
                .operation_seen
                .lock()
                .expect("operation input mutex")
                .get(&key)
                == Some(&hash)
            {
                continue;
            }
            let mut processed = true;
            for leg in frame.projection.requests {
                if leg.parent.executor.device != self.identity.public()
                    && leg.request.reference.executor.device != self.identity.public()
                {
                    continue;
                }
                let request = leg.request;
                let child_id = request.reference.id;
                let own_child = request.reference.executor.device == self.identity.public();
                let linked = self
                    .group_operations
                    .lock()
                    .expect("operation journal mutex")
                    .put_legacy_child(
                        leg.parent,
                        Entry {
                            file_copy: None,
                            reference: request.reference.clone(),
                            body: request.body.clone(),
                            status: self.operation_status_for(
                                &request.reference,
                                AccountGroupOperationPhase::AwaitingAcceptance,
                            ),
                            source: false,
                            cancel: request.cancel,
                            sequence: 0,
                        },
                    );
                if linked.is_err()
                    || (own_child && self.admit_source_group_request(request).is_err())
                {
                    processed = false;
                } else if own_child {
                    // A newly observed parent query must lift this terminal
                    // result into the bounded publication window. The outer
                    // signed-frame hash coalesces unchanged request nonces.
                    let mut journal = self
                        .group_operations
                        .lock()
                        .expect("operation journal mutex");
                    if let Some(entry) = journal
                        .entries
                        .get(&child_id)
                        .filter(|entry| entry.source && entry.terminal())
                        .cloned()
                        && journal.put(entry).is_err()
                    {
                        processed = false;
                    }
                }
            }
            for leg in frame.projection.results {
                if leg.parent.executor.device != self.identity.public()
                    && leg.result.reference.executor.device != self.identity.public()
                {
                    continue;
                }
                let linked = self
                    .group_operations
                    .lock()
                    .expect("operation journal mutex")
                    .put_legacy_child(
                        leg.parent,
                        Entry {
                            file_copy: None,
                            reference: leg.result.reference.clone(),
                            body: Some(AccountGroupOperation::LeaveAccount),
                            status: self.operation_status_for(
                                &leg.result.reference,
                                AccountGroupOperationPhase::AwaitingAcceptance,
                            ),
                            source: false,
                            cancel: false,
                            sequence: 0,
                        },
                    );
                if linked.is_err()
                    || self
                        .import_group_operation_result(leg.result, &frame.proof)
                        .is_err()
                {
                    processed = false;
                }
            }
            if processed {
                self.operation_seen
                    .lock()
                    .expect("operation input mutex")
                    .insert(key, hash);
            }
        }
        for (key, hash, frame) in self.unseen_account_group_operation_inputs() {
            if self
                .operation_seen
                .lock()
                .expect("operation input mutex")
                .get(&key)
                == Some(&hash)
            {
                continue;
            }
            let mut processed = true;
            for request in frame.projection.requests {
                if request.reference.executor.device == self.identity.public() {
                    let query = (request.reference.clone(), request.nonce);
                    if self.admit_source_group_request(request).is_err()
                        || self
                            .observe_legacy_leave_parent_query(&query.0, query.1)
                            .is_err()
                    {
                        processed = false;
                    }
                } else {
                    let mut journal = self
                        .group_operations
                        .lock()
                        .expect("group operation journal mutex");
                    if !journal.entries.contains_key(&request.reference.id)
                        && !journal.lookups.contains_key(&request.reference.id)
                    {
                        let status = self.operation_status_for(
                            &request.reference,
                            AccountGroupOperationPhase::AwaitingAcceptance,
                        );
                        let _ = journal.put(Entry {
                            file_copy: None,
                            reference: request.reference,
                            body: request.body,
                            status,
                            source: false,
                            cancel: request.cancel,
                            sequence: 0,
                        });
                    }
                }
            }
            for result in frame.projection.results {
                if self
                    .import_group_operation_result(result, &frame.proof)
                    .is_err()
                {
                    processed = false;
                }
            }
            for lookup in frame.projection.lookups {
                let mut journal = self
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex");
                if let Some(mut entry) = journal
                    .entries
                    .get(&lookup.id)
                    .filter(|entry| entry.source)
                    .cloned()
                {
                    // Republish the source's original reference/result, including a
                    // conflicting-body response, without executing it again.
                    if lookup.cancel && !entry.terminal() {
                        entry.cancel = true;
                    }
                    let _ = journal.put(entry);
                }
            }
            if processed {
                self.operation_seen
                    .lock()
                    .expect("operation input mutex")
                    .insert(key, hash);
            }
        }
        let ready = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .take_source_ready(&self.identity.public(), 8);
        let mut fanout = BTreeSet::new();
        for entry in ready {
            let request = RequestProjection {
                reference: entry.reference.clone(),
                body: entry.body.clone(),
                cancel: entry.cancel,
                nonce: [0; 16],
            };
            if self.admit_source_group_request(request).is_err() {
                continue;
            }
            match self.execute_source_control(entry.reference.id) {
                Ok(Some(group)) => {
                    fanout.insert(group.as_bytes().to_owned());
                }
                Ok(None) => {}
                Err(error) => log::debug!("account group effect awaits recovery: {error}"),
            }
        }
        // Publish each affected canonical source before an unrelated slow
        // member can hold up the existing public control distribution.
        for group in &fanout {
            let group = ConversationId::new(*group);
            self.publish_account_group_access_for(group);
            self.publish_group_inventory_for(group);
        }
        self.publish_account_group_operations();
        self.publish_account_group_legacy_operations();
        // No host, journal, membership or delivery guard escapes local work.
        fanout.into_iter().map(ConversationId::new).collect()
    }
    fn observe_legacy_leave_parent_query(
        &self,
        reference: &journal::OperationReference,
        nonce: [u8; 16],
    ) -> Result<(), NodeError> {
        if reference.kind != AccountGroupOperationKind::LeaveAccount
            || reference.executor.authority.authority.is_some()
        {
            return Ok(());
        }
        let parent = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            if journal.legacy_children.contains_key(&reference.id) {
                return Ok(());
            }
            journal
                .entries
                .get(&reference.id)
                .filter(|entry| entry.source && entry.reference.same(reference))
                .cloned()
        };
        let Some(parent) = parent else { return Ok(()) };
        let key = format!("legacy-parent-query:{}", reference.id.to_hex());
        let mut token = [0; 32];
        token[..16].copy_from_slice(&nonce);
        if self
            .operation_seen
            .lock()
            .expect("operation input mutex")
            .get(&key)
            == Some(&token)
        {
            return Ok(());
        }
        // Reference-only origins poll the parent, not its hidden physical legs.
        // Forward that fresh query through the coordinator's existing nonce so
        // bounded child result windows can rotate without a new wire field.
        if parent.terminal() {
            self.group_operations
                .lock()
                .expect("operation journal mutex")
                .put(parent)
                .map_err(NodeError::Log)?;
        }
        {
            let mut queries = self
                .operation_queries
                .lock()
                .expect("operation query mutex");
            let now = std::time::Instant::now();
            let query = queries.entry(reference.id).or_insert((0, now));
            query.0 = query.0.checked_add(1).ok_or_else(|| {
                NodeError::InvalidInput("legacy departure query exhausted".into())
            })?;
            query.1 = now;
        }
        self.operation_seen
            .lock()
            .expect("operation input mutex")
            .insert(key, token);
        Ok(())
    }
    fn legacy_member_leave_source_matches(
        &self,
        reference: &journal::OperationReference,
        source: &AccountGroupSource,
    ) -> bool {
        if reference.kind != AccountGroupOperationKind::LeaveAccount
            || reference.executor.role != AccountGroupRole::Member
            || source.role != AccountGroupRole::Member
            || reference.executor.authority.authority.is_some()
            || source.authority.authority.is_some()
            || source.authority.epoch < reference.executor.authority.epoch
        {
            return false;
        }
        // Other legacy departures advance the epoch without replacing this
        // physical membership. Parents and children retain that exact lifetime;
        // migration, reinvitation, identity changes and backward epochs do not.
        let mut current = source.clone();
        current.execution = reference.executor.execution.clone();
        current.authority.epoch = reference.executor.authority.epoch;
        current == reference.executor
    }
    pub(super) fn operation_status_for(
        &self,
        reference: &journal::OperationReference,
        phase: AccountGroupOperationPhase,
    ) -> AccountGroupOperationStatus {
        AccountGroupOperationStatus {
            id: reference.id,
            account_id: self.account_id(),
            group: reference.group,
            kind: reference.kind,
            phase: phase.clone(),
            executions: vec![AccountGroupOperationExecution {
                device_id: reference.executor.device.user_id(),
                phase,
                committed_events: Vec::new(),
            }],
            committed_events: Vec::new(),
            primary_event: None,
        }
    }
    pub(super) fn admit_source_group_request(
        &self,
        request: RequestProjection,
    ) -> Result<(), NodeError> {
        if request.reference.executor.device != self.identity.public()
            || !request.reference.valid(&self.account.public().ed25519_pub)
        {
            return Err(NodeError::Authorization(
                "invalid operation executor".into(),
            ));
        }
        let legacy_child = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            let child = journal.legacy_children.contains_key(&request.reference.id);
            if child
                && journal
                    .entries
                    .get(&request.reference.id)
                    .is_none_or(|entry| !entry.reference.same(&request.reference))
            {
                return Err(NodeError::Authorization(
                    "legacy departure child binding changed".into(),
                ));
            }
            child
        };
        if legacy_child && self.try_complete_retired_legacy_leave_child(&request)? {
            return Ok(());
        }
        let action_input = request
            .body
            .as_ref()
            .and_then(AccountGroupOperation::message_action_input);
        let file_payload = request.body.as_ref().and_then(|body| match body {
            AccountGroupOperation::Send { message }
                if message.kind == AccountGroupOperationKind::File =>
            {
                super::file_preparation::GroupFilePayload::decode(message.encoded_body())
            }
            _ => None,
        });
        // Cache checks acquire their own journal/log guards before membership
        // and delivery. Bulk transfer is owned by the existing attachment lane.
        let file_ready = file_payload.as_ref().is_some_and(|payload| {
            self.account_group_file_assets_ready(&request.reference, payload)
        });
        let existing_source = self
            .group_operations
            .lock()
            .expect("operation journal mutex")
            .entries
            .get(&request.reference.id)
            .is_some_and(|entry| entry.source);
        let mut action_metadata_unavailable = false;
        let action_target = if request.cancel || existing_source {
            None
        } else if let Some(input) = &action_input {
            match self.account_group_action_target(request.reference.group, input) {
                Ok(target) => target,
                Err(NodeError::InvalidInput(_)) => {
                    action_metadata_unavailable = true;
                    None
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("membership commit mutex");
        let owner_input = request
            .body
            .as_ref()
            .and_then(AccountGroupOperation::owner_control_input);
        let owner_proofs = owner_input
            .as_ref()
            .map(|_| self.owner_control_proof_snapshot());
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let existing = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .get(&request.reference.id)
            .cloned();
        if let Some(mut entry) = existing {
            if entry.reference.group != request.reference.group
                || entry.reference.digest != request.reference.digest
            {
                return Ok(()); // The canonical source result remains the only result for this ID.
            }
            if entry.source {
                if !request.cancel || entry.terminal() || entry.cancel {
                    return Ok(());
                }
                entry.cancel = true;
                entry.status.phase = AccountGroupOperationPhase::CancellationRequested;
                return self
                    .group_operations
                    .lock()
                    .expect("group operation journal mutex")
                    .put(entry)
                    .map_err(NodeError::Log);
            }
        }
        if request.body.is_none() && !request.cancel {
            return Ok(());
        }
        let mut phase = AccountGroupOperationPhase::Accepted;
        if request.cancel {
            phase = AccountGroupOperationPhase::Cancelled;
        } else {
            let access = self.account_group_access(request.reference.group)?;
            let source = access
                .sources
                .iter()
                .find(|source| source.device == self.identity.public());
            if access.role == AccountGroupRole::Unknown {
                return Err(NodeError::Channel("group authority is reconciling".into()));
            }
            // A private authority observation can precede its public causal
            // events. Keep the original intent retryable until this source
            // can evaluate that view; capability support alone is insufficient.
            if source.is_some_and(|source| source.execution == AccountGroupExecution::Reconciling) {
                return Err(NodeError::Channel("group authority is reconciling".into()));
            }
            if !request.reference.kind.admits_role(access.role)
                || source.is_none_or(|source| !request.reference.kind.admits_role(source.role))
            {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::NotAuthorized,
                };
            } else if source.is_some_and(|source| {
                source.lifetime != request.reference.executor.lifetime
                    || source.authority != request.reference.executor.authority
            }) && !source.is_some_and(|source| {
                self.legacy_member_leave_source_matches(&request.reference, source)
            }) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::MembershipChanged,
                };
            }
            if request.body.as_ref().is_none_or(|body| {
                !body.valid_execution_payload()
                    || body.kind() != request.reference.kind
                    || journal::operation_digest(request.reference.group, body)
                        != request.reference.digest
            }) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::InvalidPayload,
                };
            } else if let Some(AccountGroupOperation::Send { message }) = request.body.as_ref() {
                // Remote intents enter the same expiry policy as local sends.
                // Existing admitted IDs returned above retain their outcome.
                if message
                    .expires_at_ms()
                    .is_some_and(|deadline| deadline <= self.expiry_now())
                {
                    phase = AccountGroupOperationPhase::Rejected {
                        reason: AccountGroupOperationRejection::Expired,
                    };
                } else if message.validate_new_admission().is_err() {
                    phase = AccountGroupOperationPhase::Rejected {
                        reason: AccountGroupOperationRejection::InvalidPayload,
                    };
                }
            }
        }
        if phase == AccountGroupOperationPhase::Accepted
            && let (Some(input), Some(proofs)) = (owner_input, owner_proofs)
        {
            let log = self.log.lock().expect("log mutex");
            let projection = crate::node::channel::ChannelBook::project_membership(
                request.reference.group,
                &log.events(&request.reference.group),
            );
            match self.prepare_owner_control(
                request.reference.group,
                &log,
                &projection,
                &input,
                &proofs,
            ) {
                Ok(Some(prepared)) => {
                    let message_id = prepared.event.id;
                    let transaction = requested_effect(
                        &request.reference,
                        DeliveryTransaction::AccountOwnerControl {
                            input,
                            witness: prepared.witness,
                            events: vec![prepared.event],
                        },
                        message_id,
                    );
                    if delivery
                        .preflight_group_control_transaction(&transaction, &log)
                        .is_err()
                    {
                        phase = AccountGroupOperationPhase::Rejected {
                            reason: AccountGroupOperationRejection::ResourceLimit,
                        };
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    phase = AccountGroupOperationPhase::Rejected {
                        reason: AccountGroupOperationRejection::InvalidTarget,
                    };
                }
            }
        }
        if phase == AccountGroupOperationPhase::Accepted
            && let Some(input) = request
                .body
                .as_ref()
                .and_then(AccountGroupOperation::self_control_input)
        {
            let log = self.log.lock().expect("log mutex");
            let projection = crate::node::channel::ChannelBook::project_membership(
                request.reference.group,
                &log.events(&request.reference.group),
            );
            if legacy_child {
                match self.prepare_legacy_member_leave_locked(
                    request.reference.group,
                    &log,
                    &projection,
                    &self.own_group_proof(),
                ) {
                    Ok(prepared) => {
                        let id = prepared.event.id;
                        let transaction = requested_effect(
                            &request.reference,
                            DeliveryTransaction::LegacyMemberLeave {
                                witness: prepared.witness,
                                events: vec![prepared.event],
                            },
                            id,
                        );
                        if delivery
                            .preflight_legacy_member_leave_transaction(&transaction, &log)
                            .is_err()
                        {
                            phase = AccountGroupOperationPhase::Rejected {
                                reason: AccountGroupOperationRejection::ResourceLimit,
                            };
                        }
                    }
                    Err(_) => {
                        phase = AccountGroupOperationPhase::Rejected {
                            reason: AccountGroupOperationRejection::MembershipChanged,
                        };
                    }
                }
            } else if request.reference.kind == AccountGroupOperationKind::LeaveAccount
                && request.reference.executor.authority.authority.is_none()
            {
                // The durable coordinator prepares all exact physical legs;
                // the parent itself never borrows modern account authority.
            } else {
                match self.prepare_account_self_control_locked(
                    request.reference.group,
                    &log,
                    &projection,
                    input,
                    &self.own_group_proof(),
                ) {
                    Ok(prepared) => {
                        let id = prepared.event.id;
                        let transaction = requested_effect(
                            &request.reference,
                            DeliveryTransaction::AccountSelfControl {
                                input,
                                witness: prepared.witness,
                                events: vec![prepared.event],
                            },
                            id,
                        );
                        if delivery
                            .preflight_group_self_control_transaction(&transaction, &log)
                            .is_err()
                        {
                            phase = AccountGroupOperationPhase::Rejected {
                                reason: AccountGroupOperationRejection::ResourceLimit,
                            };
                        }
                    }
                    Err(_) => {
                        phase = AccountGroupOperationPhase::Rejected {
                            reason: AccountGroupOperationRejection::MembershipChanged,
                        };
                    }
                }
            }
        }
        if phase == AccountGroupOperationPhase::Accepted && action_metadata_unavailable {
            phase = AccountGroupOperationPhase::Rejected {
                reason: AccountGroupOperationRejection::InvalidTarget,
            };
        }
        if phase == AccountGroupOperationPhase::Accepted
            && let Some(input) = action_input
        {
            let now = self.expiry_now();
            if action_preparation::action_target_expired(action_target.as_ref(), now) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::Expired,
                };
            } else {
                let log = self.log.lock().expect("log mutex");
                let projection = crate::node::channel::ChannelBook::project_membership(
                    request.reference.group,
                    &log.events(&request.reference.group),
                );
                match self.prepare_channel_message_action_locked(
                    request.reference.group,
                    &log,
                    &projection,
                    &input,
                    crate::node::message_actions::MessageActionAdmission {
                        own: self.own_group_proof(),
                        target: action_target,
                        admitted_at_ms: now,
                    },
                ) {
                    Ok(prepared) => {
                        let id = prepared.event.id;
                        let transaction = requested_effect(
                            &request.reference,
                            DeliveryTransaction::ChannelMessageAction {
                                input,
                                witness: prepared.witness,
                                events: vec![prepared.event],
                                recall: prepared.recall,
                            },
                            id,
                        );
                        if delivery
                            .preflight_group_message_action(&transaction, &log, &self.identity)
                            .is_err()
                        {
                            phase = AccountGroupOperationPhase::Rejected {
                                reason: AccountGroupOperationRejection::ResourceLimit,
                            };
                        }
                    }
                    Err(_) => {
                        phase = AccountGroupOperationPhase::Rejected {
                            reason: AccountGroupOperationRejection::InvalidTarget,
                        }
                    }
                }
            }
        }
        if phase == AccountGroupOperationPhase::Accepted
            && let Some(payload) = file_payload
        {
            if payload.descriptor.as_ref().is_none_or(|descriptor| {
                !descriptor.valid_for(&request.reference, &payload.logical)
            }) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::InvalidPayload,
                };
            } else if !file_ready {
                let status = self.operation_status_for(
                    &request.reference,
                    AccountGroupOperationPhase::AwaitingAcceptance,
                );
                self.group_operations
                    .lock()
                    .expect("operation journal mutex")
                    .put(Entry {
                        file_copy: None,
                        reference: request.reference,
                        body: request.body,
                        status,
                        source: false,
                        cancel: false,
                        sequence: 0,
                    })
                    .map_err(NodeError::Log)?;
                self.account_sync_notify.notify_one();
                return Err(NodeError::File(
                    "original File assets are reconciling".into(),
                ));
            }
        }
        let status = self.operation_status_for(&request.reference, phase);
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .put(Entry {
                file_copy: None,
                reference: request.reference,
                body: request.body,
                status,
                source: true,
                cancel: request.cancel,
                sequence: 0,
            })
            .map_err(NodeError::Log)
    }
    pub(super) fn execute_source_control(
        &self,
        id: AccountGroupOperationId,
    ) -> Result<Option<ConversationId>, NodeError> {
        self.execute_source_control_with_invitation(id, None)
    }

    pub(super) fn execute_source_control_with_invitation(
        &self,
        id: AccountGroupOperationId,
        mut invitation_assets: Option<crate::node::channels::account_invitation::InvitationAssets>,
    ) -> Result<Option<ConversationId>, NodeError> {
        // The account-wide legacy parent owns no public event. Its durable
        // physical plan must progress even after this coordinator has left.
        if self.try_execute_legacy_leave_coordinator(id)? {
            return Ok(None);
        }
        let file_snapshot = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            journal
                .entries
                .get(&id)
                .filter(|entry| {
                    !entry.terminal()
                        && !entry.cancel
                        && entry.reference.kind == AccountGroupOperationKind::File
                })
                .and_then(|entry| {
                    let AccountGroupOperation::Send { message } = entry.body.as_ref()? else {
                        return None;
                    };
                    let payload =
                        super::file_preparation::GroupFilePayload::decode(message.encoded_body())?;
                    Some((
                        entry.reference.clone(),
                        payload,
                        journal.file_asset_receipt(id)?.clone(),
                    ))
                })
        };
        let file_snapshot = file_snapshot.filter(|(reference, payload, _)| {
            self.account_group_file_assets_ready(reference, payload)
        });
        let action_snapshot = self
            .group_operations
            .lock()
            .expect("operation journal mutex")
            .entries
            .get(&id)
            .filter(|entry| !entry.terminal() && !entry.cancel)
            .and_then(|entry| {
                entry
                    .body
                    .as_ref()
                    .and_then(AccountGroupOperation::message_action_input)
                    .map(|input| (entry.reference.group, input))
            });
        let action_target = match action_snapshot {
            Some((group, input)) => self.account_group_action_target(group, &input),
            None => Ok(None),
        };
        let needs_owner_proofs = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .get(&id)
            .and_then(|entry| entry.body.as_ref())
            .is_some_and(|body| body.owner_control_input().is_some());
        let owner_proofs = needs_owner_proofs.then(|| self.owner_control_proof_snapshot());
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("membership commit mutex");
        let mut delivery = self.delivery.lock().expect("delivery mutex");
        if self.runtime_work.is_closed()
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(NodeError::Authorization(
                "account source runtime is retired".into(),
            ));
        }
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let Some(mut entry) = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .get(&id)
            .cloned()
        else {
            return Ok(None);
        };
        if !entry.source || entry.terminal() {
            return Ok(None);
        }
        let legacy_child = self
            .group_operations
            .lock()
            .expect("operation journal mutex")
            .legacy_children
            .contains_key(&entry.reference.id);
        // The existing bounded data worker verifies the invitee capability and
        // seals optional history before entering these synchronous guards.
        if entry.reference.kind == AccountGroupOperationKind::Invite
            && invitation_assets.is_none()
            && !entry.cancel
        {
            return Ok(None);
        }
        if entry.cancel {
            entry.status =
                self.operation_status_for(&entry.reference, AccountGroupOperationPhase::Cancelled);
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put(entry)
                .map_err(NodeError::Log)?;
            return Ok(None);
        }
        let (action_target, rejection) = match action_target {
            Ok(target) => {
                let reason =
                    action_preparation::action_target_expired(target.as_ref(), self.expiry_now())
                        .then_some(AccountGroupOperationRejection::Expired);
                (target, reason)
            }
            Err(NodeError::InvalidInput(_)) => {
                (None, Some(AccountGroupOperationRejection::InvalidTarget))
            }
            Err(error) => return Err(error),
        };
        if let Some(reason) = rejection {
            entry.status = self.operation_status_for(
                &entry.reference,
                AccountGroupOperationPhase::Rejected { reason },
            );
            self.group_operations
                .lock()
                .expect("operation journal mutex")
                .put(entry)
                .map_err(NodeError::Log)?;
            return Ok(None);
        }
        let Some(body) = entry.body.as_ref() else {
            return Ok(None);
        };
        let access = self.account_group_access(entry.reference.group)?;
        if access.role == AccountGroupRole::Unknown {
            return Ok(None);
        }
        if access.sources.iter().any(|source| {
            source.device == self.identity.public()
                && source.execution == AccountGroupExecution::Reconciling
        }) {
            return Ok(None);
        }
        if !entry.reference.kind.admits_role(access.role)
            || access
                .sources
                .iter()
                .find(|source| source.device == self.identity.public())
                .is_none_or(|source| {
                    !entry.reference.kind.admits_role(source.role)
                        || source.lifetime != entry.reference.executor.lifetime
                        || source.authority != entry.reference.executor.authority
                            && !self.legacy_member_leave_source_matches(&entry.reference, source)
                })
        {
            entry.status = self.operation_status_for(
                &entry.reference,
                AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::MembershipChanged,
                },
            );
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put(entry)
                .map_err(NodeError::Log)?;
            return Ok(None);
        }
        if matches!(
            body,
            AccountGroupOperation::Send { .. } | AccountGroupOperation::Invite { .. }
        ) {
            self.process_channel(entry.reference.group);
        }
        // Match the ordinary sender's membership→delivery→book→log order and
        // reuse its current sender-key state; the public projection has no keys.
        let book = matches!(
            body,
            AccountGroupOperation::Send { .. } | AccountGroupOperation::Invite { .. }
        )
        .then(|| self.channels.lock().expect("channels mutex"));
        let log = self.log.lock().expect("log mutex");
        let projection = crate::node::channel::ChannelBook::project_membership(
            entry.reference.group,
            &log.events(&entry.reference.group),
        );
        // The pinned source and the prepared effect must refer to this same
        // canonical log view, including modern inherited account authority.
        let current_source = Self::public_account_group_source(
            entry.reference.group,
            &projection,
            &self.identity.public(),
            &self.own_group_proof(),
        );
        if current_source.is_none_or(|source| {
            !entry.reference.kind.admits_role(source.role)
                || source.device != entry.reference.executor.device
                || source.lifetime != entry.reference.executor.lifetime
                || source.authority != entry.reference.executor.authority
                    && !self.legacy_member_leave_source_matches(&entry.reference, &source)
        }) {
            drop(log);
            drop(book);
            entry.status = self.operation_status_for(
                &entry.reference,
                AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::MembershipChanged,
                },
            );
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .put(entry)
                .map_err(NodeError::Log)?;
            return Ok(None);
        }
        let prepared = match body {
            AccountGroupOperation::Send { message } => {
                if message
                    .expires_at_ms()
                    .is_some_and(|deadline| deadline <= self.expiry_now())
                {
                    drop(log);
                    drop(book);
                    entry.status = self.operation_status_for(
                        &entry.reference,
                        AccountGroupOperationPhase::Rejected {
                            reason: AccountGroupOperationRejection::Expired,
                        },
                    );
                    self.group_operations
                        .lock()
                        .expect("group operation journal mutex")
                        .put(entry)
                        .map_err(NodeError::Log)?;
                    return Ok(None);
                }
                let state = book
                    .as_ref()
                    .and_then(|book| book.state(&entry.reference.group))
                    .ok_or_else(|| {
                        NodeError::Channel("group sender state is reconciling".into())
                    })?;
                if !self.channel_identity_is_current_in(state, &projection)
                    || state.pending_invite()
                    || state.is_dissolved()
                    || projection.state.as_ref().is_none_or(|public| {
                        public.epoch() != state.epoch() || public.members() != state.members()
                    })
                {
                    return Err(NodeError::Channel(
                        "group sender state is reconciling".into(),
                    ));
                }
                if message.kind == AccountGroupOperationKind::File {
                    let (_, payload, ready) = file_snapshot.ok_or_else(|| {
                        NodeError::File("original File assets are reconciling".into())
                    })?;
                    let descriptor = payload
                        .descriptor
                        .as_ref()
                        .filter(|descriptor| {
                            descriptor.valid_for(&entry.reference, &payload.logical)
                        })
                        .ok_or_else(|| {
                            NodeError::Authorization("invalid original File preparation".into())
                        })?;
                    let (effect, id) = self.prepare_channel_outgoing_locked(
                        entry.reference.group,
                        descriptor.manifest.clone(),
                        crate::eventlog::EventKind::FileManifest,
                        &log,
                        state,
                    )?;
                    Some((
                        DeliveryTransaction::AccountGroupFile {
                            witness: Box::new(crate::node::delivery_store::GroupFileWitness {
                                own: self.own_group_proof(),
                                payload,
                                ready,
                            }),
                            transaction: Box::new(effect),
                        },
                        id,
                    ))
                } else {
                    Some(self.prepare_channel_outgoing_locked(
                        entry.reference.group,
                        message.encoded_body().to_vec(),
                        crate::eventlog::EventKind::Message,
                        &log,
                        state,
                    )?)
                }
            }
            AccountGroupOperation::Invite { .. } => {
                use crate::node::channels::account_invitation::{
                    InvitationPreparation, InvitationWitness,
                };
                let input = body.invitation_input().expect("selected invitation input");
                let assets = invitation_assets.take().ok_or_else(|| {
                    NodeError::Channel("invitation capability is reconciling".into())
                })?;
                let state = book
                    .as_ref()
                    .and_then(|book| book.state(&entry.reference.group))
                    .ok_or_else(|| {
                        NodeError::Channel("group sender state is reconciling".into())
                    })?;
                let (parents, _) = log
                    .prepare(&entry.reference.group)
                    .map_err(NodeError::Log)?;
                self.prepare_account_invitation_locked(InvitationPreparation {
                    group: entry.reference.group,
                    log: &log,
                    state,
                    projection: &projection,
                    input: &input,
                    witness: InvitationWitness {
                        own: self.own_group_proof(),
                        target: assets.target,
                        incarnation: assets.incarnation,
                        parents,
                        recipients: projection
                            .state
                            .as_ref()
                            .ok_or_else(|| {
                                NodeError::Channel("group authority is reconciling".into())
                            })?
                            .members()
                            .to_vec(),
                    },
                    history: assets.history,
                })?
                .map(|prepared| {
                    (
                        DeliveryTransaction::AccountInvitation {
                            input,
                            witness: prepared.witness,
                            transaction: Box::new(prepared.transaction),
                        },
                        prepared.invitation,
                    )
                })
            }
            _ if body.self_control_input().is_some() => {
                if legacy_child {
                    let prepared = self.prepare_legacy_member_leave_locked(
                        entry.reference.group,
                        &log,
                        &projection,
                        &self.own_group_proof(),
                    )?;
                    let id = prepared.event.id;
                    Some((
                        DeliveryTransaction::LegacyMemberLeave {
                            witness: prepared.witness,
                            events: vec![prepared.event],
                        },
                        id,
                    ))
                } else {
                    let input = body
                        .self_control_input()
                        .expect("selected account self-control");
                    let prepared = self.prepare_account_self_control_locked(
                        entry.reference.group,
                        &log,
                        &projection,
                        input,
                        &self.own_group_proof(),
                    )?;
                    let id = prepared.event.id;
                    Some((
                        DeliveryTransaction::AccountSelfControl {
                            input,
                            witness: prepared.witness,
                            events: vec![prepared.event],
                        },
                        id,
                    ))
                }
            }
            _ if body.message_action_input().is_some() => {
                let input = body
                    .message_action_input()
                    .expect("selected message action");
                let prepared = self.prepare_channel_message_action_locked(
                    entry.reference.group,
                    &log,
                    &projection,
                    &input,
                    crate::node::message_actions::MessageActionAdmission {
                        own: self.own_group_proof(),
                        target: action_target,
                        admitted_at_ms: self.expiry_now(),
                    },
                )?;
                let id = prepared.event.id;
                let effect = DeliveryTransaction::ChannelMessageAction {
                    input,
                    witness: prepared.witness,
                    events: vec![prepared.event],
                    recall: prepared.recall,
                };
                Some((effect, id))
            }
            _ if body.owner_control_input().is_some() => {
                let input = body.owner_control_input().expect("selected owner control");
                let Some(proofs) = owner_proofs.as_ref() else {
                    return Ok(None);
                };
                self.prepare_owner_control(
                    entry.reference.group,
                    &log,
                    &projection,
                    &input,
                    proofs,
                )?
                .map(|prepared| {
                    let id = prepared.event.id;
                    (
                        DeliveryTransaction::AccountOwnerControl {
                            input,
                            witness: prepared.witness,
                            events: vec![prepared.event],
                        },
                        id,
                    )
                })
            }
            _ => {
                let control = body.simple_control().ok_or_else(|| {
                    NodeError::InvalidInput("account group operation kind unavailable".into())
                })?;
                self.prepare_simple_channel_control(
                    entry.reference.group,
                    &log,
                    &projection,
                    control,
                )?
                .map(|effect| {
                    let id = effect.id;
                    (
                        DeliveryTransaction::ChannelControl {
                            events: vec![effect],
                        },
                        id,
                    )
                })
            }
        };
        // Noop completion also depends on a durable canonical parent view.
        log.sync().map_err(NodeError::Log)?;
        let Some((effect, message_id)) = prepared else {
            drop(log);
            drop(book);
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .complete(&entry.reference, &[])
                .map_err(NodeError::Log)?;
            return Ok(None);
        };
        let transaction = requested_effect(&entry.reference, effect, message_id);
        if matches!(
            transaction.body(),
            DeliveryTransaction::ChannelMessageAction { .. }
        ) {
            delivery
                .preflight_group_message_action(&transaction, &log, &self.identity)
                .map_err(NodeError::Log)?;
        }
        if matches!(
            transaction.body(),
            DeliveryTransaction::AccountSelfControl { .. }
        ) {
            delivery
                .preflight_group_self_control_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
        }
        if matches!(
            transaction.body(),
            DeliveryTransaction::LegacyMemberLeave { .. }
        ) {
            delivery
                .preflight_legacy_member_leave_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
        }
        if entry.reference.kind == AccountGroupOperationKind::Invite {
            delivery
                .preflight_group_invitation_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
        }
        if entry.reference.kind == AccountGroupOperationKind::File {
            delivery
                .preflight_group_file_transaction(&transaction, &log)
                .map_err(NodeError::Log)?;
        }
        drop(log);
        drop(book);
        delivery.begin(transaction).map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        Ok(Some(entry.reference.group))
    }
    #[cfg(test)]
    pub(in crate::node) fn import_group_operation_result_for_test(
        &self,
        result: ResultProjection,
        source_proof: &crate::discovery::Announce,
    ) -> Result<bool, NodeError> {
        let id = result.reference.id;
        self.import_group_operation_result(result, source_proof)?;
        Ok(self
            .group_operations
            .lock()
            .expect("operation journal mutex")
            .entries
            .get(&id)
            .is_some_and(|entry| entry.file_copy.is_some()))
    }

    fn import_group_operation_result(
        &self,
        result: ResultProjection,
        source_proof: &crate::discovery::Announce,
    ) -> Result<(), NodeError> {
        let body = {
            let journal = self
                .group_operations
                .lock()
                .expect("group operation journal mutex");
            if let Some(old) = journal.entries.get(&result.reference.id) {
                if old.source || !old.reference.same(&result.reference) || old.terminal() {
                    return Ok(());
                }
            } else if let Some(lookup) = journal.lookups.get(&result.reference.id)
                && (lookup.group != result.reference.group
                    || lookup.digest != result.reference.digest)
            {
                return Ok(());
            }
            journal
                .entries
                .get(&result.reference.id)
                .and_then(|entry| entry.body.clone())
                .or_else(|| {
                    journal
                        .lookups
                        .get(&result.reference.id)
                        .map(|lookup| lookup.body.clone())
                })
        };
        let file_copy = if result.reference.kind == AccountGroupOperationKind::File
            && result.status.phase == AccountGroupOperationPhase::Committed
        {
            if result.file_descriptor.is_none() && result.message_event.is_none() {
                None
            } else {
                let descriptor = result
                    .file_descriptor
                    .as_ref()
                    .filter(|_| result.message_event.is_some())
                    .ok_or_else(|| {
                        NodeError::Authorization("partial original File result metadata".into())
                    })?;
                let logical =
                    super::file_preparation::GroupFileLogical::from_manifest(&descriptor.manifest)
                        .ok_or_else(|| {
                            NodeError::Authorization("invalid original File metadata".into())
                        })?;
                let payload = super::file_preparation::GroupFilePayload {
                    logical,
                    descriptor: Some(descriptor.clone()),
                };
                Some(journal::GroupFileCopy {
                    descriptor: descriptor.clone(),
                    complete: self
                        .account_group_file_cached_assets_ready(&result.reference, &payload),
                })
            }
        } else {
            None
        };

        // Persist the original private body before terminal adoption trims it.
        // Failure retains the intent and causes this exact source frame to retry.
        self.import_account_group_message_copy(&result, body.as_ref(), source_proof)?;
        self.import_account_group_action_copy(&result, body.as_ref(), source_proof)?;
        let mut journal = self
            .group_operations
            .lock()
            .expect("group operation journal mutex");
        if let Some(old) = journal.entries.get(&result.reference.id)
            && (old.source || !old.reference.same(&result.reference) || old.terminal())
        {
            return Ok(());
        }
        journal
            .adopt(Entry {
                file_copy,
                reference: result.reference,
                body,
                status: result.status,
                source: false,
                cancel: false,
                sequence: 0,
            })
            .map_err(NodeError::Log)
    }
}

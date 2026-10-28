//! Frozen per-device legacy departure plans; public authority remains device-scoped.
use super::{
    journal::{Entry, LegacyLeavePlan, OperationReference},
    legacy_wire::{LegacyLegRequest, LegacyLegResult, LegacyOperationProjection},
    wire::{RequestProjection, ResultProjection},
    *,
};

impl Node {
    fn legacy_leave_admitted(&self) -> Result<(), NodeError> {
        if self.runtime_work.is_closed()
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(NodeError::Authorization(
                "legacy departure runtime retired".into(),
            ));
        }
        Ok(())
    }

    fn legacy_leave_write<T>(
        &self,
        write: impl FnOnce(&mut journal::OperationJournal) -> Result<T, crate::eventlog::LogError>,
    ) -> Result<T, NodeError> {
        let _delivery = self.delivery.lock().expect("delivery mutex");
        self.legacy_leave_admitted()?;
        let mut journal = self
            .group_operations
            .lock()
            .expect("operation journal mutex");
        write(&mut journal).map_err(NodeError::Log)
    }

    fn legacy_leave_entry(&self, reference: OperationReference) -> Entry {
        Entry {
            file_copy: None,
            status: self
                .operation_status_for(&reference, AccountGroupOperationPhase::AwaitingAcceptance),
            reference,
            body: Some(AccountGroupOperation::LeaveAccount),
            source: false,
            cancel: false,
            sequence: 0,
        }
    }

    /// Called before membership/delivery guards. A linked child always retains
    /// its own physical executor and never recursively becomes a coordinator.
    pub(super) fn try_execute_legacy_leave_coordinator(
        &self,
        id: AccountGroupOperationId,
    ) -> Result<bool, NodeError> {
        self.legacy_leave_admitted()?;
        let (mut parent, existing) = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            let Some(entry) = journal.entries.get(&id) else {
                return Ok(false);
            };
            if !entry.source
                || entry.reference.kind != AccountGroupOperationKind::LeaveAccount
                || entry.reference.executor.authority.authority.is_some()
                || journal.legacy_children.contains_key(&id)
            {
                return Ok(false);
            }
            if entry.terminal() {
                return Ok(true);
            }
            (entry.clone(), journal.legacy_plans.get(&id).cloned())
        };
        // The coordinator is an active consumer too: the existing throttled
        // query nonce gives every pending leg a turn even after its UI closes.
        let _ = self.account_group_operation_status(&self.account_id(), id)?;
        if parent.cancel && existing.is_none() {
            parent.status =
                self.operation_status_for(&parent.reference, AccountGroupOperationPhase::Cancelled);
            self.legacy_leave_write(|journal| journal.put(parent))?;
            return Ok(true);
        }
        let mut plan = match existing {
            Some(plan) => plan,
            None => {
                let _membership = self
                    .channel_membership_commit
                    .lock()
                    .expect("membership commit mutex");
                let sources = self.freeze_legacy_leave_sources(
                    parent.reference.group,
                    &parent.reference.executor,
                )?;
                let mut children = Vec::new();
                for mut source in sources {
                    source.execution = if source.device == self.identity.public() {
                        AccountGroupExecution::Local
                    } else {
                        AccountGroupExecution::ViaDevice(source.device.clone())
                    };
                    let child = OperationReference::new(
                        self,
                        self.new_account_group_operation_id(&self.account_id())?,
                        parent.reference.group,
                        &AccountGroupOperation::LeaveAccount,
                        source,
                    );
                    children.push(self.legacy_leave_entry(child));
                }
                let plan = LegacyLeavePlan {
                    parent: parent.reference.clone(),
                    legs: children
                        .iter()
                        .map(|entry| entry.reference.clone())
                        .collect(),
                    roll_forward: false,
                };
                self.legacy_leave_write(|journal| journal.put_legacy_plan(plan.clone(), children))?;
                plan
            }
        };
        let children =
            {
                let journal = self
                    .group_operations
                    .lock()
                    .expect("operation journal mutex");
                plan.legs
                    .iter()
                    .map(|leg| {
                        journal.entries.get(&leg.id).cloned().ok_or_else(|| {
                            NodeError::Channel("legacy departure child missing".into())
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            };
        if !plan.roll_forward
            && children
                .iter()
                .any(|entry| !entry.status.committed_events.is_empty())
        {
            plan.roll_forward = true;
            self.legacy_leave_write(|journal| journal.put_legacy_plan(plan.clone(), vec![]))?;
        }
        let cancelling = parent.cancel && !plan.roll_forward;
        // Cancellation is a request until all original children acknowledge it.
        // Any committed effect makes completion a durable roll-forward decision.
        for child in &children {
            if cancelling && !child.terminal() && !child.cancel {
                let mut changed = child.clone();
                changed.cancel = true;
                self.legacy_leave_write(|journal| journal.put(changed))?;
            }
            if plan.roll_forward
                && parent.cancel
                && child.status.phase == AccountGroupOperationPhase::Cancelled
                && child.status.committed_events.is_empty()
            {
                let reference = OperationReference::new(
                    self,
                    self.new_account_group_operation_id(&self.account_id())?,
                    parent.reference.group,
                    &AccountGroupOperation::LeaveAccount,
                    child.reference.executor.clone(),
                );
                let mut next = plan.clone();
                let leg = next
                    .legs
                    .iter_mut()
                    .find(|leg| leg.id == child.reference.id)
                    .expect("frozen child");
                *leg = reference.clone();
                self.legacy_leave_write(|journal| {
                    journal.put_legacy_plan(next, vec![self.legacy_leave_entry(reference)])
                })?;
                self.account_sync_notify.notify_one();
                return Ok(true);
            }
            if child.reference.executor.device == self.identity.public() && !child.terminal() {
                let request = RequestProjection {
                    reference: child.reference.clone(),
                    body: Some(AccountGroupOperation::LeaveAccount),
                    cancel: cancelling,
                    nonce: self.operation_query_nonce(parent.reference.id),
                };
                self.admit_source_group_request(request)?;
                self.execute_source_control(child.reference.id)?;
            }
        }
        let children = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            plan.legs
                .iter()
                .filter_map(|leg| journal.entries.get(&leg.id).cloned())
                .collect::<Vec<_>>()
        };
        let all_committed = children.iter().all(|entry| {
            entry.status.phase == AccountGroupOperationPhase::Committed
                && (!entry.status.committed_events.is_empty()
                    || self.legacy_leave_zero_result_allowed(&ResultProjection {
                        reference: entry.reference.clone(),
                        status: entry.status.clone(),
                        message_event: None,
                        file_descriptor: None,
                        recall_mark: None,
                    }))
        });
        let all_stopped = children.iter().all(Entry::terminal);
        let has_effect = children
            .iter()
            .any(|entry| !entry.status.committed_events.is_empty());
        if has_effect && !plan.roll_forward {
            plan.roll_forward = true;
            self.legacy_leave_write(|journal| journal.put_legacy_plan(plan.clone(), vec![]))?;
        }
        let rejected = children.iter().find_map(|entry| match &entry.status.phase {
            AccountGroupOperationPhase::Rejected { reason } => Some(*reason),
            AccountGroupOperationPhase::Cancelled if plan.roll_forward && !parent.cancel => {
                Some(AccountGroupOperationRejection::MembershipChanged)
            }
            _ => None,
        });
        parent.status.phase = if cancelling && all_stopped && !has_effect {
            AccountGroupOperationPhase::Cancelled
        } else if all_committed {
            AccountGroupOperationPhase::Committed
        } else if cancelling && !has_effect {
            AccountGroupOperationPhase::CancellationRequested
        } else if let Some(reason) = rejected {
            // A frozen authority rejection cannot heal by minting new IDs.
            AccountGroupOperationPhase::ActionRequired { reason }
        } else {
            AccountGroupOperationPhase::Executing
        };
        parent.status.executions = children
            .iter()
            .map(|entry| AccountGroupOperationExecution {
                device_id: entry.reference.executor.device.user_id(),
                phase: entry.status.phase.clone(),
                committed_events: entry.status.committed_events.clone(),
            })
            .collect();
        parent.status.committed_events = children
            .iter()
            .flat_map(|entry| entry.status.committed_events.iter().copied())
            .collect();
        parent.status.committed_events.sort_unstable();
        parent.status.committed_events.dedup();
        parent.status.primary_event = None;
        let previous = self
            .group_operations
            .lock()
            .expect("operation journal mutex")
            .entries
            .get(&id)
            .map(|entry| entry.status.clone());
        if previous.as_ref() != Some(&parent.status) {
            self.legacy_leave_write(|journal| journal.put(parent))?;
        }
        Ok(true)
    }

    /// Zero-effect acknowledgements require the exact physical incarnation to
    /// have a signed Left observation, rather than merely an unavailable route.
    pub(super) fn legacy_leave_zero_result_allowed(&self, result: &ResultProjection) -> bool {
        let reference = &result.reference;
        if reference.kind != AccountGroupOperationKind::LeaveAccount
            || reference.executor.authority.authority.is_some()
            || result.status.phase != AccountGroupOperationPhase::Committed
            || !result.status.committed_events.is_empty()
        {
            return false;
        }
        let projection = self.account_group_projection(reference.group);
        let Some(state) = projection.state.as_ref() else {
            return false;
        };
        if projection.authority.current_event().is_some()
            || state.members().contains(&reference.executor.device)
            || !projection
                .departures
                .iter()
                .any(|(device, _)| *device == reference.executor.device.user_id())
        {
            return false;
        }
        self.group_control_heads(reference.group)
            .into_iter()
            .any(|head| {
                head.valid(&self.account.public().ed25519_pub)
                    && self.account_group_binding_current(&head.proof)
                    && head.proof.public() == reference.executor.device
                    && head.access.source.role == AccountGroupRole::Left
                    && head.access.source.lifetime == reference.executor.lifetime
                    && head.access.source.authority.authority.is_none()
            })
    }

    pub(super) fn try_complete_retired_legacy_leave_child(
        &self,
        request: &RequestProjection,
    ) -> Result<bool, NodeError> {
        self.legacy_leave_admitted()?;
        let entry = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            if !journal.legacy_children.contains_key(&request.reference.id) {
                return Ok(false);
            }
            let Some(entry) = journal.entries.get(&request.reference.id) else {
                return Ok(false);
            };
            if !entry.reference.same(&request.reference)
                || request.reference.executor.device != self.identity.public()
                || !request.reference.valid(&self.account.public().ed25519_pub)
            {
                return Err(NodeError::Authorization(
                    "legacy child binding changed".into(),
                ));
            }
            if entry.terminal() {
                return Ok(false);
            }
            entry.clone()
        };
        let proof = self
            .group_control_heads(entry.reference.group)
            .into_iter()
            .find(|head| {
                head.proof.public() == self.identity.public()
                    && head.access.source.lifetime == entry.reference.executor.lifetime
            });
        let Some(observation) = self.account_group_local_observation(
            entry.reference.group,
            proof.as_ref().map(|proof| &proof.access),
        ) else {
            return Ok(false);
        };
        if observation.source.role != AccountGroupRole::Left
            || observation.source.lifetime != entry.reference.executor.lifetime
            || observation.source.authority.authority.is_some()
        {
            return Ok(false);
        }
        let mut entry = entry;
        entry.source = true;
        entry.status =
            self.operation_status_for(&entry.reference, AccountGroupOperationPhase::Committed);
        self.legacy_leave_write(|journal| journal.put(entry))?;
        Ok(true)
    }

    pub(in crate::node) fn legacy_leave_projection(
        &self,
        group: ConversationId,
    ) -> LegacyOperationProjection {
        let entries = {
            let journal = self
                .group_operations
                .lock()
                .expect("operation journal mutex");
            let mut entries = journal
                .entries
                .values()
                .filter_map(|entry| {
                    (entry.reference.group == group)
                        .then(|| {
                            journal
                                .legacy_children
                                .get(&entry.reference.id)
                                .cloned()
                                .map(|parent| (parent, entry.clone()))
                        })
                        .flatten()
                })
                .collect::<Vec<_>>();
            entries.sort_by_key(|(_, entry)| std::cmp::Reverse(entry.sequence));
            entries
        };
        let mut pending = entries
            .iter()
            .filter(|(parent, entry)| {
                parent.executor.device == self.identity.public()
                    && !entry.source
                    && !entry.terminal()
            })
            .collect::<Vec<_>>();
        let parents = pending
            .iter()
            .map(|(parent, _)| parent.id)
            .collect::<std::collections::BTreeSet<_>>();
        let counter = parents.into_iter().fold(0u64, |value, parent| {
            value.wrapping_add(u64::from_be_bytes(
                self.operation_query_nonce(parent)[8..]
                    .try_into()
                    .expect("nonce counter"),
            ))
        });
        if !pending.is_empty() {
            let start = (counter as usize).wrapping_mul(8) % pending.len();
            pending.rotate_left(start);
        }
        let requests = pending
            .into_iter()
            .take(8)
            .map(|(parent, entry)| LegacyLegRequest {
                parent: parent.clone(),
                request: RequestProjection {
                    reference: entry.reference.clone(),
                    body: Some(AccountGroupOperation::LeaveAccount),
                    cancel: entry.cancel,
                    nonce: self.operation_query_nonce(parent.id),
                },
            })
            .collect();
        let results = entries
            .into_iter()
            .filter(|(_, entry)| entry.source)
            .take(16)
            .map(|(parent, entry)| LegacyLegResult {
                parent,
                result: ResultProjection {
                    reference: entry.reference,
                    status: entry.status,
                    message_event: None,
                    file_descriptor: None,
                    recall_mark: None,
                },
            })
            .collect();
        LegacyOperationProjection { requests, results }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::{Announce, Roster},
        eventlog::{Event, EventKind},
        identity::{account::Account, device::DeviceIdentity},
        node::AccountDevice,
    };
    use std::sync::{Arc, Mutex};

    fn node(path: &std::path::Path, account: &Account) -> Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &path.join("events.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap()
    }

    #[test]
    fn local_cancel_ack_during_remote_commit_rolls_forward_without_action_required() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let coordinator = node(&dir.path().join("coordinator"), &account);
        let remote = node(&dir.path().join("remote"), &account);
        let owner = DeviceIdentity::generate();
        let owner_account = Account::generate();
        let group = new_channel_id();
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "durable cancellation versus remote commit".into(),
                members: vec![
                    owner.public(),
                    coordinator.identity.public(),
                    remote.identity.public(),
                ],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        for current in [&coordinator, &remote] {
            current
                .log
                .lock()
                .unwrap()
                .append_durable(genesis.clone())
                .unwrap();
            current.process_channel(group);
            for (public, certificate) in [
                (
                    coordinator.identity.public(),
                    account.certify(&coordinator.identity.public().ed25519_pub),
                ),
                (
                    remote.identity.public(),
                    account.certify(&remote.identity.public().ed25519_pub),
                ),
            ] {
                current
                    .account_devices
                    .lock()
                    .unwrap()
                    .insert(AccountDevice {
                        public,
                        certificate,
                    })
                    .unwrap();
            }
            for proof in [
                Announce::new_with_account(&owner, &owner_account, "offline owner", 1),
                coordinator.own_group_proof(),
                remote.own_group_proof(),
            ] {
                if proof.public() != current.identity.public() {
                    current.roster.lock().unwrap().update(
                        &proof,
                        "127.0.0.1".parse().unwrap(),
                        &current.user_id(),
                    );
                }
            }
        }
        let local_source = Node::public_account_group_source(
            group,
            &coordinator.account_group_projection(group),
            &coordinator.identity.public(),
            &coordinator.own_group_proof(),
        )
        .unwrap();
        let mut remote_source = Node::public_account_group_source(
            group,
            &remote.account_group_projection(group),
            &remote.identity.public(),
            &remote.own_group_proof(),
        )
        .unwrap();
        remote_source.execution = AccountGroupExecution::ViaDevice(remote.identity.public());
        let reference = |source| {
            OperationReference::new(
                &coordinator,
                coordinator
                    .new_account_group_operation_id(&account.account_id())
                    .unwrap(),
                group,
                &AccountGroupOperation::LeaveAccount,
                source,
            )
        };
        let parent = reference(local_source.clone());
        let local = reference(local_source);
        let other = reference(remote_source);
        let request = |reference: OperationReference| RequestProjection {
            reference,
            body: Some(AccountGroupOperation::LeaveAccount),
            cancel: false,
            nonce: [0; 16],
        };
        coordinator
            .admit_source_group_request(request(parent.clone()))
            .unwrap();
        {
            let mut journal = coordinator.group_operations.lock().unwrap();
            assert!(journal.entries[&parent.id].source);
            assert_eq!(
                journal.entries[&parent.id].status.phase,
                AccountGroupOperationPhase::Accepted
            );
            journal
                .put_legacy_plan(
                    LegacyLeavePlan {
                        parent: parent.clone(),
                        legs: vec![local.clone(), other.clone()],
                        roll_forward: false,
                    },
                    vec![
                        coordinator.legacy_leave_entry(local.clone()),
                        coordinator.legacy_leave_entry(other.clone()),
                    ],
                )
                .unwrap();
        }
        coordinator
            .admit_source_group_request(request(local.clone()))
            .unwrap();
        remote
            .group_operations
            .lock()
            .unwrap()
            .put_legacy_child(parent.clone(), remote.legacy_leave_entry(other.clone()))
            .unwrap();
        remote
            .admit_source_group_request(request(other.clone()))
            .unwrap();
        // Both cancellation records are real public SDK writes. The local flag
        // is durable but has not yet been acknowledged by its source executor.
        coordinator
            .cancel_account_group_operation(&account.account_id(), parent.id)
            .unwrap();
        coordinator
            .cancel_account_group_operation(&account.account_id(), local.id)
            .unwrap();
        assert_eq!(
            coordinator.group_operations.lock().unwrap().entries[&local.id]
                .status
                .phase,
            AccountGroupOperationPhase::CancellationRequested
        );
        // The other original physical member wins its actual WAL-backed leave
        // before the cancellation request reaches that independent source.
        remote.execute_source_control(other.id).unwrap();
        let remote_result = remote.group_operations.lock().unwrap().entries[&other.id]
            .status
            .clone();
        assert_eq!(remote_result.phase, AccountGroupOperationPhase::Committed);
        assert_eq!(remote_result.committed_events.len(), 1);
        let effect = remote
            .log
            .lock()
            .unwrap()
            .get(&remote_result.committed_events[0])
            .unwrap()
            .clone();
        assert!(effect.verify_signature());
        assert_eq!(effect.kind, EventKind::ChannelLeave);
        coordinator
            .log
            .lock()
            .unwrap()
            .append_durable(effect)
            .unwrap();
        coordinator.process_channel(group);
        coordinator
            .import_group_operation_result_for_test(
                ResultProjection {
                    reference: other.clone(),
                    status: remote_result,
                    message_event: None,
                    file_descriptor: None,
                    recall_mark: None,
                },
                &remote.own_group_proof(),
            )
            .unwrap();
        coordinator
            .try_execute_legacy_leave_coordinator(parent.id)
            .unwrap();
        {
            let journal = coordinator.group_operations.lock().unwrap();
            assert!(journal.legacy_plans[&parent.id].roll_forward);
            assert!(journal.entries[&parent.id].cancel);
            assert_eq!(
                journal.entries[&local.id].status.phase,
                AccountGroupOperationPhase::Cancelled
            );
            assert!(
                journal.entries[&local.id]
                    .status
                    .committed_events
                    .is_empty()
            );
            assert_eq!(
                journal.entries[&parent.id].status.phase,
                AccountGroupOperationPhase::Executing,
                "a newly acknowledged local cancellation is retried next turn, not a frozen authority rejection"
            );
        }
        coordinator
            .try_execute_legacy_leave_coordinator(parent.id)
            .unwrap();
        let replacement =
            coordinator.group_operations.lock().unwrap().legacy_plans[&parent.id].legs[0].clone();
        assert_ne!(replacement.id, local.id);
        assert_eq!(replacement.executor, local.executor);
        coordinator
            .try_execute_legacy_leave_coordinator(parent.id)
            .unwrap();
        let journal = coordinator.group_operations.lock().unwrap();
        assert_eq!(
            journal.entries[&parent.id].status.phase,
            AccountGroupOperationPhase::Committed
        );
        assert_eq!(journal.entries[&parent.id].status.committed_events.len(), 2);
        assert_eq!(
            journal.entries[&local.id].status.phase,
            AccountGroupOperationPhase::Cancelled
        );
        drop(journal);
        assert_eq!(
            coordinator
                .log
                .lock()
                .unwrap()
                .events(&group)
                .iter()
                .filter(|event| event.kind == EventKind::ChannelLeave
                    && event.author.user_id() == coordinator.user_id())
                .count(),
            1
        );
    }

    #[test]
    fn imported_cancel_race_retries_once_and_rejected_leg_keeps_original_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let coordinator = node(&dir.path().join("coordinator"), &account);
        let remote = node(&dir.path().join("remote"), &account);
        let owner = DeviceIdentity::generate();
        let group = new_channel_id();
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "cancel race".into(),
                members: vec![
                    owner.public(),
                    coordinator.identity.public(),
                    remote.identity.public(),
                ],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        coordinator
            .log
            .lock()
            .unwrap()
            .append_durable(genesis.clone())
            .unwrap();
        coordinator.process_channel(group);
        let local_source = Node::public_account_group_source(
            group,
            &coordinator.account_group_projection(group),
            &coordinator.identity.public(),
            &coordinator.own_group_proof(),
        )
        .unwrap();
        let mut remote_source = local_source.clone();
        remote_source.device = remote.identity.public();
        remote_source.lifetime.member = remote.identity.public();
        remote_source.execution = AccountGroupExecution::ViaDevice(remote.identity.public());
        let parent = OperationReference::new(
            &coordinator,
            coordinator
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            local_source.clone(),
        );
        let local = OperationReference::new(
            &coordinator,
            coordinator
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            local_source,
        );
        let cancelled = OperationReference::new(
            &coordinator,
            coordinator
                .new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            remote_source,
        );
        let mut parent_entry = coordinator.legacy_leave_entry(parent.clone());
        parent_entry.source = true;
        parent_entry.cancel = true;
        parent_entry.status =
            coordinator.operation_status_for(&parent, AccountGroupOperationPhase::Accepted);
        let mut cancelled_entry = coordinator.legacy_leave_entry(cancelled.clone());
        cancelled_entry.cancel = true;
        {
            let mut journal = coordinator.group_operations.lock().unwrap();
            journal.put(parent_entry).unwrap();
            journal
                .put_legacy_plan(
                    LegacyLeavePlan {
                        parent: parent.clone(),
                        legs: vec![local.clone(), cancelled.clone()],
                        roll_forward: false,
                    },
                    vec![
                        coordinator.legacy_leave_entry(local.clone()),
                        cancelled_entry,
                    ],
                )
                .unwrap();
        }
        let import = |reference: OperationReference, phase: AccountGroupOperationPhase| {
            let status = coordinator.operation_status_for(&reference, phase);
            coordinator
                .import_group_operation_result_for_test(
                    ResultProjection {
                        reference,
                        status,
                        message_event: None,
                        file_descriptor: None,
                        recall_mark: None,
                    },
                    &remote.own_group_proof(),
                )
                .unwrap();
        };
        import(cancelled.clone(), AccountGroupOperationPhase::Cancelled);
        assert!(
            !coordinator.group_operations.lock().unwrap().entries[&cancelled.id].cancel,
            "the real result importer clears the child-local flag"
        );
        let effect = Event::new(
            &coordinator.identity,
            group,
            1,
            vec![genesis.id],
            2,
            2,
            EventKind::ChannelLeave,
            vec![],
        );
        coordinator
            .log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        coordinator.process_channel(group);
        {
            let mut journal = coordinator.group_operations.lock().unwrap();
            let mut entry = journal.entries[&local.id].clone();
            entry.source = true;
            journal.put(entry).unwrap();
            journal.complete(&local, &[effect]).unwrap();
        }
        let before = coordinator.group_operations.lock().unwrap().entries.len();
        coordinator
            .try_execute_legacy_leave_coordinator(parent.id)
            .unwrap();
        let retry =
            coordinator.group_operations.lock().unwrap().legacy_plans[&parent.id].legs[1].clone();
        assert_ne!(retry.id, cancelled.id);
        assert_eq!(retry.executor, cancelled.executor);
        for _ in 0..4 {
            coordinator
                .try_execute_legacy_leave_coordinator(parent.id)
                .unwrap();
        }
        assert_eq!(
            coordinator.group_operations.lock().unwrap().entries.len(),
            before + 1
        );
        import(
            retry.clone(),
            AccountGroupOperationPhase::Rejected {
                reason: AccountGroupOperationRejection::MembershipChanged,
            },
        );
        for _ in 0..4 {
            coordinator
                .try_execute_legacy_leave_coordinator(parent.id)
                .unwrap();
        }
        let journal = coordinator.group_operations.lock().unwrap();
        assert_eq!(journal.entries.len(), before + 1);
        assert!(journal.legacy_plans[&parent.id].legs[1].same(&retry));
        assert_eq!(
            journal.entries[&parent.id].status.phase,
            AccountGroupOperationPhase::ActionRequired {
                reason: AccountGroupOperationRejection::MembershipChanged
            }
        );
    }

    #[test]
    fn canonical_freeze_waits_for_every_linked_physical_binding_and_rejects_retired_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = node(dir.path(), &account);
        let sibling = DeviceIdentity::generate();
        let owner = DeviceIdentity::generate();
        node.account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: sibling.public(),
                certificate: account.certify(&sibling.public().ed25519_pub),
            })
            .unwrap();
        let group = new_channel_id();
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "canonical legacy plan".into(),
                members: vec![owner.public(), node.identity.public(), sibling.public()],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        node.log.lock().unwrap().append_durable(genesis).unwrap();
        node.process_channel(group);
        let pinned = Node::public_account_group_source(
            group,
            &node.account_group_projection(group),
            &node.identity.public(),
            &node.own_group_proof(),
        )
        .unwrap();
        assert!(
            node.freeze_legacy_leave_sources(group, &pinned).is_err(),
            "a missing sibling proof cannot silently shrink the frozen plan"
        );
        node.roster.lock().unwrap().update(
            &Announce::new_with_account(&sibling, &account, "physical sibling", 9),
            "127.0.0.1".parse().unwrap(),
            &node.user_id(),
        );
        let sources = node.freeze_legacy_leave_sources(group, &pinned).unwrap();
        assert_eq!(sources.len(), 2);
        let mut stale = pinned.clone();
        stale.lifetime.membership = crate::eventlog::EventId::new([7; 32]);
        assert!(node.freeze_legacy_leave_sources(group, &stale).is_err());
        let id = node
            .new_account_group_operation_id(&node.account_id())
            .unwrap();
        let parent = OperationReference::new(
            &node,
            id,
            group,
            &AccountGroupOperation::LeaveAccount,
            pinned,
        );
        let mut entry = node.legacy_leave_entry(parent.clone());
        entry.source = true;
        entry.status.phase = AccountGroupOperationPhase::Accepted;
        node.group_operations.lock().unwrap().put(entry).unwrap();
        let before = node.group_operations.lock().unwrap().entries.len();
        node.runtime_work.close();
        assert!(node.try_execute_legacy_leave_coordinator(id).is_err());
        assert_eq!(node.group_operations.lock().unwrap().entries.len(), before);
        assert!(
            node.group_operations
                .lock()
                .unwrap()
                .legacy_plans
                .is_empty()
        );
    }
}

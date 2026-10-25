//! Source execution uses the existing delivery allocation and recovery owner.
use super::{
    journal::{self, Entry},
    wire::*,
    *,
};
use crate::node::delivery_store::{DeliveryTransaction, RequestRecord};
use std::{collections::BTreeSet, sync::Arc};

impl Node {
    #[cfg(test)]
    pub(in crate::node) async fn maintain_account_group_operations(self: &Arc<Self>) {
        for group in self.maintain_account_group_operation_effects() {
            let members = self.channel_members(group);
            self.distribute_channel(group, &members).await;
        }
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
        for (key, hash, frame) in self.account_group_operation_inputs() {
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
                    if self.admit_source_group_request(request).is_err() {
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
                self.import_group_operation_result(result);
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
        let ready: Vec<_> = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .entries
            .values()
            .filter(|entry| {
                entry.reference.executor.device == self.identity.public() && !entry.terminal()
            })
            .take(8)
            .cloned()
            .collect();
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
        // No host, journal, membership or delivery guard escapes local work.
        fanout.into_iter().map(ConversationId::new).collect()
    }
    fn operation_status_for(
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
    fn admit_source_group_request(&self, request: RequestProjection) -> Result<(), NodeError> {
        if request.reference.executor.device != self.identity.public()
            || !request.reference.valid(&self.account.public().ed25519_pub)
        {
            return Err(NodeError::Authorization(
                "invalid operation executor".into(),
            ));
        }
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("membership commit mutex");
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
                if request.cancel && !entry.terminal() {
                    entry.cancel = true;
                    entry.status.phase = AccountGroupOperationPhase::CancellationRequested;
                }
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
            if access.role != AccountGroupRole::Owner
                || source.is_none_or(|source| source.role != AccountGroupRole::Owner)
            {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::NotAuthorized,
                };
            } else if source.is_some_and(|source| {
                source.lifetime != request.reference.executor.lifetime
                    || source.authority != request.reference.executor.authority
            }) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::MembershipChanged,
                };
            }
            if request.body.as_ref().is_none_or(|body| {
                !body.valid_simple_control()
                    || body.kind() != request.reference.kind
                    || journal::operation_digest(request.reference.group, body)
                        != request.reference.digest
            }) {
                phase = AccountGroupOperationPhase::Rejected {
                    reason: AccountGroupOperationRejection::InvalidPayload,
                };
            }
        }
        let status = self.operation_status_for(&request.reference, phase);
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .put(Entry {
                reference: request.reference,
                body: request.body,
                status,
                source: true,
                cancel: request.cancel,
                sequence: 0,
            })
            .map_err(NodeError::Log)
    }
    fn execute_source_control(
        &self,
        id: AccountGroupOperationId,
    ) -> Result<Option<ConversationId>, NodeError> {
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("membership commit mutex");
        let mut delivery = self.delivery.lock().expect("delivery mutex");
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
        let Some(control) = entry
            .body
            .as_ref()
            .and_then(AccountGroupOperation::simple_control)
        else {
            return Ok(None);
        };
        let access = self.account_group_access(entry.reference.group)?;
        if access.role == AccountGroupRole::Unknown {
            return Ok(None);
        }
        if access.role != AccountGroupRole::Owner
            || access
                .sources
                .iter()
                .find(|source| source.device == self.identity.public())
                .is_none_or(|source| {
                    source.role != AccountGroupRole::Owner
                        || source.lifetime != entry.reference.executor.lifetime
                        || source.authority != entry.reference.executor.authority
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
            source.role != AccountGroupRole::Owner
                || source.device != entry.reference.executor.device
                || source.lifetime != entry.reference.executor.lifetime
                || source.authority != entry.reference.executor.authority
        }) {
            drop(log);
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
        let effect =
            self.prepare_simple_channel_control(entry.reference.group, &log, &projection, control)?;
        // Noop completion also depends on a durable canonical parent view.
        log.sync().map_err(NodeError::Log)?;
        drop(log);
        let Some(effect) = effect else {
            self.group_operations
                .lock()
                .expect("group operation journal mutex")
                .complete(&entry.reference, &[])
                .map_err(NodeError::Log)?;
            return Ok(None);
        };
        let now = crate::node::node::now_millis();
        let transaction = DeliveryTransaction::GroupRequested {
            operation: entry.reference.clone(),
            transaction: Box::new(DeliveryTransaction::Requested {
                request: RequestRecord {
                    request_id: *id.as_bytes(),
                    digest: entry.reference.digest,
                    destination: crate::node::MessageDestination::Channel(entry.reference.group),
                    message_id: effect.id,
                    accepted_at_ms: now,
                },
                transaction: Box::new(DeliveryTransaction::ChannelControl {
                    events: vec![effect],
                }),
            }),
        };
        delivery.begin(transaction).map_err(NodeError::Log)?;
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        Ok(Some(entry.reference.group))
    }
    fn import_group_operation_result(&self, result: ResultProjection) {
        let mut journal = self
            .group_operations
            .lock()
            .expect("group operation journal mutex");
        if let Some(old) = journal.entries.get(&result.reference.id) {
            if old.source || !old.reference.same(&result.reference) {
                return;
            }
            if old.terminal() {
                return;
            }
        } else if let Some(lookup) = journal.lookups.get(&result.reference.id)
            && (lookup.group != result.reference.group || lookup.digest != result.reference.digest)
        {
            return;
        }
        let body = journal
            .entries
            .get(&result.reference.id)
            .and_then(|entry| entry.body.clone())
            .or_else(|| {
                journal
                    .lookups
                    .get(&result.reference.id)
                    .map(|lookup| lookup.body.clone())
            });
        let _ = journal.adopt(Entry {
            reference: result.reference,
            body,
            status: result.status,
            source: false,
            cancel: false,
            sequence: 0,
        });
    }
}

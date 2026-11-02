//! A committed explicit send retains its original source provenance privately.
use super::*;
use crate::eventlog::{Event, EventKind};

impl Node {
    pub(super) fn account_group_committed_message_event(
        &self,
        reference: &journal::OperationReference,
        status: &AccountGroupOperationStatus,
    ) -> Result<Option<Event>, NodeError> {
        if status.phase != AccountGroupOperationPhase::Committed {
            return Ok(None);
        }
        let kind = match reference.kind {
            AccountGroupOperationKind::Text | AccountGroupOperationKind::Sticker => {
                EventKind::Message
            }
            AccountGroupOperationKind::File => EventKind::FileManifest,
            AccountGroupOperationKind::Reaction => EventKind::React,
            AccountGroupOperationKind::Recall => EventKind::Delete,
            _ => return Ok(None),
        };
        let Some(primary_event) = status.primary_event else {
            return Ok(None);
        };
        let event = self
            .log
            .lock()
            .expect("log mutex")
            .get(&primary_event)?
            .filter(|event| {
                event.conversation_id == reference.group
                    && event.kind == kind
                    && event.author.ed25519_pub() == &reference.executor.device.ed25519_pub
            });
        let Some(event) = event else {
            return Ok(None);
        };
        if reference.kind == AccountGroupOperationKind::File {
            // Projection also runs under the host's existing delivery guard
            // during admission preflight. Use durable lifecycle owners directly;
            // never reacquire delivery or infer retirement from cache contention.
            let suppressed = self
                .account_suppression
                .lock()
                .expect("suppression mutex")
                .contains(reference.group, event.id)?;
            let recalled = self
                .recalls
                .lock()
                .expect("recalls mutex")
                .contains(reference.group, event.id)?;
            if suppressed || recalled {
                return Ok(None);
            }
            let row = self
                .received_files
                .lock()
                .expect("received files mutex")
                .entry_in(&reference.group, event.id)?;
            let Some(row) = row else {
                return Ok(None);
            };
            if row.conversation != reference.group
                || row.from != reference.executor.device.user_id()
                || row.wall_clock != event.wall_clock
                || self.recipient_expired(&row.plaintext, event.id, event.wall_clock)
                || crate::node::files::eligible_manifest_row(&row, Some(&event), None).is_none()
            {
                return Ok(None);
            }
        }
        Ok(Some(event))
    }

    pub(super) fn account_group_committed_recall_mark(
        &self,
        reference: &journal::OperationReference,
        event: &Event,
    ) -> Result<Option<crate::node::recall::RecallMark>, NodeError> {
        if reference.kind != AccountGroupOperationKind::Recall || event.kind != EventKind::Delete {
            return Ok(None);
        }
        let Some(sealed) = crate::node::channel::SealedPayload::decode(&event.ciphertext) else {
            return Ok(None);
        };
        let Some(plaintext) = sealed.open(&self.identity, &self.identity.public().x25519_pub)
        else {
            return Ok(None);
        };
        let Some(payload) = crate::node::recall::DeletePayload::decode(&plaintext) else {
            return Ok(None);
        };
        let target = payload.target;
        if journal::operation_digest(reference.group, &AccountGroupOperation::Recall { target })
            != reference.digest
        {
            return Ok(None);
        }
        if let Some(mark) = self
            .recalls
            .lock()
            .expect("recalls mutex")
            .mark(reference.group, target)?
        {
            return Ok(Some(mark));
        }
        Ok(self
            .log
            .lock()
            .expect("log mutex")
            .get(&target)?
            .filter(|original| {
                original.conversation_id == reference.group && original.author == event.author
            })
            .map(|original| crate::node::recall::RecallMark {
                conversation: reference.group,
                target,
                wall_clock: original.wall_clock,
            }))
    }

    pub(super) fn import_account_group_action_copy(
        &self,
        result: &ResultProjection,
        body: Option<&AccountGroupOperation>,
        source_proof: &crate::discovery::Announce,
    ) -> Result<(), NodeError> {
        if result.status.phase != AccountGroupOperationPhase::Committed {
            return Ok(());
        }
        if result.reference.kind == AccountGroupOperationKind::Recall {
            if !result.valid_message_event()
                || body.is_some_and(|body| {
                    !matches!(body, AccountGroupOperation::Recall { .. })
                        || journal::operation_digest(result.reference.group, body)
                            != result.reference.digest
                })
            {
                return Err(NodeError::Authorization(
                    "invalid committed account recall proof".into(),
                ));
            }
            let mark = result
                .recall_mark
                .as_ref()
                .filter(|_| result.message_event.is_some())
                .ok_or_else(|| {
                    NodeError::Channel("committed account recall proof is reconciling".into())
                })?;
            // The source already committed the signed public effect. Persist its
            // exact existing tombstone before trimming the requester's intent.
            return self
                .recalls
                .lock()
                .expect("recalls mutex")
                .record(mark.conversation, mark.target, mark.wall_clock)
                .map_err(NodeError::Log);
        }
        let Some(
            body @ AccountGroupOperation::React {
                target,
                emoji,
                remove,
            },
        ) = body
        else {
            return Ok(());
        };
        if result.reference.kind != AccountGroupOperationKind::Reaction
            || journal::operation_digest(result.reference.group, body) != result.reference.digest
            || !result.valid_message_event()
        {
            return Err(NodeError::Authorization(
                "invalid committed account action proof".into(),
            ));
        }
        let event = result.message_event.as_ref().ok_or_else(|| {
            NodeError::Channel("committed account action proof is reconciling".into())
        })?;
        let source_certificate = source_proof
            .account_cert
            .as_ref()
            .filter(|certificate| {
                source_proof.verify()
                    && source_proof.public() == result.reference.executor.device
                    && certificate.verify()
                    && certificate.account_ed25519_pub == self.account.public().ed25519_pub
            })
            .cloned()
            .ok_or_else(|| {
                NodeError::Authorization("invalid account reaction source certificate".into())
            })?;
        self.my_dm_reactions
            .lock()
            .expect("reaction projection mutex")
            .record_channel_bound(
                result.reference.group,
                result.reference.executor.device.clone(),
                event.id,
                event.wall_clock,
                crate::node::reaction::ReactionPayload {
                    target: *target,
                    emoji: emoji.clone(),
                    remove: *remove,
                },
                source_certificate,
            )
            .map_err(NodeError::Log)
    }

    pub(super) fn import_account_group_message_copy(
        &self,
        result: &ResultProjection,
        body: Option<&AccountGroupOperation>,
        source_proof: &crate::discovery::Announce,
    ) -> Result<(), NodeError> {
        if result.reference.kind == AccountGroupOperationKind::File {
            if result.status.phase != AccountGroupOperationPhase::Committed {
                return Ok(());
            }
            if !result.valid_message_event() {
                return Err(NodeError::Authorization(
                    "invalid committed File proof".into(),
                ));
            }
            // Retired File receipts preserve immutable terminal status without
            // releasing a decryption key, installing a card or scheduling bytes.
            if result.file_descriptor.is_none() && result.message_event.is_none() {
                return Ok(());
            }
            let descriptor = result.file_descriptor.as_ref().ok_or_else(|| {
                NodeError::Channel("committed File descriptor is reconciling".into())
            })?;
            let event = result
                .message_event
                .as_ref()
                .ok_or_else(|| NodeError::Channel("committed File source is reconciling".into()))?;
            if let Some(body) = body {
                let AccountGroupOperation::Send { message } = body else {
                    return Err(NodeError::Authorization(
                        "invalid original File intent".into(),
                    ));
                };
                let payload =
                    super::file_preparation::GroupFilePayload::decode(message.encoded_body())
                        .filter(|payload| {
                            message.kind == AccountGroupOperationKind::File
                                && message.staged_assets == [payload.logical.checksum]
                        })
                        .ok_or_else(|| {
                            NodeError::Authorization("invalid original File intent".into())
                        })?;
                if journal::operation_digest(result.reference.group, body)
                    != result.reference.digest
                    || !descriptor.valid_for(&result.reference, &payload.logical)
                    || payload
                        .descriptor
                        .as_ref()
                        .is_some_and(|original| original != descriptor)
                {
                    return Err(NodeError::Authorization(
                        "committed File preparation changed".into(),
                    ));
                }
            }
            return self.import_explicit_account_group_file_card(
                &result.reference,
                event,
                descriptor,
                source_proof,
            );
        }
        let Some(AccountGroupOperation::Send { message }) = body else {
            return Ok(());
        };
        if result.status.phase != AccountGroupOperationPhase::Committed {
            return Ok(());
        }
        if !result.valid_message_event()
            || journal::operation_digest(result.reference.group, body.expect("retained send"))
                != result.reference.digest
            || message.kind != result.reference.kind
            || PreparedAccountGroupMessage::from_plaintext(
                message.kind,
                message.encoded_body.clone(),
            )
            .is_none()
        {
            return Err(NodeError::Authorization(
                "invalid committed account message proof".into(),
            ));
        }
        let event = result.message_event.as_ref().ok_or_else(|| {
            NodeError::Channel("committed account message proof is reconciling".into())
        })?;
        self.import_own_account_channel_copy(&crate::node::received_log::ReceivedEntry {
            event_id: event.id,
            conversation: result.reference.group,
            from: result.reference.executor.device.user_id(),
            wall_clock: event.wall_clock,
            plaintext: message.encoded_body.clone(),
        })
    }
}

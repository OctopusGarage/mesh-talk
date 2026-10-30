//! Shared channel message-action preparation and immutable replay evidence.
//!
//! Integration contract: callers snapshot expiry/protected-file metadata before
//! locking the public log, then retain membership and delivery allocation guards
//! through preparation and WAL admission. Recovery validates the saved causal
//! parent view and admission clock, never mutable caches or the current clock.
//! The existing per-member sealed-payload format remains unchanged.

use crate::{
    discovery::Announce,
    eventlog::{Author, ConversationId, Event, EventId, EventKind, persist::PersistentEventLog},
    identity::device::{DeviceIdentity, PublicIdentity},
    node::{
        AccountGroupOperation, AccountGroupRole, Node, NodeError,
        channel::{ChannelMembershipProjection, SealedPayload},
        reaction::ReactionPayload,
        recall::{DeletePayload, RECALL_WINDOW_MS, RecallMark},
    },
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum MessageActionInput {
    React {
        target: EventId,
        emoji: String,
        remove: bool,
    },
    Recall {
        target: EventId,
    },
}
impl MessageActionInput {
    pub(in crate::node) fn operation(&self) -> AccountGroupOperation {
        match self {
            Self::React {
                target,
                emoji,
                remove,
            } => AccountGroupOperation::React {
                target: *target,
                emoji: emoji.clone(),
                remove: *remove,
            },
            Self::Recall { target } => AccountGroupOperation::Recall { target: *target },
        }
    }
}

/// Metadata is captured from the existing source sidecars, before the log lock.
/// It is local encrypted WAL evidence, never a public authority claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct MessageActionTarget {
    pub original: Event,
    pub expires_at_ms: Option<u64>,
    pub protected_file: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MessageActionWitness {
    pub(in crate::node) own: Announce,
    pub(in crate::node) target: Option<MessageActionTarget>,
    pub(in crate::node) parents: Vec<EventId>,
    pub(in crate::node) admitted_at_ms: u64,
}
/// Source-owned metadata captured before taking the event-log lock.
pub(in crate::node) struct MessageActionAdmission {
    pub own: Announce,
    pub target: Option<MessageActionTarget>,
    pub admitted_at_ms: u64,
}
pub(in crate::node) struct PreparedMessageAction {
    pub event: Event,
    pub witness: MessageActionWitness,
    pub recall: Option<RecallMark>,
}

fn validate_recall_target(
    group: ConversationId,
    author: Author,
    target: &Event,
    protected_file: bool,
    expires_at_ms: Option<u64>,
    admitted_at_ms: u64,
) -> Result<(), &'static str> {
    if target.conversation_id != group {
        return Err("recall target belongs to another group");
    }
    if target.author != author {
        return Err("can only recall your own message");
    }
    if target.kind != EventKind::Message
        && !(target.kind == EventKind::FileManifest && protected_file)
    {
        return Err("not a message");
    }
    if admitted_at_ms.saturating_sub(target.wall_clock) > RECALL_WINDOW_MS {
        return Err("the 2-minute recall window has passed");
    }
    if expires_at_ms.is_some_and(|deadline| deadline <= admitted_at_ms) {
        return Err("recall target has expired");
    }
    Ok(())
}

/// Owns the one payload decision shared by preparation and saved-view replay.
/// React intentionally retains existing arbitrary target/emoji semantics.
pub(in crate::node) fn expected_payload(
    group: ConversationId,
    input: &MessageActionInput,
    witness: &MessageActionWitness,
) -> Result<(EventKind, Vec<u8>, Option<RecallMark>), NodeError> {
    match input {
        MessageActionInput::React {
            target,
            emoji,
            remove,
        } => {
            if witness.target.is_some() {
                return Err(NodeError::InvalidInput(
                    "unexpected reaction target witness".into(),
                ));
            }
            Ok((
                EventKind::React,
                ReactionPayload {
                    target: *target,
                    emoji: emoji.clone(),
                    remove: *remove,
                }
                .encode(),
                None,
            ))
        }
        MessageActionInput::Recall { target } => {
            let source = witness.target.as_ref().ok_or_else(|| {
                NodeError::InvalidInput("recall original event unavailable".into())
            })?;
            if source.original.id != *target
                || !source.original.is_canonical()
                || !source.original.verify_integrity()
                || !source.original.verify_signature()
            {
                return Err(NodeError::InvalidInput(
                    "invalid recall original event".into(),
                ));
            }
            validate_recall_target(
                group,
                Author::from_ed25519(witness.own.ed25519_pub),
                &source.original,
                source.protected_file,
                source.expires_at_ms,
                witness.admitted_at_ms,
            )
            .map_err(|reason| NodeError::Channel(reason.into()))?;
            Ok((
                EventKind::Delete,
                DeletePayload { target: *target }.encode(),
                Some(RecallMark {
                    conversation: group,
                    target: *target,
                    wall_clock: source.original.wall_clock,
                }),
            ))
        }
    }
}

/// The sender can open its own recipient entry; other entries retain the
/// existing ephemeral sealed-box crypto. Exact recipient membership is checked
/// independently. No plaintext is persisted in the action witness.
pub(in crate::node) fn validate_self_payload(
    identity: &DeviceIdentity,
    input: &MessageActionInput,
    witness: &MessageActionWitness,
    event: &Event,
    recipients: &[PublicIdentity],
) -> Result<(), NodeError> {
    let (kind, payload, _) = expected_payload(event.conversation_id, input, witness)?;
    let sealed = SealedPayload::decode(&event.ciphertext)
        .ok_or_else(|| NodeError::InvalidInput("invalid sealed message action".into()))?;
    if identity.public() != witness.own.public()
        || event.kind != kind
        || sealed.sender != identity.public().user_id()
        || sealed
            .entries
            .iter()
            .map(|entry| entry.user_id.clone())
            .collect::<Vec<_>>()
            != recipients
                .iter()
                .map(PublicIdentity::user_id)
                .collect::<Vec<_>>()
        || sealed
            .open(identity, &identity.public().x25519_pub)
            .as_deref()
            != Some(payload.as_slice())
    {
        return Err(NodeError::InvalidInput(
            "message action payload binding failed".into(),
        ));
    }
    Ok(())
}

impl Node {
    /// No locking, appending or I/O: the integration owner retains the immutable
    /// membership/log view and supplies the original source metadata snapshot.
    pub(in crate::node) fn prepare_channel_message_action_locked(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &ChannelMembershipProjection,
        input: &MessageActionInput,
        admission: MessageActionAdmission,
    ) -> Result<PreparedMessageAction, NodeError> {
        let MessageActionAdmission {
            own,
            target,
            admitted_at_ms,
        } = admission;
        let state = projection
            .state
            .as_ref()
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        let source =
            Self::public_account_group_source(group, projection, &self.identity.public(), &own)
                .ok_or_else(|| NodeError::Channel("not a channel member".into()))?;
        if !matches!(
            source.role,
            AccountGroupRole::Member | AccountGroupRole::Owner
        ) || own.public() != self.identity.public()
        {
            return Err(NodeError::Channel("not a channel member".into()));
        }
        if target
            .as_ref()
            .is_some_and(|target| log.get(&target.original.id) != Some(&target.original))
        {
            return Err(NodeError::Channel(
                "recall original event is reconciling".into(),
            ));
        }
        let (parents, lamport) = log.prepare(&group).map_err(NodeError::Log)?;
        let witness = MessageActionWitness {
            own,
            target,
            parents: parents.clone(),
            admitted_at_ms,
        };
        let (kind, plaintext, recall) = expected_payload(group, input, &witness)?;
        let recipients = state.recipient_members();
        let sealed = SealedPayload::seal(&self.identity, &recipients, &plaintext)
            .map_err(|error| NodeError::Channel(format!("message action seal failed: {error}")))?;
        let seq = log
            .next_sequence(
                &group,
                &Author::from_ed25519(self.identity.public().ed25519_pub),
            )
            .map_err(NodeError::Log)?;
        let event = Event::new(
            &self.identity,
            group,
            seq,
            parents,
            lamport,
            admitted_at_ms,
            kind,
            sealed.encode(),
        );
        if !super::session::event_fits_frame(&event) {
            return Err(NodeError::InvalidInput(
                "message action exceeds supported frame".into(),
            ));
        }
        validate_self_payload(&self.identity, input, &witness, &event, &recipients)?;
        Ok(PreparedMessageAction {
            event,
            witness,
            recall,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::{Author, ConversationId, Event, EventId, EventKind},
        identity::device::DeviceIdentity,
        node::recall::RECALL_WINDOW_MS,
    };

    fn target(identity: &DeviceIdentity, group: ConversationId, kind: EventKind) -> Event {
        Event::new(
            identity,
            group,
            1,
            vec![EventId::new([3; 32])],
            2,
            1_000,
            kind,
            vec![],
        )
    }

    #[test]
    fn recall_rejects_target_from_another_group() {
        let identity = DeviceIdentity::generate();
        let group = ConversationId::new([1; 32]);
        let event = target(&identity, ConversationId::new([2; 32]), EventKind::Message);
        assert!(validate_recall_target(group, event.author, &event, false, None, 1_001).is_err());
    }

    #[test]
    fn recall_rejects_a_different_device_author() {
        let identity = DeviceIdentity::generate();
        let other = DeviceIdentity::generate();
        let group = ConversationId::new([1; 32]);
        let event = target(&identity, group, EventKind::Message);
        let author = Author::from_ed25519(other.public().ed25519_pub);
        assert!(validate_recall_target(group, author, &event, false, None, 1_001).is_err());
    }

    #[test]
    fn recall_keeps_original_two_minute_window() {
        let identity = DeviceIdentity::generate();
        let group = ConversationId::new([1; 32]);
        let event = target(&identity, group, EventKind::Message);
        assert!(
            validate_recall_target(
                group,
                event.author,
                &event,
                false,
                None,
                1_000 + RECALL_WINDOW_MS
            )
            .is_ok()
        );
        assert!(
            validate_recall_target(
                group,
                event.author,
                &event,
                false,
                None,
                1_001 + RECALL_WINDOW_MS
            )
            .is_err()
        );
    }

    #[test]
    fn recall_rejects_expired_source_at_admission() {
        let identity = DeviceIdentity::generate();
        let group = ConversationId::new([1; 32]);
        let event = target(&identity, group, EventKind::Message);
        assert!(
            validate_recall_target(group, event.author, &event, false, Some(1_001), 1_001).is_err()
        );
    }

    #[test]
    fn recall_requires_validated_protection_for_file_manifests() {
        let identity = DeviceIdentity::generate();
        let group = ConversationId::new([1; 32]);
        let event = target(&identity, group, EventKind::FileManifest);
        assert!(validate_recall_target(group, event.author, &event, true, None, 1_001).is_ok());
        assert!(validate_recall_target(group, event.author, &event, false, None, 1_001).is_err());
    }
}

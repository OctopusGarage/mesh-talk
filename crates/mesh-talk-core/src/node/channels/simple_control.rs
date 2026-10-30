//! Prepare owner controls against the caller's locked membership projection.
use super::*;
use crate::channel::HistoryPolicy;
use crate::eventlog::persist::PersistentEventLog;
use crate::eventlog::{Author, Event};

pub(in crate::node) enum SimpleControl {
    Rename(String),
    HistoryPolicy(HistoryPolicy),
    Dissolve,
}

pub(in crate::node) fn normalize_simple_name(name: &str) -> Result<String, NodeError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NodeError::InvalidInput(
            "channel name cannot be empty".into(),
        ));
    }
    if name.len() > 16 * 1024 {
        return Err(NodeError::InvalidInput("channel name is too long".into()));
    }
    Ok(name.to_owned())
}

impl SimpleControl {
    /// Decode only the canonical payloads supported by this control owner.
    pub(in crate::node) fn from_event(event: &Event) -> Option<Self> {
        match event.kind {
            EventKind::ChannelRename => {
                let name = std::str::from_utf8(&event.ciphertext).ok()?;
                let normalized = normalize_simple_name(name).ok()?;
                (normalized == name).then_some(Self::Rename(normalized))
            }
            EventKind::ChannelHistoryPolicy => {
                let policy = super::super::join_history::decode_policy(&event.ciphertext)?;
                (super::super::join_history::encode_policy(policy) == event.ciphertext)
                    .then_some(Self::HistoryPolicy(policy))
            }
            EventKind::ChannelDissolve if event.ciphertext.is_empty() => Some(Self::Dissolve),
            _ => None,
        }
    }
}

impl Node {
    /// The caller owns membership, delivery and log guards. This prepares one
    /// signed effect without acquiring those locks or publishing any state.
    pub(in crate::node) fn prepare_simple_channel_control(
        &self,
        group: ConversationId,
        log: &PersistentEventLog,
        projection: &ChannelMembershipProjection,
        control: SimpleControl,
    ) -> Result<Option<Event>, NodeError> {
        let state = projection
            .state
            .as_ref()
            .filter(|state| state.id() == group)
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        if !self.channel_identity_can_manage_in(state, projection) || state.is_dissolved() {
            return Err(NodeError::Channel(
                "only the active channel owner can manage it".into(),
            ));
        }
        let (kind, payload) = match control {
            SimpleControl::Rename(name) => {
                let name = normalize_simple_name(&name)?;
                if state.name() == name {
                    return Ok(None);
                }
                (EventKind::ChannelRename, name.into_bytes())
            }
            SimpleControl::HistoryPolicy(policy) => {
                if super::super::join_history::policy(&log.events(&group)) == policy {
                    return Ok(None);
                }
                (
                    EventKind::ChannelHistoryPolicy,
                    super::super::join_history::encode_policy(policy),
                )
            }
            SimpleControl::Dissolve => (EventKind::ChannelDissolve, Vec::new()),
        };
        let seq = log
            .next_sequence(
                &group,
                &Author::from_ed25519(self.identity.public().ed25519_pub),
            )
            .map_err(NodeError::Log)?;
        let (parents, lamport) = log.prepare(&group).map_err(NodeError::Log)?;
        Ok(Some(Event::new(
            &self.identity,
            group,
            seq,
            parents,
            lamport,
            now_millis(),
            kind,
            payload,
        )))
    }
}

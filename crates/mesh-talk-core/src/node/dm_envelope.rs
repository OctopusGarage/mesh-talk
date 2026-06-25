//! The DM routing envelope for account-addressed messaging (multi-device). A DM's
//! sealed plaintext is `DmEnvelope = MAGIC ‖ bincode({ route, body })`, where `body`
//! is the inner [`crate::node::message::MessageBody`] bytes and `route` names the
//! sender/recipient ACCOUNTS (not devices). The receiver uses the route to file the
//! message under the right account conversation and to decide `from_me` (true when the
//! route's sender is its own account — i.e. a self-synced copy of its own send).
//!
//! Backward compatible: a sealed plaintext that does not start with the magic (a
//! legacy device-addressed message) is handled by the caller as a raw `MessageBody`.

use bincode::Options;
use serde::{Deserialize, Serialize};

/// Frames the envelope so it is distinguishable from a legacy raw `MessageBody`.
const DM_ENV_MAGIC: &[u8] = b"MTDE1";
/// Frames an account-addressed reaction (sealed plaintext of a `React` event).
const REACT_ENV_MAGIC: &[u8] = b"MTRE1";
/// Frames an account-addressed recall (sealed plaintext of a `Delete` event).
const RECALL_ENV_MAGIC: &[u8] = b"MTRC1";

/// Logical routing for an account-addressed DM: which account sent it and which
/// account it is addressed to. Both are 32-hex `account_id`s.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmRoute {
    pub sender_account: String,
    pub recipient_account: String,
}

/// The sealed-plaintext envelope: a route, a stable logical message id (shared by
/// every per-device copy of one logical send, so reactions/replies can target it
/// account-wide), and the inner `MessageBody` bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DmEnvelope {
    pub route: DmRoute,
    pub msg_id: [u8; 32],
    pub body: Vec<u8>,
}

impl DmEnvelope {
    pub fn new(
        sender_account: String,
        recipient_account: String,
        msg_id: [u8; 32],
        body: Vec<u8>,
    ) -> Self {
        DmEnvelope {
            route: DmRoute {
                sender_account,
                recipient_account,
            },
            msg_id,
            body,
        }
    }

    /// `DM_ENV_MAGIC ‖ bincode(self)` — the plaintext that gets sealed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DM_ENV_MAGIC.len() + 64);
        out.extend_from_slice(DM_ENV_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("dm envelope serializes"),
        );
        out
    }

    /// Recover an envelope from opened plaintext, or `None` if the bytes are not a
    /// framed envelope (a legacy device-addressed message — caller falls back).
    pub fn decode(bytes: &[u8]) -> Option<DmEnvelope> {
        let rest = bytes.strip_prefix(DM_ENV_MAGIC)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<DmEnvelope>(rest)
            .ok()
    }
}

/// An account-addressed reaction: routed like a DM, targeting a logical message id
/// ([`DmEnvelope::msg_id`]) so it aggregates across every device of an account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionEnvelope {
    pub route: DmRoute,
    pub target: [u8; 32],
    pub emoji: String,
    pub remove: bool,
}

impl ReactionEnvelope {
    pub fn new(
        sender_account: String,
        recipient_account: String,
        target: [u8; 32],
        emoji: String,
        remove: bool,
    ) -> Self {
        ReactionEnvelope {
            route: DmRoute {
                sender_account,
                recipient_account,
            },
            target,
            emoji,
            remove,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(REACT_ENV_MAGIC.len() + 64);
        out.extend_from_slice(REACT_ENV_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("reaction envelope serializes"),
        );
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<ReactionEnvelope> {
        let rest = bytes.strip_prefix(REACT_ENV_MAGIC)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<ReactionEnvelope>(rest)
            .ok()
    }
}

/// The sealed plaintext of an account-addressed `Delete` (recall) event: the route
/// (sender/recipient accounts) and the logical `msg_id` being recalled. Like
/// [`ReactionEnvelope`], it is fanned out to every device of both accounts and bound to
/// the authenticating peer on receipt, so only the message's own author can recall it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallEnvelope {
    pub route: DmRoute,
    pub target: [u8; 32],
}

impl RecallEnvelope {
    pub fn new(sender_account: String, recipient_account: String, target: [u8; 32]) -> Self {
        RecallEnvelope {
            route: DmRoute {
                sender_account,
                recipient_account,
            },
            target,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(RECALL_ENV_MAGIC.len() + 64);
        out.extend_from_slice(RECALL_ENV_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("recall envelope serializes"),
        );
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<RecallEnvelope> {
        let rest = bytes.strip_prefix(RECALL_ENV_MAGIC)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<RecallEnvelope>(rest)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let env = DmEnvelope::new(
            "alice".into(),
            "bob".into(),
            [7u8; 32],
            b"inner-body".to_vec(),
        );
        assert_eq!(DmEnvelope::decode(&env.encode()), Some(env));
    }

    #[test]
    fn legacy_plaintext_is_not_an_envelope() {
        assert_eq!(DmEnvelope::decode(b"just a MessageBody"), None);
        // Magic prefix but garbage body → not a valid envelope.
        let mut bytes = DM_ENV_MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF]);
        assert_eq!(DmEnvelope::decode(&bytes), None);
    }

    #[test]
    fn rejects_trailing_bytes() {
        let env = DmEnvelope::new("a".into(), "b".into(), [0u8; 32], b"x".to_vec());
        let mut bytes = env.encode();
        bytes.push(0xAB);
        assert_eq!(DmEnvelope::decode(&bytes), None);
    }
}

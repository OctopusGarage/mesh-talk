//! DM conversation identity derivation.

use crate::eventlog::event::ConversationId;
use crate::identity::device::PublicIdentity;
use sha2::{Digest, Sha256};

/// Domain separator for DM conversation-id derivation.
const DM_CONV_DOMAIN: &[u8] = b"mesh-talk-dm-conversation-v1";

/// The deterministic conversation id for the 1:1 DM between two peers — a hash of
/// the SORTED pair of Ed25519 keys, so both peers compute the same id regardless
/// of who is "a" and who is "b". A self-DM (`a == b`) yields a well-defined,
/// unique id (it hashes the key twice); it never collides with a two-party pair.
pub fn dm_conversation_id(a: &PublicIdentity, b: &PublicIdentity) -> ConversationId {
    let (lo, hi) = if a.ed25519_pub <= b.ed25519_pub {
        (a.ed25519_pub, b.ed25519_pub)
    } else {
        (b.ed25519_pub, a.ed25519_pub)
    };
    let mut hasher = Sha256::new();
    hasher.update(DM_CONV_DOMAIN);
    hasher.update(lo);
    hasher.update(hi);
    ConversationId::new(hasher.finalize().into())
}

/// Domain separator for account-conversation-id derivation.
const ACCOUNT_CONV_DOMAIN: &[u8] = b"mesh-talk-account-conversation-v1";

/// The deterministic conversation id for the logical 1:1 conversation between two
/// accounts — a hash of the SORTED pair of account ids, so both accounts compute the
/// same id. This keys account-level history; it is distinct from the per-device-pair
/// [`dm_conversation_id`] used for transport/crypto.
pub fn account_conversation_id(a: &str, b: &str) -> ConversationId {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_CONV_DOMAIN);
    hasher.update((lo.len() as u32).to_be_bytes());
    hasher.update(lo.as_bytes());
    hasher.update(hi.as_bytes());
    ConversationId::new(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn conversation_id_is_symmetric_and_deterministic() {
        let a = DeviceIdentity::generate().public();
        let b = DeviceIdentity::generate().public();
        let id = dm_conversation_id(&a, &b);
        assert_eq!(id, dm_conversation_id(&b, &a)); // order-independent → both peers agree
        assert_eq!(id, dm_conversation_id(&a, &b)); // deterministic
    }

    #[test]
    fn different_pairs_have_different_conversation_ids() {
        let a = DeviceIdentity::generate().public();
        let b = DeviceIdentity::generate().public();
        let c = DeviceIdentity::generate().public();
        assert_ne!(dm_conversation_id(&a, &b), dm_conversation_id(&a, &c));
    }

    #[test]
    fn account_conversation_id_is_symmetric_and_deterministic() {
        let id = account_conversation_id("alice", "bob");
        assert_eq!(id, account_conversation_id("bob", "alice"));
        assert_eq!(id, account_conversation_id("alice", "bob"));
    }

    #[test]
    fn account_conversation_id_is_unambiguous_across_boundary() {
        // Length-prefixing the low id prevents ("a","bc") colliding with ("ab","c").
        assert_ne!(
            account_conversation_id("a", "bc"),
            account_conversation_id("ab", "c")
        );
    }

    #[test]
    fn different_account_pairs_differ() {
        assert_ne!(
            account_conversation_id("alice", "bob"),
            account_conversation_id("alice", "carol")
        );
    }
}

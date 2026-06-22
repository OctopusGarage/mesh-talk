//! The canonical, content-addressed event. `id` is the SHA-256 hash of the
//! event's content (everything except `id` and `sig`); `sig` is the author's
//! Ed25519 signature over `id`. Hash-linked via `parents`, carrying a Lamport
//! clock and a per-author sequence number.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain separator for event-content hashing.
const EVENT_DOMAIN: &[u8] = b"mesh-talk-event-v1";

/// 32-byte content hash identifying an event.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventId([u8; 32]);

impl EventId {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

impl std::fmt::Debug for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "EventId({}…)", &self.to_hex()[..8])
    }
}

/// 32-byte opaque conversation identifier (a DM pair or a channel).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ConversationId([u8; 32]);

impl ConversationId {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// An event author, identified by their Ed25519 public key (self-certifying).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Author([u8; 32]);

impl Author {
    pub fn from_ed25519(ed25519_pub: [u8; 32]) -> Self {
        Self(ed25519_pub)
    }
    pub fn ed25519_pub(&self) -> &[u8; 32] {
        &self.0
    }
    /// The stable user-id fingerprint of this author (hex, 32 chars).
    pub fn user_id(&self) -> String {
        PublicIdentity::user_id_from(&self.0)
    }
}

impl std::fmt::Debug for Author {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Author({}…)", &hex::encode(self.0)[..8])
    }
}

/// The kind of an event. The payload itself lives (encrypted) in `ciphertext`.
///
/// The variant ORDER is part of the content-address wire format: serde/bincode
/// encode the positional index, so this enum is **append-only** — never reorder
/// or remove a variant; only add new ones at the end. The explicit discriminants
/// document the frozen indices; `event_kind_wire_encoding_is_stable` guards them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum EventKind {
    Message = 0,
    Edit = 1,
    Delete = 2,
    React = 3,
    ReadMarker = 4,
    MembershipChange = 5,
    KeyRotation = 6,
    FileManifest = 7,
    Profile = 8,
}

/// A single, content-addressed, signed log event.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Event {
    pub id: EventId,
    pub conversation_id: ConversationId,
    pub author: Author,
    pub seq: u64,
    pub parents: Vec<EventId>,
    pub lamport: u64,
    pub wall_clock: u64,
    pub kind: EventKind,
    pub ciphertext: Vec<u8>,
    pub sig: Vec<u8>,
}

/// The hashed content of an event (everything that fixes its identity). Held by
/// reference so we never copy the ciphertext to compute the id.
#[derive(Serialize)]
struct EventContent<'a> {
    conversation_id: &'a ConversationId,
    author: &'a Author,
    seq: u64,
    parents: &'a [EventId],
    lamport: u64,
    wall_clock: u64,
    kind: EventKind,
    ciphertext: &'a [u8],
}

fn hash_content(content: &EventContent) -> EventId {
    let bytes = bincode::serialize(content).expect("event content is serializable");
    let mut hasher = Sha256::new();
    hasher.update(EVENT_DOMAIN);
    hasher.update(&bytes);
    EventId(hasher.finalize().into())
}

/// The domain-separated message the author signs: `EVENT_DOMAIN || id`.
fn signing_input(id: &EventId) -> Vec<u8> {
    let mut input = Vec::with_capacity(EVENT_DOMAIN.len() + 32);
    input.extend_from_slice(EVENT_DOMAIN);
    input.extend_from_slice(id.as_bytes());
    input
}

impl Event {
    /// Build a fresh event: canonicalize parents, compute the content-hash id,
    /// and sign that id with the author's Ed25519 key.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        identity: &DeviceIdentity,
        conversation_id: ConversationId,
        seq: u64,
        mut parents: Vec<EventId>,
        lamport: u64,
        wall_clock: u64,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) -> Self {
        // Canonical parent order so the same logical event always hashes equal.
        parents.sort();
        parents.dedup();
        let author = Author::from_ed25519(identity.public().ed25519_pub);
        let id = hash_content(&EventContent {
            conversation_id: &conversation_id,
            author: &author,
            seq,
            parents: &parents,
            lamport,
            wall_clock,
            kind,
            ciphertext: &ciphertext,
        });
        let sig = identity.sign(&signing_input(&id)).to_vec();
        Event {
            id,
            conversation_id,
            author,
            seq,
            parents,
            lamport,
            wall_clock,
            kind,
            ciphertext,
            sig,
        }
    }

    /// Recompute the content-hash id from the event's own fields.
    pub fn recompute_id(&self) -> EventId {
        hash_content(&EventContent {
            conversation_id: &self.conversation_id,
            author: &self.author,
            seq: self.seq,
            parents: &self.parents,
            lamport: self.lamport,
            wall_clock: self.wall_clock,
            kind: self.kind,
            ciphertext: &self.ciphertext,
        })
    }

    /// True if `id` matches the hash of the content (tamper-evidence).
    /// Does not check parent canonicalization; see [`Event::is_canonical`].
    pub fn verify_integrity(&self) -> bool {
        self.id == self.recompute_id()
    }

    /// True if `sig` is a valid Ed25519 signature over `id` by `author`.
    pub fn verify_signature(&self) -> bool {
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(self.author.ed25519_pub(), &signing_input(&self.id), &sig)
    }

    /// True if `parents` is already canonical (sorted, de-duplicated) — the form
    /// [`Event::new`] produces. A validly-signed event with non-canonical parents
    /// hashes differently from its canonical equivalent (a phantom fork), so the
    /// store layer rejects non-canonical events on ingest.
    pub fn is_canonical(&self) -> bool {
        let mut expected = self.parents.clone();
        expected.sort();
        expected.dedup();
        self.parents == expected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv() -> ConversationId {
        ConversationId::new([1u8; 32])
    }

    #[test]
    fn id_is_deterministic_for_identical_content() {
        let id = DeviceIdentity::generate();
        let a = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"hi".to_vec(),
        );
        let b = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"hi".to_vec(),
        );
        // Same content (and Ed25519 is deterministic) → identical events.
        assert_eq!(a.id, b.id);
        assert_eq!(a, b);
    }

    #[test]
    fn id_changes_when_content_changes() {
        let id = DeviceIdentity::generate();
        let a = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"hi".to_vec(),
        );
        let b = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"bye".to_vec(),
        );
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn parents_are_order_independent() {
        let id = DeviceIdentity::generate();
        let p1 = EventId([7u8; 32]);
        let p2 = EventId([9u8; 32]);
        let a = Event::new(
            &id,
            conv(),
            2,
            vec![p1, p2],
            2,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let b = Event::new(
            &id,
            conv(),
            2,
            vec![p2, p1],
            2,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        assert_eq!(a.id, b.id);
        assert_eq!(a.parents, b.parents); // both sorted to the same order
    }

    #[test]
    fn signature_round_trips_and_rejects_tampering() {
        let id = DeviceIdentity::generate();
        let e = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi".to_vec(),
        );
        assert!(e.verify_signature());

        let mut tampered = e.clone();
        tampered.sig[0] ^= 0xFF;
        assert!(!tampered.verify_signature());
    }

    #[test]
    fn integrity_detects_content_mutation() {
        let id = DeviceIdentity::generate();
        let e = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi".to_vec(),
        );
        assert!(e.verify_integrity());

        let mut mutated = e.clone();
        mutated.ciphertext.push(0); // change content without recomputing id
        assert!(!mutated.verify_integrity());
    }

    #[test]
    fn author_user_id_matches_identity() {
        let id = DeviceIdentity::generate();
        let e = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi".to_vec(),
        );
        assert_eq!(
            e.author.user_id(),
            PublicIdentity::user_id_from(&id.public().ed25519_pub)
        );
    }

    #[test]
    fn event_kind_wire_encoding_is_stable() {
        // bincode encodes the positional variant index as a u32. These bytes are
        // part of every event's content-address; fail loudly if reordered.
        let cases = [
            (EventKind::Message, 0u32),
            (EventKind::Edit, 1),
            (EventKind::Delete, 2),
            (EventKind::React, 3),
            (EventKind::ReadMarker, 4),
            (EventKind::MembershipChange, 5),
            (EventKind::KeyRotation, 6),
            (EventKind::FileManifest, 7),
            (EventKind::Profile, 8),
        ];
        for (kind, index) in cases {
            assert_eq!(bincode::serialize(&kind).unwrap(), index.to_le_bytes());
        }
    }

    #[test]
    fn id_changes_when_metadata_changes() {
        let id = DeviceIdentity::generate();
        let base = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"x".to_vec(),
        );
        let diff_seq = Event::new(
            &id,
            conv(),
            2,
            vec![],
            1,
            100,
            EventKind::Message,
            b"x".to_vec(),
        );
        let diff_kind = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            100,
            EventKind::Edit,
            b"x".to_vec(),
        );
        let diff_conv = Event::new(
            &id,
            ConversationId::new([2u8; 32]),
            1,
            vec![],
            1,
            100,
            EventKind::Message,
            b"x".to_vec(),
        );
        assert_ne!(base.id, diff_seq.id);
        assert_ne!(base.id, diff_kind.id);
        assert_ne!(base.id, diff_conv.id);
    }

    #[test]
    fn different_author_produces_different_id() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let ea = Event::new(
            &a,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let eb = Event::new(
            &b,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        assert_ne!(ea.id, eb.id);
    }

    #[test]
    fn duplicate_parents_are_normalized() {
        let id = DeviceIdentity::generate();
        let p = EventId([4u8; 32]);
        let with_dup = Event::new(
            &id,
            conv(),
            1,
            vec![p, p],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        let without = Event::new(
            &id,
            conv(),
            1,
            vec![p],
            1,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        assert_eq!(with_dup.parents, vec![p]);
        assert_eq!(with_dup.id, without.id);
        assert!(with_dup.is_canonical());
    }

    #[test]
    fn is_canonical_rejects_non_canonical_parents() {
        let id = DeviceIdentity::generate();
        let p1 = EventId([3u8; 32]);
        let p2 = EventId([5u8; 32]);
        let mut e = Event::new(
            &id,
            conv(),
            2,
            vec![p1, p2],
            2,
            0,
            EventKind::Message,
            b"x".to_vec(),
        );
        assert!(e.is_canonical());
        // Hand-craft an unsorted, duplicate-bearing parents list.
        e.parents = vec![p2, p1, p1];
        assert!(!e.is_canonical());
    }

    #[test]
    fn verify_signature_rejects_wrong_length_sig() {
        let id = DeviceIdentity::generate();
        let mut e = Event::new(
            &id,
            conv(),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            b"hi".to_vec(),
        );
        e.sig = vec![0u8; 10];
        assert!(!e.verify_signature());
        e.sig = vec![];
        assert!(!e.verify_signature());
    }
}

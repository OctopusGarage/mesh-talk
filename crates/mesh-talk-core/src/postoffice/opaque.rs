//! Pairwise, opaque envelopes carried by the existing signed relay log.
//! The relay sees a pseudonymous mailbox and constant event headers, never
//! the signed inner event. Direct peer sync continues to use the inner event.

use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const DOMAIN: &[u8] = b"mesh-talk-post-office-opaque-v2";
const FEDERATED_DOMAIN: &[u8] = b"mesh-talk-post-office-opaque-v3";
const FEDERATED_ID_PREFIX: &[u8; 4] = b"MTF3";
const BUCKETS: [usize; 6] = [512, 2048, 8192, 32768, 57344, 61440];
const MAX_INNER_BYTES: usize = BUCKETS[5] - 4;
const _: () = assert!(BUCKETS[5] + 28 + 1024 < crate::transport::MAX_PLAINTEXT);

/// Cheap wire-shape gate for a production relay. It cannot prove that a client
/// actually encrypted the bytes, but blocks old clear-header event formats.
pub(crate) fn envelope_shape(event: &Event) -> bool {
    event.kind == EventKind::Message
        && event.parents.is_empty()
        && event.lamport == 0
        && event.wall_clock == 0
        && event.ciphertext.len() >= 28
        && BUCKETS.contains(&(event.ciphertext.len() - 28))
        && event.seq
            == u64::from_le_bytes(event.ciphertext[..8].try_into().expect("length checked"))
}

#[derive(Debug)]
pub enum MailboxError {
    InvalidPeer,
    InvalidEnvelope,
    TooLarge,
}

impl std::fmt::Display for MailboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for MailboxError {}

/// Both devices derive the same relay-specific mailbox from their static DH.
/// The relay sees a stable opaque mailbox and authenticated connection identities.
pub struct RelayMailbox {
    id: ConversationId,
    key: [u8; 32],
    nonce_key: [u8; 32],
    signer: DeviceIdentity,
}

impl RelayMailbox {
    pub fn new(
        me: &DeviceIdentity,
        other: &PublicIdentity,
        relay: &PublicIdentity,
    ) -> Result<Self, MailboxError> {
        Self::derive(me, other, Some(relay))
    }

    /// A pairwise mailbox whose ciphertext can be reconciled by any relay in
    /// the federation. The signed inner event remains readable only by peers.
    pub fn federated(me: &DeviceIdentity, other: &PublicIdentity) -> Result<Self, MailboxError> {
        Self::derive(me, other, None)
    }

    pub fn is_federated_id(id: &ConversationId) -> bool {
        id.as_bytes().starts_with(FEDERATED_ID_PREFIX)
    }

    fn derive(
        me: &DeviceIdentity,
        other: &PublicIdentity,
        relay: Option<&PublicIdentity>,
    ) -> Result<Self, MailboxError> {
        let me_public = me.public();
        if me_public == *other {
            return Err(MailboxError::InvalidPeer);
        }
        let mut secret = me.secret_bytes().1;
        let mut shared = StaticSecret::from(secret)
            .diffie_hellman(&PublicKey::from(other.x25519_pub))
            .to_bytes();
        secret.zeroize();
        if shared == [0; 32] {
            return Err(MailboxError::InvalidPeer);
        }
        let (first, second) = if (me_public.x25519_pub, me_public.ed25519_pub)
            < (other.x25519_pub, other.ed25519_pub)
        {
            (&me_public, other)
        } else {
            (other, &me_public)
        };
        let mut context = Vec::with_capacity(DOMAIN.len() + 160);
        context.extend_from_slice(if relay.is_some() {
            DOMAIN
        } else {
            FEDERATED_DOMAIN
        });
        context.extend_from_slice(&first.x25519_pub);
        context.extend_from_slice(&first.ed25519_pub);
        context.extend_from_slice(&second.x25519_pub);
        context.extend_from_slice(&second.ed25519_pub);
        if let Some(relay) = relay {
            context.extend_from_slice(&relay.ed25519_pub);
        }
        let hk = Hkdf::<Sha256>::new(Some(&context), &shared);
        shared.zeroize();
        let mut material = [0u8; 160];
        hk.expand(b"mailbox-keys", &mut material)
            .expect("fixed HKDF output length");
        let mut id_bytes: [u8; 32] = material[..32].try_into().unwrap();
        if relay.is_none() {
            id_bytes[..4].copy_from_slice(FEDERATED_ID_PREFIX);
        }
        let id = ConversationId::new(id_bytes);
        let key = material[32..64].try_into().unwrap();
        let nonce_key = material[64..96].try_into().unwrap();
        let signer = DeviceIdentity::from_secret_bytes(
            material[96..128].try_into().unwrap(),
            material[128..160].try_into().unwrap(),
        );
        material.zeroize();
        Ok(Self {
            id,
            key,
            nonce_key,
            signer,
        })
    }

    pub fn id(&self) -> ConversationId {
        self.id
    }

    fn nonce_for(&self, id: &EventId) -> [u8; 12] {
        let hk = Hkdf::<Sha256>::new(Some(&self.nonce_key), id.as_bytes());
        let mut nonce = [0; 12];
        hk.expand(b"event-nonce", &mut nonce)
            .expect("fixed HKDF output length");
        nonce
    }

    pub fn wrap(&self, inner: &Event) -> Result<Event, MailboxError> {
        if !inner.is_canonical() || !inner.verify_integrity() || !inner.verify_signature() {
            return Err(MailboxError::InvalidEnvelope);
        }
        let mut serialized =
            bincode::serialize(inner).map_err(|_| MailboxError::InvalidEnvelope)?;
        if serialized.len() > MAX_INNER_BYTES {
            serialized.zeroize();
            return Err(MailboxError::TooLarge);
        }
        let bucket = BUCKETS
            .iter()
            .copied()
            .find(|size| serialized.len() + 4 <= *size)
            .ok_or(MailboxError::TooLarge)?;
        let mut plaintext = vec![0u8; bucket];
        plaintext[..4].copy_from_slice(&(serialized.len() as u32).to_le_bytes());
        plaintext[4..4 + serialized.len()].copy_from_slice(&serialized);
        serialized.zeroize();
        let nonce = self.nonce_for(&inner.id);
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("valid key length");
        let aead_nonce = Nonce::try_from(nonce.as_slice()).expect("valid nonce length");
        let encrypted = cipher.encrypt(
            &aead_nonce,
            Payload {
                msg: &plaintext,
                aad: self.id.as_bytes(),
            },
        );
        plaintext.zeroize();
        let ciphertext = encrypted.map_err(|_| MailboxError::InvalidEnvelope)?;
        let mut body = Vec::with_capacity(12 + ciphertext.len());
        body.extend_from_slice(&nonce);
        body.extend_from_slice(&ciphertext);
        let seq = u64::from_le_bytes(nonce[..8].try_into().unwrap());
        Ok(Event::new(
            &self.signer,
            self.id,
            seq,
            vec![],
            0,
            0,
            EventKind::Message,
            body,
        ))
    }

    pub fn open(&self, outer: &Event) -> Result<Event, MailboxError> {
        if outer.conversation_id != self.id
            || outer.author != Author::from_ed25519(self.signer.public().ed25519_pub)
            || outer.kind != EventKind::Message
            || !outer.parents.is_empty()
            || outer.lamport != 0
            || outer.wall_clock != 0
            || outer.ciphertext.len() < 28
            || outer.ciphertext.len() > BUCKETS[5] + 28
            || !outer.verify_integrity()
            || !outer.verify_signature()
        {
            return Err(MailboxError::InvalidEnvelope);
        }
        let nonce: [u8; 12] = outer.ciphertext[..12].try_into().unwrap();
        if outer.seq != u64::from_le_bytes(nonce[..8].try_into().unwrap()) {
            return Err(MailboxError::InvalidEnvelope);
        }
        let cipher = Aes256Gcm::new_from_slice(&self.key).expect("valid key length");
        let aead_nonce = Nonce::try_from(nonce.as_slice()).expect("valid nonce length");
        let mut plaintext = cipher
            .decrypt(
                &aead_nonce,
                Payload {
                    msg: &outer.ciphertext[12..],
                    aad: self.id.as_bytes(),
                },
            )
            .map_err(|_| MailboxError::InvalidEnvelope)?;
        if !BUCKETS.contains(&plaintext.len()) {
            plaintext.zeroize();
            return Err(MailboxError::InvalidEnvelope);
        }
        let length = u32::from_le_bytes(
            plaintext[..4]
                .try_into()
                .map_err(|_| MailboxError::InvalidEnvelope)?,
        ) as usize;
        if length > MAX_INNER_BYTES
            || length + 4 > plaintext.len()
            || plaintext[4 + length..].iter().any(|byte| *byte != 0)
        {
            plaintext.zeroize();
            return Err(MailboxError::InvalidEnvelope);
        }
        let result: Result<Event, _> = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_INNER_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(&plaintext[4..4 + length]);
        plaintext.zeroize();
        let inner = result.map_err(|_| MailboxError::InvalidEnvelope)?;
        if self.nonce_for(&inner.id) != nonce
            || !inner.is_canonical()
            || !inner.verify_integrity()
            || !inner.verify_signature()
        {
            return Err(MailboxError::InvalidEnvelope);
        }
        Ok(inner)
    }
}

impl Drop for RelayMailbox {
    fn drop(&mut self) {
        self.key.zeroize();
        self.nonce_key.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};

    #[test]
    fn relay_envelope_is_stable_and_hides_inner_headers() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let event = Event::new(
            &alice,
            ConversationId::new([7; 32]),
            1,
            vec![],
            1,
            123_456_789,
            EventKind::MembershipChange,
            b"secret channel and member names".to_vec(),
        );
        let from_alice = RelayMailbox::new(&alice, &bob.public(), &relay.public()).unwrap();
        let from_bob = RelayMailbox::new(&bob, &alice.public(), &relay.public()).unwrap();
        let outer = from_alice.wrap(&event).unwrap();
        let slightly_longer = Event::new(
            &alice,
            event.conversation_id,
            2,
            vec![],
            2,
            123_456_790,
            EventKind::MembershipChange,
            b"secret channel and member names!".to_vec(),
        );
        assert_eq!(
            outer.ciphertext.len(),
            from_alice.wrap(&slightly_longer).unwrap().ciphertext.len()
        );
        assert_eq!(outer, from_alice.wrap(&event).unwrap());
        assert_eq!(outer.conversation_id, from_bob.id());
        assert_eq!(outer.kind, EventKind::Message);
        assert_eq!(outer.wall_clock, 0);
        assert_eq!(outer.parents, Vec::new());
        assert_ne!(outer.author.ed25519_pub(), &alice.public().ed25519_pub);
        assert_eq!(from_bob.open(&outer).unwrap(), event);
        let wire = bincode::serialize(&outer).unwrap();
        assert!(
            !wire
                .windows(b"secret channel and member names".len())
                .any(|w| w == b"secret channel and member names")
        );
        assert!(!wire.windows(32).any(|w| w == bob.public().ed25519_pub));
        assert!(!wire.windows(32).any(|w| w == bob.public().x25519_pub));
        assert!(
            RelayMailbox::new(&relay, &alice.public(), &bob.public())
                .unwrap()
                .open(&outer)
                .is_err()
        );
        let other_relay = DeviceIdentity::generate();
        assert!(
            RelayMailbox::new(&bob, &alice.public(), &other_relay.public())
                .unwrap()
                .open(&outer)
                .is_err()
        );
        let mut tampered = outer.clone();
        tampered.ciphertext[12] ^= 1;
        assert!(from_bob.open(&tampered).is_err());
        let mut forged_inner = event;
        forged_inner.sig[0] ^= 1;
        assert!(from_alice.wrap(&forged_inner).is_err());
    }

    #[test]
    fn mailbox_is_scoped_to_the_recipient_pair_and_relay() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let relay_a = DeviceIdentity::generate();
        let relay_b = DeviceIdentity::generate();
        let event = Event::new(
            &alice,
            ConversationId::new([9; 32]),
            7,
            vec![],
            42,
            123_456,
            EventKind::MembershipChange,
            b"private group membership".to_vec(),
        );

        let alice_bob = RelayMailbox::new(&alice, &bob.public(), &relay_a.public()).unwrap();
        let bob_alice = RelayMailbox::new(&bob, &alice.public(), &relay_a.public()).unwrap();
        let alice_carol = RelayMailbox::new(&alice, &carol.public(), &relay_a.public()).unwrap();
        let other_relay = RelayMailbox::new(&alice, &bob.public(), &relay_b.public()).unwrap();
        let wrapped = alice_bob.wrap(&event).unwrap();

        assert_eq!(alice_bob.id(), bob_alice.id());
        assert_ne!(alice_bob.id(), alice_carol.id());
        assert_ne!(alice_bob.id(), other_relay.id());
        assert_eq!(bob_alice.open(&wrapped).unwrap(), event);
        assert!(alice_carol.open(&wrapped).is_err());
        assert!(other_relay.open(&wrapped).is_err());
        assert!(envelope_shape(&wrapped));
        assert!(!envelope_shape(&event));
    }

    #[test]
    fn federated_mailbox_is_shared_across_relays_but_private_to_pair() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let relay_a = DeviceIdentity::generate();
        let relay_b = DeviceIdentity::generate();
        let event = Event::new(
            &alice,
            ConversationId::new([11; 32]),
            1,
            vec![],
            1,
            42,
            EventKind::Message,
            b"encrypted inner event".to_vec(),
        );
        let sent = RelayMailbox::federated(&alice, &bob.public()).unwrap();
        let received = RelayMailbox::federated(&bob, &alice.public()).unwrap();
        let other_pair = RelayMailbox::federated(&alice, &carol.public()).unwrap();
        let outer = sent.wrap(&event).unwrap();

        assert_eq!(sent.id(), received.id());
        assert_eq!(received.open(&outer).unwrap(), event);
        assert_ne!(sent.id(), other_pair.id());
        assert_ne!(
            sent.id(),
            RelayMailbox::new(&alice, &bob.public(), &relay_a.public())
                .unwrap()
                .id()
        );
        assert_ne!(
            sent.id(),
            RelayMailbox::new(&alice, &bob.public(), &relay_b.public())
                .unwrap()
                .id()
        );
        assert!(RelayMailbox::is_federated_id(&sent.id()));
        assert!(!RelayMailbox::is_federated_id(
            &RelayMailbox::new(&alice, &bob.public(), &relay_a.public())
                .unwrap()
                .id()
        ));
    }

    #[test]
    fn oversized_inner_event_cannot_bypass_the_relay_frame_limit() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mailbox = RelayMailbox::new(&alice, &bob.public(), &relay.public()).unwrap();
        let oversized = Event::new(
            &alice,
            ConversationId::new([3; 32]),
            1,
            vec![],
            1,
            1,
            EventKind::Message,
            vec![0; MAX_INNER_BYTES],
        );

        assert!(matches!(
            mailbox.wrap(&oversized),
            Err(MailboxError::TooLarge)
        ));
    }
}

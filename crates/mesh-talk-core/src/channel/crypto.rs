//! Channel (group) crypto primitive: a per-channel symmetric group key, sealed to
//! each member via the DM sealed-box, and raw-key AES-256-GCM for channel messages.
//! Pure crypto — no network, no state. Authorship of a channel message is proven by
//! the per-event Ed25519 signature (see `eventlog`), so a single shared group key
//! (not per-sender keys) suffices for v1; per-sender ratcheting is a later refinement.

use crate::dm::{self, DmError};
use crate::identity::device::DeviceIdentity;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use rand_core::OsRng;
use rand_core::RngCore;
use zeroize::Zeroize;

const KEY_SIZE: usize = 32;
const NONCE_SIZE: usize = 12;
/// AES-256-GCM authentication tag size — a valid sealed message is at least
/// `NONCE_SIZE + TAG_SIZE` bytes even for empty plaintext.
const TAG_SIZE: usize = 16;

/// A per-channel symmetric group key (AES-256-GCM). Generated fresh, distributed to
/// members via [`seal_group_key`], and rotated on membership change (by the caller).
/// Zeroized on drop (and deliberately not `Debug`) so the long-lived key does not
/// linger in freed memory — consistent with `SenderKey`/`RatchetState`.
#[derive(Clone)]
pub struct GroupKey([u8; KEY_SIZE]);

impl Drop for GroupKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl GroupKey {
    /// A fresh random group key.
    pub fn generate() -> Self {
        let mut key = [0u8; KEY_SIZE];
        OsRng.fill_bytes(&mut key);
        GroupKey(key)
    }

    /// Wrap raw key bytes (e.g. after opening a sealed key).
    pub fn from_bytes(bytes: [u8; KEY_SIZE]) -> Self {
        GroupKey(bytes)
    }

    /// The raw 32 key bytes.
    pub fn as_bytes(&self) -> &[u8; KEY_SIZE] {
        &self.0
    }
}

/// Errors from channel crypto.
#[derive(Debug)]
pub enum ChannelError {
    /// Sealing/opening the group key (via the DM sealed-box) failed.
    Seal(DmError),
    /// AES-256-GCM message encryption failed.
    Encrypt,
    /// AES-256-GCM message decryption failed (wrong key or tampered).
    Decrypt,
    /// A malformed envelope (bad length / structure).
    Malformed(String),
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Seal(e) => write!(f, "channel key seal error: {e}"),
            ChannelError::Encrypt => write!(f, "channel message encrypt error"),
            ChannelError::Decrypt => write!(f, "channel message decrypt error"),
            ChannelError::Malformed(m) => write!(f, "malformed channel envelope: {m}"),
        }
    }
}

impl std::error::Error for ChannelError {}

impl From<DmError> for ChannelError {
    fn from(e: DmError) -> Self {
        ChannelError::Seal(e)
    }
}

/// Seal `key` for a member: encrypt the raw key bytes to their X25519 key using the
/// DM sealed-box. Only that member can open it; the post office never can.
pub fn seal_group_key(
    sender: &DeviceIdentity,
    recipient_x25519_pub: &[u8; KEY_SIZE],
    key: &GroupKey,
) -> Result<Vec<u8>, ChannelError> {
    dm::seal(sender, recipient_x25519_pub, key.as_bytes()).map_err(ChannelError::Seal)
}

/// Open a sealed group key addressed to us (the inverse of [`seal_group_key`]).
pub fn open_group_key(
    recipient: &DeviceIdentity,
    sender_x25519_pub: &[u8; KEY_SIZE],
    envelope: &[u8],
) -> Result<GroupKey, ChannelError> {
    let bytes = dm::open(recipient, sender_x25519_pub, envelope).map_err(ChannelError::Seal)?;
    let arr: [u8; KEY_SIZE] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| ChannelError::Malformed("group key is not 32 bytes".into()))?;
    Ok(GroupKey(arr))
}

/// Encrypt a channel message with the group key: `nonce ‖ AES-256-GCM(key, nonce,
/// plaintext)`, with a fresh random 96-bit nonce per message.
pub fn seal_channel_message(key: &GroupKey, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| ChannelError::Encrypt)?;
    let mut nonce = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext)
        .map_err(|_| ChannelError::Encrypt)?;
    let mut out = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt a channel message produced by [`seal_channel_message`]. Returns
/// `ChannelError::Decrypt` for the wrong key or a tampered ciphertext.
pub fn open_channel_message(key: &GroupKey, envelope: &[u8]) -> Result<Vec<u8>, ChannelError> {
    if envelope.len() < NONCE_SIZE + TAG_SIZE {
        return Err(ChannelError::Malformed(
            "channel envelope too short (need nonce + tag)".into(),
        ));
    }
    let (nonce, ciphertext) = envelope.split_at(NONCE_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).map_err(|_| ChannelError::Decrypt)?;
    cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .map_err(|_| ChannelError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_key_seals_to_a_member_and_opens() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let key = GroupKey::generate();

        let sealed = seal_group_key(&alice, &bob.public().x25519_pub, &key).unwrap();
        let opened = open_group_key(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.as_bytes(), key.as_bytes());
    }

    #[test]
    fn a_non_member_cannot_open_the_group_key() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let key = GroupKey::generate();

        let sealed = seal_group_key(&alice, &bob.public().x25519_pub, &key).unwrap();
        assert!(open_group_key(&carol, &alice.public().x25519_pub, &sealed).is_err());
    }

    #[test]
    fn channel_message_round_trips_with_the_group_key() {
        let key = GroupKey::generate();
        let sealed = seal_channel_message(&key, b"hello channel").unwrap();
        let opened = open_channel_message(&key, &sealed).unwrap();
        assert_eq!(opened, b"hello channel");
    }

    #[test]
    fn a_different_group_key_cannot_decrypt() {
        let key = GroupKey::generate();
        let other = GroupKey::generate();
        let sealed = seal_channel_message(&key, b"secret").unwrap();
        assert!(open_channel_message(&other, &sealed).is_err());
    }

    #[test]
    fn a_tampered_channel_message_fails_to_open() {
        let key = GroupKey::generate();
        let mut sealed = seal_channel_message(&key, b"secret").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;
        assert!(open_channel_message(&key, &sealed).is_err());
    }

    #[test]
    fn a_tampered_nonce_fails_to_open() {
        // Corrupting the prepended nonce must also fail authentication (guards
        // against a future nonce/ciphertext ordering mistake).
        let key = GroupKey::generate();
        let mut sealed = seal_channel_message(&key, b"secret").unwrap();
        sealed[0] ^= 0xFF; // flip the first nonce byte
        assert!(open_channel_message(&key, &sealed).is_err());
    }

    #[test]
    fn two_seals_of_the_same_plaintext_differ() {
        let key = GroupKey::generate();
        let a = seal_channel_message(&key, b"same").unwrap();
        let b = seal_channel_message(&key, b"same").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn a_short_envelope_is_malformed() {
        let key = GroupKey::generate();
        assert!(matches!(
            open_channel_message(&key, &[0u8; 4]),
            Err(ChannelError::Malformed(_))
        ));
    }

    #[test]
    fn opening_a_non_key_sized_payload_is_malformed() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let sealed = dm::seal(&alice, &bob.public().x25519_pub, b"not a key").unwrap();
        assert!(matches!(
            open_group_key(&bob, &alice.public().x25519_pub, &sealed),
            Err(ChannelError::Malformed(_))
        ));
    }
}

//! Channel sender-key ratchet: per-sender symmetric chains for forward-secret group
//! messages. Reuses the DM ratchet's chain step + message-key derivation; there is no
//! DH ratchet (sender keys rotate wholesale on membership change). A sender advances
//! its own chain per message (deleting old keys); each receiver tracks that sender's
//! chain and ratchets forward to a message's position, buffering bounded skipped keys.

use crate::ratchet::{kdf_ck, message_keys};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use rand_core::OsRng;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use zeroize::{Zeroize, Zeroizing};

mod checkpoint;

/// Max messages a receiver will skip forward in one sender's chain (DoS bound) and
/// the total buffered skipped keys.
pub(crate) const MAX_SKIP: u32 = 1000;
const MAX_SKIPPED_TOTAL: usize = 2000;

#[derive(Debug)]
pub enum SenderKeyError {
    Encrypt,
    Decrypt,
    TooManySkipped,
    Malformed(String),
}

/// A member's SENDING chain for one channel epoch.
pub struct SenderKey {
    chain_key: [u8; 32],
    n: u32,
}

impl Drop for SenderKey {
    fn drop(&mut self) {
        self.chain_key.zeroize();
    }
}

impl SenderKey {
    /// A fresh sender key (new chain, position 0).
    pub fn generate() -> Self {
        let mut chain_key = [0u8; 32];
        OsRng.fill_bytes(&mut chain_key);
        SenderKey { chain_key, n: 0 }
    }

    /// The distribution snapshot to seal to members so they can follow this chain
    /// from the CURRENT position. (Distribute right after `generate`, before sending,
    /// so members start at n=0.)
    pub fn distribution(&self) -> SenderKeyDistribution {
        SenderKeyDistribution {
            chain_key: self.chain_key,
            n: self.n,
        }
    }

    /// Advance the chain by one message: returns `(n, message_key)` and deletes the
    /// old chain key (forward secrecy).
    ///
    /// Legacy infallible boundary: panics before changing state when the u32
    /// position is exhausted. Runtime callers use [`Self::try_ratchet`] instead.
    pub fn ratchet(&mut self) -> (u32, [u8; 32]) {
        self.try_ratchet().expect("sender chain exhausted")
    }

    /// Advance only when the next position is representable. Exhaustion leaves
    /// the chain unchanged; it never wraps or creates a replacement epoch.
    pub fn try_ratchet(&mut self) -> Result<(u32, [u8; 32]), SenderKeyError> {
        let next_n = self.n.checked_add(1).ok_or_else(exhausted)?;
        let (next, mk) = kdf_ck(&self.chain_key);
        let next = Zeroizing::new(next);
        let n = self.n;
        self.chain_key.zeroize();
        self.chain_key = *next;
        self.n = next_n;
        Ok((n, mk))
    }

    /// Serialize this sending chain (fixint bincode). The output is SECRET — callers
    /// MUST store it encrypted at rest.
    pub fn serialize(&self) -> Vec<u8> {
        let wire = SenderKeyWire {
            chain_key: self.chain_key,
            n: self.n,
        };
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&wire)
            .expect("sender key serializes")
    }

    /// Reconstruct a sending chain from [`serialize`] output. `None` if malformed.
    /// Strict parse (`reject_trailing_bytes`): the serialized sender key is secret
    /// chain-key material stored encrypted at rest; it must STAY strict, and grows
    /// only via a new version, never by tolerating extra bytes.
    pub fn deserialize(bytes: &[u8]) -> Option<SenderKey> {
        let wire: SenderKeyWire = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()?;
        Some(SenderKey {
            chain_key: wire.chain_key,
            n: wire.n,
        })
    }
}

/// Serialization mirror (private — used only inside serialize/deserialize).
#[derive(Serialize, Deserialize)]
struct SenderKeyWire {
    chain_key: [u8; 32],
    n: u32,
}
impl Drop for SenderKeyWire {
    fn drop(&mut self) {
        self.chain_key.zeroize();
    }
}

/// The sealed-per-member initial sender chain (chain key + starting position).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderKeyDistribution {
    pub chain_key: [u8; 32],
    pub n: u32,
}
impl Zeroize for SenderKeyDistribution {
    fn zeroize(&mut self) {
        self.chain_key.zeroize();
        self.n.zeroize();
    }
}

fn exhausted() -> SenderKeyError {
    SenderKeyError::Malformed("sender chain exhausted".into())
}

impl SenderKeyDistribution {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("skd serializes")
    }
    /// Strict parse (`reject_trailing_bytes`): the distribution carries the secret
    /// chain key sealed to members; it stays strict. As a bincode (positional) type
    /// it cannot grow in place — evolution is via a new version, not trailing bytes.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// A receiver's view of one sender's chain.
pub struct SenderChain {
    chain_key: [u8; 32],
    n: u32,
    skipped: HashMap<u32, [u8; 32]>,
    order: VecDeque<u32>,
}

/// Crypto preparation only: not a durable receive transaction. Dropping an
/// abandoned candidate wipes its chain and plaintext through their owners.
pub(super) struct PreparedReceiverOpen {
    next: SenderChain,
    plaintext: Zeroizing<Vec<u8>>,
}
type OpenCheckpoint = (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>, u32);

impl PreparedReceiverOpen {
    pub(super) fn into_checkpoint(self) -> Result<OpenCheckpoint, SenderKeyError> {
        Ok((self.next.encode_checkpoint()?, self.plaintext, self.next.n))
    }
    pub(super) fn install(self, chain: &mut SenderChain) -> Vec<u8> {
        let Self {
            next,
            mut plaintext,
        } = self;
        *chain = next;
        std::mem::take(&mut *plaintext)
    }
}

impl Drop for SenderChain {
    fn drop(&mut self) {
        self.chain_key.zeroize();
        for mk in self.skipped.values_mut() {
            mk.zeroize();
        }
    }
}

impl SenderChain {
    pub(super) fn prepare_open(
        &self,
        target: u32,
        ciphertext: &[u8],
        aad: &[u8],
    ) -> Result<PreparedReceiverOpen, SenderKeyError> {
        // The bounded local codec is also the exact private fork: it includes
        // consumed holes and eviction order, unlike a distribution snapshot.
        let mut next = Self::decode_checkpoint(&self.encode_checkpoint()?)?;
        let key = Zeroizing::new(next.message_key(target)?);
        let plaintext = Zeroizing::new(open_message(&key, ciphertext, aad)?);
        Ok(PreparedReceiverOpen { next, plaintext })
    }
    /// Recover a gap the normal ratchet cannot cross. Routine out-of-order
    /// messages keep their original receive position and this device's buffered
    /// skipped keys. Only an authenticated account snapshot may jump a stalled
    /// receiver; its own unconsumed skipped keys survive and consumed keys do not.
    pub(crate) fn refresh_distribution(&mut self, skd: &SenderKeyDistribution) {
        if skd.n.saturating_sub(self.n) > MAX_SKIP {
            self.chain_key.zeroize();
            self.chain_key = skd.chain_key;
            self.n = skd.n;
        }
    }

    pub fn from_distribution(skd: &SenderKeyDistribution) -> Self {
        SenderChain {
            chain_key: skd.chain_key,
            n: skd.n,
            skipped: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    /// A receive-only bootstrap starts at the current chain position. Consumed
    /// positions and buffered skipped keys remain private to the original device.
    pub(crate) fn current_distribution(&self) -> SenderKeyDistribution {
        SenderKeyDistribution {
            chain_key: self.chain_key,
            n: self.n,
        }
    }

    /// The message key for position `target`, ratcheting forward (buffering skipped
    /// keys) as needed. Returns an error for an already-consumed position,
    /// a jump beyond the skip bound, or exhausted fresh-chain advancement.
    pub fn message_key(&mut self, target: u32) -> Result<[u8; 32], SenderKeyError> {
        if let Some(stored) = self.skipped.get_mut(&target) {
            let mk = *stored;
            stored.zeroize();
            self.skipped.remove(&target);
            // Drop the matching order entry too, or `order` would accumulate
            // consumed-key tombstones without bound: consuming a MIDDLE key while a
            // lower index stays buffered leaves a front-only prune unable to reclaim
            // it (the MAX_SKIPPED_TOTAL cap only bounds the map). Kept in sync,
            // `order.len() == skipped.len()` always (mirrors the DM ratchet).
            self.order.retain(|k| *k != target);
            return Ok(mk);
        }
        if target < self.n {
            return Err(SenderKeyError::Malformed(
                "message key already consumed".into(),
            ));
        }
        // Check the entire requested advance before changing keys, skip maps or
        // eviction order. Already-buffered keys above remain usable at terminal n.
        let next_n = target.checked_add(1).ok_or_else(exhausted)?;
        if target - self.n > MAX_SKIP {
            return Err(SenderKeyError::TooManySkipped);
        }
        while self.n < target {
            let (next, mk) = kdf_ck(&self.chain_key);
            let next = Zeroizing::new(next);
            let mk = Zeroizing::new(mk);
            self.skipped.insert(self.n, *mk);
            self.order.push_back(self.n);
            self.chain_key.zeroize();
            self.chain_key = *next;
            self.n += 1;
            while self.skipped.len() > MAX_SKIPPED_TOTAL {
                if let Some(old) = self.order.pop_front() {
                    if let Some(evicted) = self.skipped.get_mut(&old) {
                        evicted.zeroize();
                    }
                    self.skipped.remove(&old);
                } else {
                    break;
                }
            }
        }
        let (next, mk) = kdf_ck(&self.chain_key);
        let next = Zeroizing::new(next);
        self.chain_key.zeroize();
        self.chain_key = *next;
        self.n = next_n;
        Ok(mk)
    }
}

/// Seal `plaintext` with a single-use message key; `aad` authenticates the wire
/// framing (epoch ‖ sender ‖ n) the channel layer prepends.
pub fn seal_message(
    mk: &[u8; 32],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, SenderKeyError> {
    let (mut key, mut nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SenderKeyError::Encrypt)?;
    let mut aead_nonce = Nonce::try_from(nonce.as_slice()).map_err(|_| SenderKeyError::Encrypt)?;
    let result = cipher.encrypt(
        &aead_nonce,
        Payload {
            msg: plaintext,
            aad,
        },
    );
    key.zeroize();
    nonce.zeroize();
    aead_nonce.as_mut_slice().zeroize();
    result.map_err(|_| SenderKeyError::Encrypt)
}

pub fn open_message(
    mk: &[u8; 32],
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, SenderKeyError> {
    let (mut key, mut nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| SenderKeyError::Decrypt)?;
    let mut aead_nonce = Nonce::try_from(nonce.as_slice()).map_err(|_| SenderKeyError::Decrypt)?;
    let result = cipher.decrypt(
        &aead_nonce,
        Payload {
            msg: ciphertext,
            aad,
        },
    );
    key.zeroize();
    nonce.zeroize();
    aead_nonce.as_mut_slice().zeroize();
    result.map_err(|_| SenderKeyError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange(sk: &mut SenderKey, rc: &mut SenderChain, pt: &[u8], aad: &[u8]) -> Vec<u8> {
        let (n, mk) = sk.ratchet();
        let ct = seal_message(&mk, pt, aad).unwrap();
        let (rn, rmk) = (n, rc.message_key(n).unwrap());
        assert_eq!(rn, n);
        open_message(&rmk, &ct, aad).unwrap()
    }

    #[test]
    fn abandoned_receive_preparation_leaves_source_exactly_unchanged() {
        let sender = SenderKey::generate();
        let receiver = SenderChain::from_distribution(&sender.distribution());
        let before = receiver.encode_checkpoint().unwrap();
        let mut sending = sender;
        let (_, key) = sending.try_ratchet().unwrap();
        let key = Zeroizing::new(key);
        let ciphertext = seal_message(&key, b"prepared only", b"aad").unwrap();
        let candidate = receiver.prepare_open(0, &ciphertext, b"aad").unwrap();
        assert_eq!(candidate.plaintext.as_slice(), b"prepared only");
        assert_eq!(*receiver.encode_checkpoint().unwrap(), *before);
        drop(candidate);
        assert_eq!(*receiver.encode_checkpoint().unwrap(), *before);
        assert!(receiver.prepare_open(0, &ciphertext, b"aad").is_ok());
    }

    #[test]
    fn sender_exhaustion_is_checked_and_legacy_panic_leaves_state_unchanged() {
        let mut sender = SenderKey {
            chain_key: [19; 32],
            n: u32::MAX - 1,
        };
        assert_eq!(sender.try_ratchet().unwrap().0, u32::MAX - 1);
        let before = sender.serialize();
        assert!(sender.try_ratchet().is_err());
        assert_eq!(sender.serialize(), before);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sender.ratchet())).is_err()
        );
        assert_eq!(sender.serialize(), before);
    }

    #[test]
    fn terminal_receiver_keeps_delayed_skipped_keys() {
        let mut receiver = SenderChain::from_distribution(&SenderKeyDistribution {
            chain_key: [17; 32],
            n: u32::MAX - 2,
        });
        receiver.message_key(u32::MAX - 1).unwrap();
        let mut restored =
            SenderChain::decode_checkpoint(&receiver.encode_checkpoint().unwrap()).unwrap();
        assert!(restored.message_key(u32::MAX).is_err());
        assert!(restored.message_key(u32::MAX - 2).is_ok());
        assert!(restored.message_key(u32::MAX - 2).is_err());
        assert!(restored.message_key(u32::MAX - 1).is_err());
    }

    #[test]
    fn receiver_terminal_position_rejects_before_any_mutation() {
        let mut receiver = SenderChain::from_distribution(&SenderKeyDistribution {
            chain_key: [17; 32],
            n: u32::MAX - 2,
        });
        let before = receiver.chain_key;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            receiver.message_key(u32::MAX)
        }));
        assert!(
            matches!(result, Ok(Err(_))),
            "exhaustion must return an error"
        );
        assert_eq!(receiver.n, u32::MAX - 2);
        assert_eq!(receiver.chain_key, before);
        assert!(receiver.skipped.is_empty());
        assert!(receiver.order.is_empty());
    }

    #[test]
    fn in_order_messages_round_trip() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        assert_eq!(exchange(&mut sk, &mut rc, b"m0", b"aad0"), b"m0");
        assert_eq!(exchange(&mut sk, &mut rc, b"m1", b"aad1"), b"m1");
    }

    #[test]
    fn out_of_order_uses_skipped_keys() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n0, mk0) = sk.ratchet();
        let c0 = seal_message(&mk0, b"m0", b"a").unwrap();
        let (n1, mk1) = sk.ratchet();
        let c1 = seal_message(&mk1, b"m1", b"a").unwrap();
        let (n2, mk2) = sk.ratchet();
        let c2 = seal_message(&mk2, b"m2", b"a").unwrap();
        // receive 2, 0, 1
        let r2 = rc.message_key(n2).unwrap();
        assert_eq!(open_message(&r2, &c2, b"a").unwrap(), b"m2");
        let r0 = rc.message_key(n0).unwrap();
        assert_eq!(open_message(&r0, &c0, b"a").unwrap(), b"m0");
        let r1 = rc.message_key(n1).unwrap();
        assert_eq!(open_message(&r1, &c1, b"a").unwrap(), b"m1");
    }

    #[test]
    fn a_consumed_position_cannot_be_reopened() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n0, _mk0) = sk.ratchet();
        let _ = rc.message_key(n0).unwrap();
        assert!(rc.message_key(n0).is_err()); // single-use (forward secrecy)
    }

    #[test]
    fn too_many_skipped_is_rejected() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        for _ in 0..(MAX_SKIP + 2) {
            sk.ratchet();
        }
        let (n, _mk) = sk.ratchet();
        assert!(matches!(
            rc.message_key(n),
            Err(SenderKeyError::TooManySkipped)
        ));
    }

    #[test]
    fn a_fresh_sender_key_after_rotation_is_unrelated() {
        let mut sk1 = SenderKey::generate();
        let (_n, mk1) = sk1.ratchet();
        let mut sk2 = SenderKey::generate(); // rotation: brand-new chain
        // Snapshot the distribution at position 0 (before sending) so a receiver
        // follows the new chain from the start.
        let mut rc2 = SenderChain::from_distribution(&sk2.distribution());
        let (_n2, mk2) = sk2.ratchet();
        assert_ne!(mk1, mk2);
        // A receiver with only the new distribution cannot open the old chain's key
        // position (different chain key).
        assert_ne!(rc2.message_key(0).unwrap(), mk1);
    }

    #[test]
    fn tampered_ciphertext_or_aad_fails() {
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());
        let (n, mk) = sk.ratchet();
        let mut ct = seal_message(&mk, b"secret", b"aad").unwrap();
        let rmk = rc.message_key(n).unwrap();
        let last = ct.len() - 1;
        ct[last] ^= 0xFF;
        assert!(open_message(&rmk, &ct, b"aad").is_err());
        ct[last] ^= 0xFF;
        assert!(open_message(&rmk, &ct, b"WRONG").is_err()); // AAD mismatch
    }

    #[test]
    fn sender_key_serialize_round_trips_and_resumes_chain() {
        let mut sk = SenderKey::generate();
        // Advance the chain so n != 0; the restored key must resume from here.
        let (_n0, _mk0) = sk.ratchet();
        let mut rc = SenderChain::from_distribution(&SenderKeyDistribution {
            chain_key: sk.chain_key,
            n: sk.n,
        });
        let bytes = sk.serialize();
        let mut restored = SenderKey::deserialize(&bytes).unwrap();
        // The restored key continues the SAME chain: its next message opens against a
        // receiver that started at the restored position.
        let (n, mk) = restored.ratchet();
        let ct = seal_message(&mk, b"resumed", b"aad").unwrap();
        let rmk = rc.message_key(n).unwrap();
        assert_eq!(open_message(&rmk, &ct, b"aad").unwrap(), b"resumed");
    }

    #[test]
    fn sender_key_deserialize_rejects_trailing_bytes() {
        let sk = SenderKey::generate();
        let mut bytes = sk.serialize();
        bytes.push(0xAB);
        assert!(SenderKey::deserialize(&bytes).is_none());
    }

    #[test]
    fn sender_key_formats_encode_to_golden_bytes() {
        // Pins both sender-key wire layouts (fixint bincode). A field reorder/resize
        // breaks interop with deployed peers and durable on-disk sender keys.
        let skd = SenderKeyDistribution {
            chain_key: [4u8; 32],
            n: 5,
        };
        assert_eq!(
            hex::encode(skd.encode()),
            "0404040404040404040404040404040404040404040404040404040404040404\
05000000"
        );
        assert_eq!(SenderKeyDistribution::decode(&skd.encode()), Some(skd));

        // The (private) SenderKeyWire used by SenderKey::serialize.
        let sk = SenderKey {
            chain_key: [4u8; 32],
            n: 7,
        };
        assert_eq!(
            hex::encode(sk.serialize()),
            "0404040404040404040404040404040404040404040404040404040404040404\
07000000"
        );
        assert_eq!(SenderKey::deserialize(&sk.serialize()).unwrap().n, 7);
    }

    #[test]
    fn order_stays_bounded_when_consuming_middle_keys() {
        // Regression (DoS): consuming a MIDDLE buffered key while a lower index stays
        // buffered must reclaim its `order` entry, or `order` accumulates tombstones
        // without bound even though `skipped` is capped. A hostile sender driving high-n
        // headers would otherwise grow `order` on every peer forever. Invariant:
        // order.len() == skipped.len() at all times.
        let mut sk = SenderKey::generate();
        let mut rc = SenderChain::from_distribution(&sk.distribution());

        // Buffer 0..=2 by jumping to 3 (positions 0,1,2 become skipped).
        for _ in 0..4 {
            sk.ratchet();
        }
        rc.message_key(3).unwrap();
        assert_eq!(rc.order.len(), 3);
        assert_eq!(rc.order.len(), rc.skipped.len());

        // Consume the MIDDLE (1), leaving the lower index 0 buffered. Front-only
        // pruning could NOT reclaim 1's slot; retain must.
        rc.message_key(1).unwrap();
        assert_eq!(rc.skipped.len(), 2); // {0, 2}
        assert_eq!(rc.order.len(), rc.skipped.len());

        // Soak: repeatedly buffer a fresh high run and consume its middle, always
        // leaving index 0 stuck. order must track skipped, never grow unbounded.
        let mut next = 4u32;
        for _ in 0..500 {
            let lo = next;
            for _ in 0..3 {
                sk.ratchet();
                next += 1;
            }
            // jump to lo+2 buffering lo, lo+1
            rc.message_key(lo + 2).unwrap();
            // consume the middle (lo+1), leaving lo buffered
            rc.message_key(lo + 1).unwrap();
            assert_eq!(rc.order.len(), rc.skipped.len());
            assert!(rc.skipped.len() <= MAX_SKIPPED_TOTAL);
        }
        // order never accumulated tombstones beyond the live map.
        assert_eq!(rc.order.len(), rc.skipped.len());
    }

    #[test]
    fn receive_bootstrap_preserves_future_keys_without_consumed_or_skipped_history() {
        let mut sender = SenderKey::generate();
        let mut source = SenderChain::from_distribution(&sender.distribution());
        for _ in 0..4 {
            sender.ratchet();
        }
        source.message_key(3).unwrap();
        let snapshot = source.current_distribution();
        assert_eq!(snapshot.n, 4);
        let mut target = SenderChain::from_distribution(&snapshot);
        assert!(
            target.message_key(0).is_err(),
            "skipped history stays on the original device"
        );
        assert!(
            target.message_key(3).is_err(),
            "consumed positions are not restored"
        );
        let (position, expected) = sender.ratchet();
        assert_eq!(target.message_key(position).unwrap(), expected);
        assert_eq!(source.message_key(position).unwrap(), expected);
        assert!(
            source.message_key(0).is_ok(),
            "export does not consume the source's skipped keys"
        );
    }

    #[test]
    fn skd_codec_round_trips() {
        let skd = SenderKeyDistribution {
            chain_key: [4u8; 32],
            n: 5,
        };
        assert_eq!(SenderKeyDistribution::decode(&skd.encode()), Some(skd));
        let mut junk = SenderKeyDistribution {
            chain_key: [0u8; 32],
            n: 0,
        }
        .encode();
        junk.push(0xAB);
        assert_eq!(SenderKeyDistribution::decode(&junk), None);
    }
}

//! The Double Ratchet state machine: `RatchetState` + `init_alice`/`init_bob` +
//! `ratchet_encrypt`/`ratchet_decrypt`, with the DH ratchet, symmetric chains, and
//! bounded skipped-message-key handling for out-of-order delivery.

use crate::ratchet::kdf::{kdf_ck, kdf_rk, message_keys};
use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use bincode::Options;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroize;

const MAX_SKIP: u32 = 1000;

/// Hard cap on TOTAL buffered skipped message keys (across all chains/steps) — a
/// memory-DoS backstop. Oldest are evicted; an extremely-late message older than
/// this bound won't decrypt (acceptable, matches Signal's bounded buffer).
const MAX_SKIPPED_TOTAL: usize = 2000;

#[derive(Debug, PartialEq, Eq)]
pub enum RatchetError {
    /// AEAD decryption failed (wrong key, tampered ciphertext/header).
    Decrypt,
    /// The header requested more skipped keys than `MAX_SKIP`.
    TooManySkipped,
    /// We have no sending chain yet (Bob must receive before he can send).
    NotInitializedForSend,
    /// A malformed header.
    Malformed,
}

/// The per-message header, authenticated as AEAD associated data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub ratchet_pub: [u8; 32],
    pub pn: u32, // previous sending-chain length
    pub n: u32,  // message number in the current sending chain
}

impl Header {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("header serializes")
    }
    /// Strict parse (`reject_trailing_bytes`): the header is AEAD associated data
    /// on a security-critical ratchet message, so a malleable parse tolerating
    /// trailing bytes would admit distinct encodings of the same header (an
    /// oracle/malleability foothold). It must STAY strict; evolution is via a new
    /// version, never by relaxing this.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// A Double Ratchet session's state (one peer). In-memory for now (durable storage
/// is a later plan).
pub struct RatchetState {
    dhs_secret: StaticSecret, // our current ratchet private key
    dhs_public: PublicKey,    // our current ratchet public key
    dhr: Option<PublicKey>,   // their current ratchet public key
    rk: [u8; 32],             // root key
    cks: Option<[u8; 32]>,    // sending chain key
    ckr: Option<[u8; 32]>,    // receiving chain key
    ns: u32,                  // messages sent in current sending chain
    nr: u32,                  // messages received in current receiving chain
    pn: u32,                  // previous sending-chain length
    skipped: HashMap<([u8; 32], u32), [u8; 32]>, // (their ratchet pub, n) -> mk
    skipped_order: VecDeque<([u8; 32], u32)>, // insertion order for FIFO eviction
}

fn dh(secret: &StaticSecret, public: &PublicKey) -> [u8; 32] {
    secret.diffie_hellman(public).to_bytes()
}

/// Initialise the SENDER (Alice): she knows the shared secret + Bob's initial ratchet
/// public key, and sets up the first sending chain via a DH ratchet.
pub fn init_alice(shared_secret: &[u8; 32], bob_ratchet_pub: &[u8; 32]) -> RatchetState {
    let dhs_secret = StaticSecret::random_from_rng(OsRng);
    let dhs_public = PublicKey::from(&dhs_secret);
    let dhr = PublicKey::from(*bob_ratchet_pub);
    let (rk, cks) = kdf_rk(shared_secret, &dh(&dhs_secret, &dhr));
    RatchetState {
        dhs_secret,
        dhs_public,
        dhr: Some(dhr),
        rk,
        cks: Some(cks),
        ckr: None,
        ns: 0,
        nr: 0,
        pn: 0,
        skipped: HashMap::new(),
        skipped_order: VecDeque::new(),
    }
}

/// Initialise the RECEIVER (Bob): he holds the shared secret + his own ratchet
/// keypair (whose PUBLIC half Alice used in `init_alice`). He has no sending chain
/// until he processes Alice's first message (which DH-ratchets him).
pub fn init_bob(shared_secret: &[u8; 32], bob_ratchet_secret: [u8; 32]) -> RatchetState {
    let dhs_secret = StaticSecret::from(bob_ratchet_secret);
    let dhs_public = PublicKey::from(&dhs_secret);
    RatchetState {
        dhs_secret,
        dhs_public,
        dhr: None,
        rk: *shared_secret,
        cks: None,
        ckr: None,
        ns: 0,
        nr: 0,
        pn: 0,
        skipped: HashMap::new(),
        skipped_order: VecDeque::new(),
    }
}

impl RatchetState {
    /// Our current ratchet public key (Alice publishes this; tests need it).
    pub fn ratchet_public(&self) -> [u8; 32] {
        self.dhs_public.to_bytes()
    }

    /// Encrypt `plaintext`, advancing the sending chain. Returns the header + the
    /// AEAD ciphertext (the header is authenticated as AAD).
    pub fn ratchet_encrypt(&mut self, plaintext: &[u8]) -> Result<(Header, Vec<u8>), RatchetError> {
        let cks = self.cks.ok_or(RatchetError::NotInitializedForSend)?;
        let (cks2, mk) = kdf_ck(&cks);
        self.cks = Some(cks2);
        let header = Header {
            ratchet_pub: self.dhs_public.to_bytes(),
            pn: self.pn,
            n: self.ns,
        };
        self.ns += 1;
        let ct = aead_encrypt(&mk, plaintext, &header.encode())?;
        Ok((header, ct))
    }

    /// Decrypt a message. Handles a DH-ratchet step (new `ratchet_pub`) and
    /// out-of-order delivery (skipped keys), then advances the receiving chain.
    ///
    /// NOTE: this mutates `self` (DH ratchet / skipped keys / chain advance) BEFORE the final
    /// AEAD authentication, so a forged message can leave `self` in an advanced state on `Err`.
    /// Callers must therefore decrypt on a COPY and persist it only on `Ok` — the node does
    /// (`node::dm_ratchet` operates on a fresh `RatchetSessions::get` clone, `put`-ing only on
    /// success), so a forged/replayed message cannot corrupt the durable session.
    pub fn ratchet_decrypt(
        &mut self,
        header: &Header,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, RatchetError> {
        // 1. A previously-skipped key for this exact (ratchet_pub, n)?
        let key = (header.ratchet_pub, header.n);
        if let Some(mk) = self.skipped.remove(&key) {
            // Drop the matching FIFO entry too, or `skipped_order` would accumulate
            // consumed-key tombstones without bound (the MAX_SKIPPED_TOTAL cap only
            // bounds the map). Kept in sync, `skipped_order.len() == skipped.len()`.
            self.skipped_order.retain(|k| *k != key);
            return aead_decrypt(&mk, ciphertext, &header.encode());
        }
        // 2. New DH ratchet key? Skip the rest of the current receiving chain, then ratchet.
        let header_pub = PublicKey::from(header.ratchet_pub);
        if self.dhr.map(|d| d.to_bytes()) != Some(header.ratchet_pub) {
            self.skip_message_keys(header.pn)?;
            self.dh_ratchet(&header_pub);
        }
        // 3. Skip forward in the (now correct) receiving chain to header.n.
        self.skip_message_keys(header.n)?;
        // 4. Derive this message's key and advance.
        let ckr = self.ckr.ok_or(RatchetError::Malformed)?;
        let (ckr2, mk) = kdf_ck(&ckr);
        self.ckr = Some(ckr2);
        self.nr += 1;
        aead_decrypt(&mk, ciphertext, &header.encode())
    }

    /// Store skipped message keys in the current receiving chain up to (not incl) `until`.
    fn skip_message_keys(&mut self, until: u32) -> Result<(), RatchetError> {
        let Some(ckr) = self.ckr else {
            return Ok(()); // no receiving chain yet (first ever message)
        };
        if until < self.nr {
            return Ok(());
        }
        if until - self.nr > MAX_SKIP {
            return Err(RatchetError::TooManySkipped);
        }
        let dhr = self.dhr.expect("ckr implies dhr").to_bytes();
        let mut ck = ckr;
        while self.nr < until {
            let (ck2, mk) = kdf_ck(&ck);
            self.skipped.insert((dhr, self.nr), mk);
            self.skipped_order.push_back((dhr, self.nr));
            while self.skipped.len() > MAX_SKIPPED_TOTAL {
                if let Some(old) = self.skipped_order.pop_front() {
                    if let Some(mut evicted) = self.skipped.remove(&old) {
                        evicted.zeroize();
                    }
                } else {
                    break;
                }
            }
            ck = ck2;
            self.nr += 1;
        }
        self.ckr = Some(ck);
        Ok(())
    }

    /// Perform a DH ratchet step on receiving a new ratchet public key.
    fn dh_ratchet(&mut self, header_pub: &PublicKey) {
        self.pn = self.ns;
        self.ns = 0;
        self.nr = 0;
        self.dhr = Some(*header_pub);
        // New receiving chain from their new pubkey.
        let (rk2, ckr) = kdf_rk(&self.rk, &dh(&self.dhs_secret, header_pub));
        self.rk = rk2;
        self.ckr = Some(ckr);
        // New sending keypair + sending chain.
        self.dhs_secret = StaticSecret::random_from_rng(OsRng);
        self.dhs_public = PublicKey::from(&self.dhs_secret);
        let (rk3, cks) = kdf_rk(&self.rk, &dh(&self.dhs_secret, header_pub));
        self.rk = rk3;
        self.cks = Some(cks);
    }
}

// ---------------------------------------------------------------------------
// Serialization mirror (private — used only inside serialize/deserialize)
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct RatchetWire {
    dhs_secret: [u8; 32],
    dhr: Option<[u8; 32]>,
    rk: [u8; 32],
    cks: Option<[u8; 32]>,
    ckr: Option<[u8; 32]>,
    ns: u32,
    nr: u32,
    pn: u32,
    // (their ratchet pub, message number, message key)
    skipped: Vec<([u8; 32], u32, [u8; 32])>,
}

impl RatchetState {
    /// Serialize the full session (fixint bincode). The output is SECRET — callers
    /// MUST store it encrypted at rest.
    pub fn serialize(&self) -> Vec<u8> {
        let wire = RatchetWire {
            dhs_secret: self.dhs_secret.to_bytes(),
            dhr: self.dhr.map(|p| p.to_bytes()),
            rk: self.rk,
            cks: self.cks,
            ckr: self.ckr,
            ns: self.ns,
            nr: self.nr,
            pn: self.pn,
            // Emit in `skipped_order` (true FIFO insertion order), NOT HashMap order,
            // so a reload reconstructs the same eviction order — otherwise the oldest-
            // first cap (MAX_SKIPPED_TOTAL) would drop arbitrary keys after a restart,
            // losing legitimately-buffered out-of-order messages.
            skipped: self
                .skipped_order
                .iter()
                .filter_map(|key| self.skipped.get(key).map(|mk| (key.0, key.1, *mk)))
                .collect(),
        };
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&wire)
            .expect("ratchet state serializes")
    }

    /// Reconstruct a session from [`serialize`] output. `None` if malformed.
    /// Strict parse (`reject_trailing_bytes`): the serialized session is secret
    /// key material stored encrypted at rest; it must STAY strict, and grows only
    /// via a new version, never by tolerating extra bytes.
    pub fn deserialize(bytes: &[u8]) -> Option<RatchetState> {
        let wire: RatchetWire = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()?;
        let dhs_secret = StaticSecret::from(wire.dhs_secret);
        let dhs_public = PublicKey::from(&dhs_secret);
        let skipped_order: VecDeque<([u8; 32], u32)> =
            wire.skipped.iter().map(|(p, n, _)| (*p, *n)).collect();
        let skipped: HashMap<([u8; 32], u32), [u8; 32]> = wire
            .skipped
            .into_iter()
            .map(|(pub_, n, mk)| ((pub_, n), mk))
            .collect();
        Some(RatchetState {
            dhs_secret,
            dhs_public,
            dhr: wire.dhr.map(PublicKey::from),
            rk: wire.rk,
            cks: wire.cks,
            ckr: wire.ckr,
            ns: wire.ns,
            nr: wire.nr,
            pn: wire.pn,
            skipped,
            skipped_order,
        })
    }

    /// Test-only accessor: number of currently buffered skipped keys.
    #[cfg(test)]
    pub fn skipped_len(&self) -> usize {
        self.skipped.len()
    }

    /// Test-only accessor: buffered skipped-key positions in FIFO (insertion) order.
    #[cfg(test)]
    pub fn skipped_order_ns(&self) -> Vec<u32> {
        self.skipped_order.iter().map(|(_, n)| *n).collect()
    }
}

impl Drop for RatchetState {
    fn drop(&mut self) {
        self.rk.zeroize();
        if let Some(ck) = self.cks.as_mut() {
            ck.zeroize();
        }
        if let Some(ck) = self.ckr.as_mut() {
            ck.zeroize();
        }
        for mk in self.skipped.values_mut() {
            mk.zeroize();
        }
        // dhs_secret (StaticSecret) zeroizes itself on drop.
    }
}

fn aead_encrypt(mk: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, RatchetError> {
    let (mut key, mut nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RatchetError::Decrypt)?;
    let result = cipher.encrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: plaintext,
            aad,
        },
    );
    key.zeroize();
    nonce.zeroize();
    result.map_err(|_| RatchetError::Decrypt)
}

fn aead_decrypt(mk: &[u8; 32], ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, RatchetError> {
    let (mut key, mut nonce) = message_keys(mk);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| RatchetError::Decrypt)?;
    let result = cipher.decrypt(
        Nonce::from_slice(&nonce),
        Payload {
            msg: ciphertext,
            aad,
        },
    );
    key.zeroize();
    nonce.zeroize();
    result.map_err(|_| RatchetError::Decrypt)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Establish a paired Alice/Bob session over a fixed shared secret + Bob keypair.
    fn pair() -> (RatchetState, RatchetState) {
        let shared = [9u8; 32];
        let bob_secret = [3u8; 32];
        let bob_pub = PublicKey::from(&StaticSecret::from(bob_secret)).to_bytes();
        let alice = init_alice(&shared, &bob_pub);
        let bob = init_bob(&shared, bob_secret);
        (alice, bob)
    }

    #[test]
    fn single_message_round_trips() {
        let (mut alice, mut bob) = pair();
        let (h, ct) = alice.ratchet_encrypt(b"hello bob").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &ct).unwrap(), b"hello bob");
    }

    #[test]
    fn bidirectional_with_dh_ratchets() {
        let (mut alice, mut bob) = pair();
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h1, &c1).unwrap(), b"a1");
        let (h2, c2) = bob.ratchet_encrypt(b"b1").unwrap(); // Bob can send after receiving
        assert_eq!(alice.ratchet_decrypt(&h2, &c2).unwrap(), b"b1");
        let (h3, c3) = alice.ratchet_encrypt(b"a2").unwrap(); // Alice DH-ratchets again
        assert_eq!(bob.ratchet_decrypt(&h3, &c3).unwrap(), b"a2");
    }

    #[test]
    fn out_of_order_within_a_chain() {
        let (mut alice, mut bob) = pair();
        let (h0, c0) = alice.ratchet_encrypt(b"m0").unwrap();
        let (h1, c1) = alice.ratchet_encrypt(b"m1").unwrap();
        let (h2, c2) = alice.ratchet_encrypt(b"m2").unwrap();
        // Bob receives 2, then 0, then 1 (skipped keys cover the gaps).
        assert_eq!(bob.ratchet_decrypt(&h2, &c2).unwrap(), b"m2");
        assert_eq!(bob.ratchet_decrypt(&h0, &c0).unwrap(), b"m0");
        assert_eq!(bob.ratchet_decrypt(&h1, &c1).unwrap(), b"m1");
    }

    #[test]
    fn consuming_a_skipped_key_prunes_the_fifo_order() {
        // Regression: consuming a buffered out-of-order key must drop its FIFO entry too,
        // or `skipped_order` accumulates consumed-key tombstones without bound (the
        // MAX_SKIPPED_TOTAL cap only bounds the map). The two must stay the same length.
        let (mut alice, mut bob) = pair();
        let msgs: Vec<(Header, Vec<u8>)> = (0..4u8)
            .map(|i| alice.ratchet_encrypt(&[i]).unwrap())
            .collect();
        // Receive #3 first → 0,1,2 buffered as skipped, FIFO [0,1,2].
        bob.ratchet_decrypt(&msgs[3].0, &msgs[3].1).unwrap();
        assert_eq!(bob.skipped_len(), 3);
        assert_eq!(bob.skipped_order_ns(), vec![0, 1, 2]);
        // Consuming a buffered key shrinks BOTH structures and preserves FIFO order.
        bob.ratchet_decrypt(&msgs[1].0, &msgs[1].1).unwrap();
        assert_eq!(bob.skipped_len(), 2);
        assert_eq!(bob.skipped_order_ns(), vec![0, 2]);
        bob.ratchet_decrypt(&msgs[0].0, &msgs[0].1).unwrap();
        bob.ratchet_decrypt(&msgs[2].0, &msgs[2].1).unwrap();
        // All consumed → no tombstones left behind.
        assert_eq!(bob.skipped_len(), 0);
        assert!(bob.skipped_order_ns().is_empty());
    }

    #[test]
    fn skipped_key_fifo_order_survives_serialization() {
        // Regression: serialize must emit skipped keys in FIFO (skipped_order) order,
        // not HashMap order, so the oldest-first eviction cap stays correct after a
        // reload (which happens on every restart). Otherwise a reload would evict
        // arbitrary buffered keys, silently dropping out-of-order messages under load.
        let (mut alice, mut bob) = pair();
        let msgs: Vec<(Header, Vec<u8>)> = (0..5u8)
            .map(|i| alice.ratchet_encrypt(&[i]).unwrap())
            .collect();
        // Bob receives the last first → buffers positions 0..=3 as skipped, in order.
        bob.ratchet_decrypt(&msgs[4].0, &msgs[4].1).unwrap();
        assert_eq!(bob.skipped_order_ns(), vec![0, 1, 2, 3]);

        // The FIFO order must round-trip through serialize/deserialize unchanged.
        let mut restored = RatchetState::deserialize(&bob.serialize()).unwrap();
        assert_eq!(restored.skipped_order_ns(), vec![0, 1, 2, 3]);

        // And the restored state still opens the oldest buffered message.
        assert_eq!(
            restored.ratchet_decrypt(&msgs[0].0, &msgs[0].1).unwrap(),
            &[0u8]
        );
    }

    #[test]
    fn out_of_order_across_a_dh_ratchet() {
        let (mut alice, mut bob) = pair();
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        bob.ratchet_decrypt(&h1, &c1).unwrap();
        let (hb, cb) = bob.ratchet_encrypt(b"b1").unwrap();
        alice.ratchet_decrypt(&hb, &cb).unwrap();
        // Alice sends two in her new chain; Bob gets the second before the first.
        let (h2, c2) = alice.ratchet_encrypt(b"a2").unwrap();
        let (h3, c3) = alice.ratchet_encrypt(b"a3").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h3, &c3).unwrap(), b"a3");
        assert_eq!(bob.ratchet_decrypt(&h2, &c2).unwrap(), b"a2");
    }

    #[test]
    fn too_many_skipped_is_rejected() {
        let (mut alice, mut bob) = pair();
        // Alice sends MAX_SKIP + 2 messages; Bob jumps straight to the last.
        let mut last = None;
        for i in 0..(MAX_SKIP + 2) {
            last = Some(alice.ratchet_encrypt(format!("m{i}").as_bytes()).unwrap());
        }
        let (h, c) = last.unwrap();
        assert_eq!(
            bob.ratchet_decrypt(&h, &c),
            Err(RatchetError::TooManySkipped)
        );
    }

    #[test]
    fn tampered_ciphertext_and_header_are_rejected() {
        let (mut alice, mut bob) = pair();
        let (h, mut c) = alice.ratchet_encrypt(b"secret").unwrap();
        let last = c.len() - 1;
        c[last] ^= 0xFF;
        assert_eq!(bob.ratchet_decrypt(&h, &c), Err(RatchetError::Decrypt));
        // A tampered header (different n) also fails (header is AAD).
        let (mut alice2, mut bob2) = pair();
        let (mut h2, c2) = alice2.ratchet_encrypt(b"secret").unwrap();
        h2.n = 7;
        assert!(bob2.ratchet_decrypt(&h2, &c2).is_err());
    }

    #[test]
    fn forward_secrecy_keys_are_single_use() {
        // After Bob decrypts message n, the same (header, ct) cannot be decrypted
        // again (its skipped key was consumed / the chain advanced past it).
        let (mut alice, mut bob) = pair();
        let (h, c) = alice.ratchet_encrypt(b"once").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &c).unwrap(), b"once");
        assert!(bob.ratchet_decrypt(&h, &c).is_err()); // no key to re-derive
    }

    #[test]
    fn header_encodes_to_golden_bytes() {
        // Pins the ratchet Header wire layout (fixint bincode). The header is AEAD
        // AAD on every ratchet message, so a layout change would break decryption
        // against deployed peers — fail loudly here if a field is reordered/resized.
        let h = Header {
            ratchet_pub: [5u8; 32],
            pn: 3,
            n: 9,
        };
        assert_eq!(
            hex::encode(h.encode()),
            "0505050505050505050505050505050505050505050505050505050505050505\
03000000\
09000000"
        );
        assert_eq!(Header::decode(&h.encode()).unwrap().n, 9);
    }

    #[test]
    fn header_codec_round_trips() {
        let h = Header {
            ratchet_pub: [5u8; 32],
            pn: 3,
            n: 9,
        };
        assert_eq!(Header::decode(&h.encode()).unwrap().n, 9);
        // Fail closed: too-short input decodes to None (no panic)...
        assert!(Header::decode(b"junk").is_none());
        // ...and trailing bytes after a valid header are rejected.
        let mut trailing = h.encode();
        trailing.push(0xff);
        assert!(Header::decode(&trailing).is_none());
    }

    #[test]
    fn a_serialized_session_resumes_losslessly() {
        let (mut alice, mut bob) = pair();
        // Exchange a few messages (advance both chains + a DH ratchet).
        let (h1, c1) = alice.ratchet_encrypt(b"a1").unwrap();
        bob.ratchet_decrypt(&h1, &c1).unwrap();
        let (hb, cb) = bob.ratchet_encrypt(b"b1").unwrap();
        alice.ratchet_decrypt(&hb, &cb).unwrap();
        // Create a skipped key on Bob's side (Alice sends 2, Bob will get #1 later).
        let (h2, c2) = alice.ratchet_encrypt(b"a2").unwrap();
        let (h3, c3) = alice.ratchet_encrypt(b"a3").unwrap();
        bob.ratchet_decrypt(&h3, &c3).unwrap(); // stores skipped key for a2

        // Serialize BOTH, drop, and reload.
        let alice2 = RatchetState::deserialize(&alice.serialize()).unwrap();
        let mut bob2 = RatchetState::deserialize(&bob.serialize()).unwrap();
        let mut alice2 = alice2;

        // Bob reloads and can still open the skipped a2; Alice reloads and keeps sending.
        assert_eq!(bob2.ratchet_decrypt(&h2, &c2).unwrap(), b"a2");
        let (h4, c4) = alice2.ratchet_encrypt(b"a4").unwrap();
        assert_eq!(bob2.ratchet_decrypt(&h4, &c4).unwrap(), b"a4");

        // Malformed input is rejected.
        assert!(RatchetState::deserialize(b"junk").is_none());
    }

    #[test]
    fn skipped_buffer_is_bounded() {
        let (mut alice, mut bob) = pair();
        // Drive many small skipped-key steps across DH ratchets so the TOTAL would
        // exceed the cap without eviction. Each round: Alice sends N then bob jumps.
        for _ in 0..6 {
            // advance alice's chain with a fresh DH ratchet each round
            let (hb, cb) = {
                let (h1, c1) = alice.ratchet_encrypt(b"x").unwrap();
                bob.ratchet_decrypt(&h1, &c1).unwrap();
                bob.ratchet_encrypt(b"y").unwrap()
            };
            alice.ratchet_decrypt(&hb, &cb).unwrap();
            // Alice sends 500, bob skips to the last → 499 skipped keys this step.
            let mut last = None;
            for i in 0..500u32 {
                last = Some(alice.ratchet_encrypt(format!("m{i}").as_bytes()).unwrap());
            }
            let (h, c) = last.unwrap();
            bob.ratchet_decrypt(&h, &c).unwrap();
        }
        // Total buffered skipped keys never exceeds the cap.
        assert!(bob.skipped_len() <= MAX_SKIPPED_TOTAL);
    }
}

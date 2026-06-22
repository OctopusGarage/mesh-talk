//! Device-linking pairing crypto + wire (multi-device). An existing device (the
//! "linker") shows a one-time [`PairingCode`]; a new device (the "joiner") proves it
//! over the LAN Noise channel and receives the account secret. The authenticator is a
//! SHA-256 secret-prefix MAC over the code and BOTH device keys, so a captured proof
//! cannot be replayed to enrol a different device. Confidentiality of the transferred
//! secret is provided by the Noise channel (encrypted to the joiner's pinned key);
//! the code provides authorization (user consent carried out-of-band).

use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PAIR_DOMAIN: &[u8] = b"mesh-talk-pairing-v1";
const REQ_MAGIC: &[u8] = b"MTPQ1";
const RESP_MAGIC: &[u8] = b"MTPS1";
const BACKFILL_MAGIC: &[u8] = b"MTBF1";

/// A one-time, high-entropy linking code shown by the linker and entered on the joiner.
#[derive(Clone)]
pub struct PairingCode([u8; 16]);

impl PairingCode {
    /// A fresh random 128-bit code.
    pub fn generate() -> Self {
        use rand_core::RngCore;
        let mut bytes = [0u8; 16];
        rand_core::OsRng.fill_bytes(&mut bytes);
        PairingCode(bytes)
    }

    /// 32 lowercase hex chars (what the user reads / types).
    pub fn as_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Parse a code from its hex form (case-insensitive). `None` if malformed.
    ///
    /// A valid code is exactly 16 bytes = 32 hex chars. We bound-check the length
    /// BEFORE `hex::decode` so an implausibly long (attacker- or fat-finger-supplied)
    /// string is rejected up front rather than allocating a decode buffer for it.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.len() != 32 {
            return None;
        }
        let bytes = hex::decode(s).ok()?;
        let arr: [u8; 16] = bytes.try_into().ok()?;
        Some(PairingCode(arr))
    }

    /// Authenticator = SHA-256(DOMAIN ‖ code ‖ linker_ed ‖ joiner_ed). Binding both
    /// device keys stops a captured tag being replayed to enrol another device.
    pub fn authenticator(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(PAIR_DOMAIN);
        h.update(self.0);
        h.update(linker_ed);
        h.update(joiner_ed);
        h.finalize().into()
    }

    /// Constant-time check that `tag` is the expected authenticator.
    pub fn verify(&self, linker_ed: &[u8; 32], joiner_ed: &[u8; 32], tag: &[u8; 32]) -> bool {
        ct_eq(&self.authenticator(linker_ed, joiner_ed), tag)
    }
}

/// Linker-side state while "link a device" mode is active: the one-time code plus when it
/// was issued and how many failed proofs we've seen. Backs a code TTL + attempt ceiling
/// (defense-in-depth — the code is already a full 128-bit secret, but a standing code with
/// unlimited guesses is a needless window).
pub struct PendingLink {
    pub code: PairingCode,
    pub created_at_ms: u64,
    pub attempts: u32,
}

/// Constant-time 32-byte equality (no early return on first mismatch).
fn ct_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// Joiner → linker: "I hold the code; here is my device identity." Magic-framed so the
/// responder can distinguish it from a sync wire on the first frame of a connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingRequest {
    pub joiner: PublicIdentity,
    pub tag: [u8; 32],
}

impl PairingRequest {
    pub fn encode(&self) -> Vec<u8> {
        frame(REQ_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(REQ_MAGIC, bytes)
    }
}

/// Linker → joiner: the account secret + the account public key + a certificate
/// binding the joiner's device key to the account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingResponse {
    pub account_secret: [u8; 32],
    pub account_ed25519_pub: [u8; 32],
    pub cert: DeviceCertificate,
}

impl PairingResponse {
    pub fn encode(&self) -> Vec<u8> {
        frame(RESP_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(RESP_MAGIC, bytes)
    }
}

/// One account-history message a linker hands a freshly-linked device so it starts
/// populated. Keyed by the account conversation id (a hash of account ids, identical
/// on both devices once linked) so the joiner can store it directly. `from` is the
/// reacting/sending account; account history derives `from_me` from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillRecord {
    pub conv: [u8; 32],
    pub from: String,
    pub wall_clock: u64,
    pub plaintext: Vec<u8>,
    pub event_id: [u8; 32],
}

impl BackfillRecord {
    pub fn encode(&self) -> Vec<u8> {
        frame(BACKFILL_MAGIC, self)
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(BACKFILL_MAGIC, bytes)
    }
}

fn frame<T: Serialize>(magic: &[u8], v: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(magic.len() + 64);
    out.extend_from_slice(magic);
    out.extend_from_slice(
        &bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(v)
            .expect("pairing message serializes"),
    );
    out
}

/// Upper bound on a single pairing/backfill frame before we attempt to deserialize.
/// Every pairing message is small (keys, a cert, one backfilled message body); a frame
/// larger than the Noise transport's own `MAX_FRAME` (65535) cannot be legitimate, so
/// reject it early rather than feeding it to bincode. Defense-in-depth: the transport
/// already caps wire frames; this guards the in-process decode path too.
const MAX_PAIRING_FRAME: usize = 65_535;

fn unframe<T: for<'de> Deserialize<'de>>(magic: &[u8], bytes: &[u8]) -> Option<T> {
    if bytes.len() > MAX_PAIRING_FRAME {
        return None;
    }
    let rest = bytes.strip_prefix(magic)?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize::<T>(rest)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn code_hex_round_trips() {
        let c = PairingCode::generate();
        assert_eq!(PairingCode::from_hex(&c.as_hex()).unwrap().0, c.0);
        assert!(PairingCode::from_hex("nothex").is_none());
        assert!(PairingCode::from_hex("ab").is_none()); // wrong length
                                                        // An implausibly long input is rejected up front (length bound before decode).
        assert!(PairingCode::from_hex(&"a".repeat(100_000)).is_none());
        assert!(PairingCode::from_hex(&"ab".repeat(17)).is_none()); // 34 chars
    }

    #[test]
    fn authenticator_verifies_and_binds_both_keys() {
        let code = PairingCode::generate();
        let linker = DeviceIdentity::generate();
        let joiner = DeviceIdentity::generate();
        let l = linker.public().ed25519_pub;
        let j = joiner.public().ed25519_pub;
        let tag = code.authenticator(&l, &j);
        assert!(code.verify(&l, &j, &tag));
        // Wrong code, wrong joiner, wrong linker all fail.
        assert!(!PairingCode::generate().verify(&l, &j, &tag));
        let other = DeviceIdentity::generate().public().ed25519_pub;
        assert!(!code.verify(&l, &other, &tag));
        assert!(!code.verify(&other, &j, &tag));
    }

    #[test]
    fn request_and_response_round_trip() {
        let joiner = DeviceIdentity::generate();
        let code = PairingCode::generate();
        let req = PairingRequest {
            joiner: joiner.public(),
            tag: code.authenticator(&[1u8; 32], &joiner.public().ed25519_pub),
        };
        assert_eq!(PairingRequest::decode(&req.encode()), Some(req));
        assert!(PairingRequest::decode(b"not a pairing frame").is_none());

        let account = Account::generate();
        let cert = account.certify(&joiner.public().ed25519_pub);
        let resp = PairingResponse {
            account_secret: account.secret_bytes(),
            account_ed25519_pub: account.public().ed25519_pub,
            cert,
        };
        assert_eq!(PairingResponse::decode(&resp.encode()), Some(resp));
    }

    #[test]
    fn a_sync_like_frame_is_not_a_pairing_request() {
        // Bytes without the magic must not decode as a pairing request.
        assert!(PairingRequest::decode(&[0u8, 1, 2, 3, 4, 5]).is_none());
    }
}

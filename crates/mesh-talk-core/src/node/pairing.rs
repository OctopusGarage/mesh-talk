//! Device-linking pairing crypto + wire (multi-device). An existing device (the
//! "linker") shows a one-time [`PairingCode`]; a new device (the "joiner") proves it
//! over the LAN Noise channel and receives the account secret. The authenticator is a
//! SHA-256 secret-prefix MAC over the code and BOTH device keys, so a captured proof
//! cannot be replayed to enrol a different device. Confidentiality of the transferred
//! secret is provided by the Noise channel (encrypted to the joiner's pinned key);
//! the code provides authorization (user consent carried out-of-band).

use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use crate::node::wire::{frame, unframe};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const PAIR_DOMAIN: &[u8] = b"mesh-talk-pairing-v1";
const REQ_MAGIC: &[u8] = b"MTPQ1";
const SELECTED_REQ_MAGIC: &[u8] = b"MTPQ2";
const SELECTED_PAIR_DOMAIN: &[u8] = b"mesh-talk-pairing-v2";
const RESP_MAGIC: &[u8] = b"MTPS1";
const METADATA_REQ_MAGIC: &[u8] = b"MTPQ3";
const METADATA_RESP_MAGIC: &[u8] = b"MTPS2";
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

    /// V2 proof binds the requested history choice before any history is sent.
    pub fn authenticator_selected(
        &self,
        linker_ed: &[u8; 32],
        joiner_ed: &[u8; 32],
        history: bool,
    ) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(SELECTED_PAIR_DOMAIN);
        h.update(self.0);
        h.update(linker_ed);
        h.update(joiner_ed);
        h.update([u8::from(history)]);
        h.finalize().into()
    }

    pub fn authenticator_metadata(
        &self,
        linker_ed: &[u8; 32],
        joiner_ed: &[u8; 32],
        history: bool,
    ) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"mesh-talk-pairing-v3");
        h.update(self.0);
        h.update(linker_ed);
        h.update(joiner_ed);
        h.update([u8::from(history)]);
        h.finalize().into()
    }

    pub fn verify_metadata(
        &self,
        linker_ed: &[u8; 32],
        joiner_ed: &[u8; 32],
        history: bool,
        tag: &[u8; 32],
    ) -> bool {
        ct_eq(
            &self.authenticator_metadata(linker_ed, joiner_ed, history),
            tag,
        )
    }

    /// Verify a V2 selection without accepting a V1 proof in its place.
    pub fn verify_selected(
        &self,
        linker_ed: &[u8; 32],
        joiner_ed: &[u8; 32],
        history: bool,
        tag: &[u8; 32],
    ) -> bool {
        ct_eq(
            &self.authenticator_selected(linker_ed, joiner_ed, history),
            tag,
        )
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
    /// Encode a V2 request; `tag` must authenticate the same history choice.
    pub fn encode_selected(&self, history: bool) -> Vec<u8> {
        frame(SELECTED_REQ_MAGIC, &(self, history))
    }

    pub fn encode_metadata(&self, history: bool) -> Vec<u8> {
        frame(METADATA_REQ_MAGIC, &(self, history))
    }

    pub fn decode_negotiated(bytes: &[u8]) -> Option<(Self, Option<bool>, bool)> {
        if let Some((request, history)) = unframe(METADATA_REQ_MAGIC, bytes) {
            return Some((request, Some(history), true));
        }
        Self::decode_selected(bytes).map(|(request, history)| (request, history, false))
    }

    /// Recognize all versions. Authorization must use `decode_negotiated` so
    /// the selection and its proof domain are preserved.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_negotiated(bytes).map(|(request, _, _)| request)
    }

    /// `None` selection denotes the unchanged V1 request, which requests history.
    pub fn decode_selected(bytes: &[u8]) -> Option<(Self, Option<bool>)> {
        if let Some((request, history)) = unframe(SELECTED_REQ_MAGIC, bytes) {
            return Some((request, Some(history)));
        }
        unframe(REQ_MAGIC, bytes).map(|request| (request, None))
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
    pub fn encode_metadata(&self, created_at_ms: Option<u64>) -> Vec<u8> {
        frame(METADATA_RESP_MAGIC, &(self, created_at_ms))
    }

    pub fn decode_metadata(bytes: &[u8]) -> Option<(Self, Option<u64>)> {
        let (response, created_at_ms) = unframe(METADATA_RESP_MAGIC, bytes)?;
        crate::identity::account::Account::from_secret_bytes_with_created_at(
            [0; 32],
            created_at_ms,
        )?;
        Some((response, created_at_ms))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn pairing_v3_metadata_proof_is_not_interchangeable_with_v2_or_v1() {
        let code = PairingCode::generate();
        let tag = code.authenticator_metadata(&[1; 32], &[2; 32], false);
        assert!(code.verify_metadata(&[1; 32], &[2; 32], false, &tag));
        assert!(!code.verify_metadata(&[1; 32], &[2; 32], true, &tag));
        assert!(!code.verify_metadata(&[9; 32], &[2; 32], false, &tag));
        assert!(!code.verify_metadata(&[1; 32], &[9; 32], false, &tag));
        assert!(!code.verify_selected(&[1; 32], &[2; 32], false, &tag));
        assert!(!code.verify(&[1; 32], &[2; 32], &tag));
        let request = PairingRequest {
            joiner: DeviceIdentity::generate().public(),
            tag,
        };
        assert_eq!(
            PairingRequest::decode_negotiated(&request.encode_metadata(false)),
            Some((request, Some(false), true))
        );
    }

    #[test]
    fn pairing_v2_request_is_recognized_without_changing_legacy_bytes() {
        let req = PairingRequest {
            joiner: DeviceIdentity::generate().public(),
            tag: [7; 32],
        };
        let legacy = frame(b"MTPQ1", &req);
        assert_eq!(req.encode(), legacy);
        assert_eq!(PairingRequest::decode(&legacy), Some(req.clone()));
        let selected = frame(b"MTPQ2", &(req.clone(), false));
        assert_eq!(PairingRequest::decode(&selected), Some(req));
    }

    #[test]
    fn pairing_v2_authenticator_binds_choice_code_and_both_keys() {
        let code = PairingCode::from_hex("000102030405060708090a0b0c0d0e0f").unwrap();
        let linker = [1; 32];
        let joiner = [2; 32];
        let tag = code.authenticator_selected(&linker, &joiner, false);
        assert!(code.verify_selected(&linker, &joiner, false, &tag));
        assert!(!code.verify_selected(&linker, &joiner, true, &tag));
        assert!(!code.verify_selected(&[3; 32], &joiner, false, &tag));
        assert!(!code.verify_selected(&linker, &[3; 32], false, &tag));
        assert!(!PairingCode::generate().verify_selected(&linker, &joiner, false, &tag));
        assert!(!code.verify(&linker, &joiner, &tag));
        assert!(!code.verify_selected(
            &linker,
            &joiner,
            false,
            &code.authenticator(&linker, &joiner)
        ));
        for history in [true, false] {
            let request = PairingRequest {
                joiner: PublicIdentity {
                    ed25519_pub: linker,
                    x25519_pub: joiner,
                },
                tag: code.authenticator_selected(&linker, &joiner, history),
            };
            assert_eq!(
                PairingRequest::decode_selected(&request.encode_selected(history)),
                Some((request, Some(history)))
            );
        }
    }

    #[test]
    fn pairing_v1_request_fixed_fixture_remains_unchanged() {
        let request = PairingRequest {
            joiner: PublicIdentity {
                ed25519_pub: [1; 32],
                x25519_pub: [2; 32],
            },
            tag: [3; 32],
        };
        let mut fixture = b"MTPQ1".to_vec();
        fixture.extend_from_slice(&[1; 32]);
        fixture.extend_from_slice(&[2; 32]);
        fixture.extend_from_slice(&[3; 32]);
        assert_eq!(request.encode(), fixture);
        assert_eq!(
            PairingRequest::decode_selected(&fixture),
            Some((request, None))
        );
    }

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

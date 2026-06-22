//! The signed discovery announce and its UDP wire format.

use crate::identity::account::{Account, DeviceCertificate};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Domain separator for announce signatures.
const ANNOUNCE_DOMAIN: &[u8] = b"mesh-talk-announce-v1";
/// Wire framing: 4-byte magic + 1-byte version, then `bincode(Announce)`.
const MAGIC: &[u8; 4] = b"MTAN";
/// The version this build EMITS. Bump only when the bincode body layout changes
/// (e.g. a new field/variant) — because bincode is positional, a layout change can
/// never be made in place; it is always a new version with its own parser.
const CURRENT_VERSION: u8 = 2;
/// The oldest body layout this build can still DECODE. `decode` accepts any version
/// in `[MIN_SUPPORTED_VERSION, CURRENT_VERSION]` and dispatches to a per-version
/// parser that up-converts to the canonical in-memory [`Announce`]. Today there is
/// exactly one layout (v2), so MIN == CURRENT; when v3 is added, keep this at 2 (so
/// v2 peers are still understood) and add a `decode_v3` arm.
const MIN_SUPPORTED_VERSION: u8 = 2;

/// A peer's self-announcement: its identity keys, display name, TCP listen port,
/// and whether it serves as a post office, signed by its Ed25519 key. `user_id`
/// is the fingerprint of `ed25519_pub`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub user_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub post_office: bool,
    /// The account this device claims membership in, proven by the account key's
    /// signature over this device's key. `None` for a device not yet linked to an
    /// account. The device's own `sig` covers the bound account key (below), so
    /// this cert cannot be swapped for one minting the device under another account.
    pub account_cert: Option<DeviceCertificate>,
    pub sig: Vec<u8>,
}

/// Length-prefixed, domain-separated bytes the announce signs over (everything
/// except `sig`). Length prefixes make the concatenation unambiguous. The account
/// public key is covered so a device's signature commits to which account it
/// claims — preventing an attacker from swapping in a different (validly-signed)
/// certificate to re-home this device under another account.
fn signing_input(
    user_id: &str,
    ed25519_pub: &[u8; 32],
    x25519_pub: &[u8; 32],
    name: &str,
    tcp_port: u16,
    post_office: bool,
    account_pub: Option<&[u8; 32]>,
) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(ANNOUNCE_DOMAIN);
    v.extend_from_slice(&(user_id.len() as u32).to_be_bytes());
    v.extend_from_slice(user_id.as_bytes());
    v.extend_from_slice(ed25519_pub);
    v.extend_from_slice(x25519_pub);
    v.extend_from_slice(&(name.len() as u32).to_be_bytes());
    v.extend_from_slice(name.as_bytes());
    v.extend_from_slice(&tcp_port.to_be_bytes());
    v.push(post_office as u8);
    match account_pub {
        Some(pk) => {
            v.extend_from_slice(&(pk.len() as u32).to_be_bytes()); // 32
            v.extend_from_slice(pk);
        }
        None => v.extend_from_slice(&0u32.to_be_bytes()),
    }
    v
}

impl Announce {
    /// Build and sign a normal (non-post-office) announce for `identity`, with no
    /// account binding.
    pub fn new(identity: &DeviceIdentity, name: impl Into<String>, tcp_port: u16) -> Self {
        Self::new_with_role(identity, name, tcp_port, false, None)
    }

    /// Build and sign a post-office announce for `identity`, with no account binding.
    pub fn new_post_office(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true, None)
    }

    /// Build and sign a normal announce that advertises `account` (the device is
    /// certified by the account and the device signature commits to the account key).
    pub fn new_with_account(
        identity: &DeviceIdentity,
        account: &Account,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, false, Some(account))
    }

    /// Build and sign a post-office announce that advertises `account`.
    pub fn new_post_office_with_account(
        identity: &DeviceIdentity,
        account: &Account,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Self {
        Self::new_with_role(identity, name, tcp_port, true, Some(account))
    }

    fn new_with_role(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
        post_office: bool,
        account: Option<&Account>,
    ) -> Self {
        let public = identity.public();
        let user_id = public.user_id();
        let name = name.into();
        let account_cert = account.map(|a| a.certify(&public.ed25519_pub));
        let account_pub = account_cert.as_ref().map(|c| &c.account_ed25519_pub);
        let sig = identity
            .sign(&signing_input(
                &user_id,
                &public.ed25519_pub,
                &public.x25519_pub,
                &name,
                tcp_port,
                post_office,
                account_pub,
            ))
            .to_vec();
        Announce {
            user_id,
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            name,
            tcp_port,
            post_office,
            account_cert,
            sig,
        }
    }

    /// True if the announce is internally consistent and authentically signed:
    /// `user_id` is the fingerprint of `ed25519_pub`; `sig` verifies (over all
    /// fields including `post_office` and the bound account key); and, if an account
    /// cert is present, it certifies THIS device under the bound account.
    pub fn verify(&self) -> bool {
        if self.user_id != PublicIdentity::user_id_from(&self.ed25519_pub) {
            return false;
        }
        // If a cert is present it must be for THIS device and validly account-signed.
        if let Some(cert) = &self.account_cert {
            if cert.device_ed25519_pub != self.ed25519_pub || !cert.verify() {
                return false;
            }
        }
        let account_pub = self.account_cert.as_ref().map(|c| &c.account_ed25519_pub);
        let Ok(sig): Result<[u8; 64], _> = self.sig.as_slice().try_into() else {
            return false;
        };
        DeviceIdentity::verify(
            &self.ed25519_pub,
            &signing_input(
                &self.user_id,
                &self.ed25519_pub,
                &self.x25519_pub,
                &self.name,
                self.tcp_port,
                self.post_office,
                account_pub,
            ),
            &sig,
        )
    }

    /// The advertised public identity (Ed25519 + X25519 keys).
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }

    /// The account id this device is bound to, if it advertises a (valid-shaped)
    /// cert. Only meaningful once `verify()` has returned true.
    pub fn account_id(&self) -> Option<String> {
        self.account_cert.as_ref().map(|c| c.account_id())
    }
}

/// Frame an announce for the wire (magic + CURRENT_VERSION + bincode body).
pub fn encode(announce: &Announce) -> Vec<u8> {
    let body = encode_v2(announce);
    let mut out = Vec::with_capacity(MAGIC.len() + 1 + body.len());
    out.extend_from_slice(MAGIC);
    out.push(CURRENT_VERSION);
    out.extend_from_slice(&body);
    out
}

/// Serialize the v2 body. The descriptive announce evolves by VERSION BUMP, not by
/// growing this layout in place (bincode is positional, so a new field would shift
/// every byte after it and break v2 peers) — a future change adds an `encode_v3` /
/// `decode_v3` pair and bumps `CURRENT_VERSION`, leaving this untouched.
fn encode_v2(announce: &Announce) -> Vec<u8> {
    bincode::serialize(announce).expect("announce serializes")
}

/// Parse a wire datagram into an announce, or `None` if the framing/bincode is
/// invalid. Accepts any version in `[MIN_SUPPORTED_VERSION, CURRENT_VERSION]` and
/// dispatches to that version's parser, which up-converts to the canonical
/// in-memory [`Announce`]. A too-new (or too-old/unknown) version is rejected. The
/// result is NOT yet authenticated — call [`Announce::verify`].
pub fn decode(data: &[u8]) -> Option<Announce> {
    if data.len() < MAGIC.len() + 1 || &data[..MAGIC.len()] != MAGIC {
        return None;
    }
    let version = data[MAGIC.len()];
    if !(MIN_SUPPORTED_VERSION..=CURRENT_VERSION).contains(&version) {
        return None;
    }
    let body = &data[MAGIC.len() + 1..];
    match version {
        2 => decode_v2(body),
        // Unreachable today; the range check above gates `version`. New arms (e.g.
        // `3 => decode_v3(body)`) are added here as the body layout evolves.
        _ => None,
    }
}

/// Parse a v2 body. Strict parse: reject trailing bytes (fail closed). The fixint
/// encoding matches `bincode::serialize` in `encode_v2`, so valid announces decode.
fn decode_v2(body: &[u8]) -> Option<Announce> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize(body)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_announce_verifies() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.user_id, id.public().user_id());
        assert_eq!(a.public(), id.public());
    }

    #[test]
    fn tampered_signature_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.sig[0] ^= 0xFF;
        assert!(!a.verify());
    }

    #[test]
    fn mismatched_user_id_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.user_id = "0000000000000000000000000000000000000000000000000000000000000000".to_string();
        assert!(!a.verify());
    }

    #[test]
    fn tampered_field_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.tcp_port = 9999; // not what was signed
        assert!(!a.verify());
    }

    #[test]
    fn encode_decode_round_trips() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        let bytes = encode(&a);
        let back = decode(&bytes).expect("decodes");
        assert_eq!(back, a);
        assert!(back.verify());
    }

    #[test]
    fn decode_rejects_bad_framing() {
        assert!(decode(&[]).is_none());
        assert!(decode(b"XXXX\x01garbage").is_none()); // bad magic
        assert!(decode(b"MTAN\x02garbage").is_none()); // valid version, garbage body
    }

    #[test]
    fn decode_rejects_unsupported_versions() {
        // A version below MIN_SUPPORTED or above CURRENT is rejected outright, even
        // with an otherwise-valid v2 body — multi-version dispatch fails closed.
        let id = DeviceIdentity::generate();
        let framed = encode(&Announce::new(&id, "Alice", 4000));
        let body = &framed[MAGIC.len() + 1..]; // the valid v2 body bytes

        for bad_version in [0u8, 1, CURRENT_VERSION + 1, 255] {
            let mut data = MAGIC.to_vec();
            data.push(bad_version);
            data.extend_from_slice(body);
            assert!(
                decode(&data).is_none(),
                "version {bad_version} must be rejected"
            );
        }
        // The genuine CURRENT_VERSION framing still round-trips byte-identically.
        assert_eq!(decode(&framed).unwrap(), Announce::new(&id, "Alice", 4000));
    }

    #[test]
    fn current_announce_encodes_to_golden_bytes() {
        // Pins the v2 wire layout: a hand-built announce with fixed fields (no
        // account cert, fixed 64-byte sig) must encode to these exact bytes. If a
        // field is reordered/added/removed, this fails — protecting deployed peers.
        let a = Announce {
            user_id: "u".to_string(),
            ed25519_pub: [0xAA; 32],
            x25519_pub: [0xBB; 32],
            name: "N".to_string(),
            tcp_port: 4000,
            post_office: false,
            account_cert: None,
            sig: vec![0xCC; 64],
        };
        let golden = "4d54414e02010000000000000075\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\
01000000000000004ea00f0000\
4000000000000000\
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        assert_eq!(hex::encode(encode(&a)), golden);
        // And it decodes back to the same value (round-trip on the golden input).
        assert_eq!(decode(&encode(&a)).unwrap(), a);
    }

    #[test]
    fn announce_from_a_does_not_verify_under_b() {
        // Take A's signed announce, swap in B's identity keys + user_id (so the
        // user_id still matches its ed25519_pub) but keep A's signature → fails.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let a_announce = Announce::new(&a, "Alice", 4000);
        let spoofed = Announce {
            user_id: b.public().user_id(),
            ed25519_pub: b.public().ed25519_pub,
            x25519_pub: a_announce.x25519_pub,
            name: a_announce.name.clone(),
            tcp_port: a_announce.tcp_port,
            post_office: a_announce.post_office,
            account_cert: a_announce.account_cert.clone(),
            sig: a_announce.sig.clone(),
        };
        assert!(!spoofed.verify());
    }

    #[test]
    fn tampered_x25519_key_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Alice", 4000);
        a.x25519_pub[0] ^= 0xFF; // the DH key is a signed field
        assert!(!a.verify());
    }

    #[test]
    fn decode_rejects_valid_framing_with_garbage_body() {
        assert!(decode(b"MTAN\x01\xFF\xFF\xFF\xFF").is_none());
    }

    #[test]
    fn decode_rejects_trailing_bytes() {
        let id = DeviceIdentity::generate();
        let mut bytes = encode(&Announce::new(&id, "Alice", 4000));
        bytes.push(0xAB); // junk appended after a valid announce
        assert!(decode(&bytes).is_none());
    }

    #[test]
    fn post_office_role_round_trips_and_is_signed() {
        let id = DeviceIdentity::generate();
        let normal = Announce::new(&id, "Node", 4000);
        assert!(!normal.post_office);
        assert!(normal.verify());
        // A normal announce wire-round-trips with the flag still false.
        let back_normal = decode(&encode(&normal)).expect("decodes normal");
        assert!(!back_normal.post_office);

        let po = Announce::new_post_office(&id, "Relay", 4000);
        assert!(po.post_office);
        assert!(po.verify());

        // The role survives the wire round-trip.
        let back = decode(&encode(&po)).expect("decodes");
        assert!(back.post_office);
        assert!(back.verify());
    }

    #[test]
    fn flipping_the_post_office_bit_fails_verify() {
        let id = DeviceIdentity::generate();
        let mut a = Announce::new(&id, "Node", 4000);
        a.post_office = true; // not what was signed
        assert!(!a.verify());
    }

    #[test]
    fn account_announce_round_trips_and_verifies() {
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let acct = Account::generate();
        let a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.account_id(), Some(acct.account_id()));
        let back = decode(&encode(&a)).expect("decodes");
        assert_eq!(back, a);
        assert!(back.verify());
        assert_eq!(back.account_id(), Some(acct.account_id()));
    }

    #[test]
    fn no_account_announce_has_no_account_id() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "Alice", 4000);
        assert!(a.verify());
        assert_eq!(a.account_id(), None);
    }

    #[test]
    fn swapping_in_a_foreign_account_cert_fails_verify() {
        // The attack: take a victim's account announce, replace the cert with one
        // an attacker validly minted over the victim's device key. cert.verify()
        // passes, but the device sig committed to the original account key → fail.
        use crate::identity::account::Account;
        let victim_device = DeviceIdentity::generate();
        let victim_account = Account::generate();
        let attacker_account = Account::generate();

        let mut a = Announce::new_with_account(&victim_device, &victim_account, "Victim", 4000);
        // Attacker can sign the victim's device key under their own account:
        let forged = attacker_account.certify(&victim_device.public().ed25519_pub);
        assert!(forged.verify()); // the cert itself is valid…
        a.account_cert = Some(forged); // …but the device never signed THIS account
        assert!(!a.verify());
    }

    #[test]
    fn a_cert_for_a_different_device_fails_verify() {
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let other_device = DeviceIdentity::generate();
        let acct = Account::generate();
        let mut a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        // Cert certifies a different device than the one announcing.
        a.account_cert = Some(acct.certify(&other_device.public().ed25519_pub));
        assert!(!a.verify());
    }

    #[test]
    fn stripping_the_account_cert_fails_verify() {
        // Removing the cert from an account-bound announce changes the signed
        // account key (Some → None) → device sig no longer matches.
        use crate::identity::account::Account;
        let id = DeviceIdentity::generate();
        let acct = Account::generate();
        let mut a = Announce::new_with_account(&id, &acct, "Alice", 4000);
        a.account_cert = None;
        assert!(!a.verify());
    }
}

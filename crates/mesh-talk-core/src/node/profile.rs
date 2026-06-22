//! The signed account PROFILE that propagates a user's custom avatar to peers.
//!
//! Avatars used to be local-only (the desktop `avatars.json`); a contact never saw the
//! photo you set. A `Profile` rides the SAME reliable per-device-pair sync that
//! account-addressed DMs use (sealed to each peer device, appended as an
//! `EventKind::Profile` event, delivered direct + replicated to the post office), so it
//! converges exactly like a message rather than via a fragile one-shot push.
//!
//! It is authenticated by the ACCOUNT key (not the device key): the account signs
//! `DOMAIN ‖ account_ed25519_pub ‖ updated_at ‖ avatar`, so a peer device cannot forge
//! a profile "from" another account. The receiver additionally binds the payload's
//! `account_id` to the AUTHENTICATED sending device's certified account (the same
//! re-homing defense `emit_new_messages` applies to account DMs).
//!
//! Bounds: the avatar is a small data-URL JPEG/PNG (~10–30KB the UI produces); anything
//! over [`MAX_AVATAR_BYTES`] is rejected. The receiver keeps ONE profile per account
//! (newest `updated_at` wins), so a peer spamming updates can't grow storage without
//! bound. `avatar = None` propagates a CLEARED avatar (revert to the glyph).

use crate::identity::account::{Account, AccountPublic};
use crate::identity::device::DeviceIdentity;
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Domain separator for the profile signature.
const PROFILE_DOMAIN: &[u8] = b"mesh-talk-profile-v1";

/// Frames the sealed plaintext so a profile is distinguishable from a `MessageBody` /
/// `DmEnvelope` (which share the DM transport conversation).
const PROFILE_MAGIC: &[u8] = b"MTPF1";

/// Upper bound on a propagated avatar (the data-URL bytes). The UI resizes photos to
/// ~10–30KB; 128KB is generous headroom while still rejecting an abusive blob.
pub const MAX_AVATAR_BYTES: usize = 128 * 1024;

/// serde helper for the 64-byte signature (serde derives arrays only up to len 32).
mod sig_bytes {
    use serde::de::{Error, SeqAccess, Visitor};
    use serde::ser::SerializeTuple;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(sig: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        let mut t = s.serialize_tuple(64)?;
        for b in sig {
            t.serialize_element(b)?;
        }
        t.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        struct SigVisitor;
        impl<'de> Visitor<'de> for SigVisitor {
            type Value = [u8; 64];
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a 64-byte signature")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<[u8; 64], A::Error> {
                let mut out = [0u8; 64];
                for (i, slot) in out.iter_mut().enumerate() {
                    *slot = seq
                        .next_element()?
                        .ok_or_else(|| Error::invalid_length(i, &self))?;
                }
                Ok(out)
            }
        }
        d.deserialize_tuple(64, SigVisitor)
    }
}

/// The bytes the account key signs to bind the avatar to the account + version:
/// `DOMAIN ‖ account_pub ‖ updated_at(le) ‖ avatar_len(le) ‖ avatar`. `avatar_len`
/// distinguishes "no avatar" (`None` → len 0) from an empty present avatar isn't a
/// concern (an empty data-URL is meaningless), so `None` is encoded as len 0.
fn signing_input(account_pub: &[u8; 32], updated_at: u64, avatar: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(PROFILE_DOMAIN.len() + 32 + 8 + 8 + avatar.len());
    m.extend_from_slice(PROFILE_DOMAIN);
    m.extend_from_slice(account_pub);
    m.extend_from_slice(&updated_at.to_le_bytes());
    m.extend_from_slice(&(avatar.len() as u64).to_le_bytes());
    m.extend_from_slice(avatar);
    m
}

/// A signed account profile: the account's avatar (data-URL bytes, or `None` to clear),
/// versioned by `updated_at` and authenticated by the account key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfilePayload {
    /// The signing account's Ed25519 public key (its `account_id` derives from this).
    pub account_ed25519_pub: [u8; 32],
    /// Wall-clock millis the avatar was set; newest wins on the receiver.
    pub updated_at: u64,
    /// The avatar data-URL bytes, or `None` to clear (revert to the glyph).
    pub avatar: Option<Vec<u8>>,
    #[serde(with = "sig_bytes")]
    pub signature: [u8; 64],
}

impl ProfilePayload {
    /// Build and sign a profile for `account`. `avatar` is the data-URL bytes (or `None`
    /// to clear). The caller is responsible for the size bound on a SET (see
    /// [`MAX_AVATAR_BYTES`]); this only signs.
    pub fn new(account: &Account, avatar: Option<Vec<u8>>, updated_at: u64) -> Self {
        let account_ed25519_pub = account.public().ed25519_pub;
        let avatar_bytes = avatar.as_deref().unwrap_or(&[]);
        let signature = account.sign(&signing_input(
            &account_ed25519_pub,
            updated_at,
            avatar_bytes,
        ));
        ProfilePayload {
            account_ed25519_pub,
            updated_at,
            avatar,
            signature,
        }
    }

    /// The account id this profile claims (only meaningful once [`Self::verify`] is true).
    pub fn account_id(&self) -> String {
        AccountPublic {
            ed25519_pub: self.account_ed25519_pub,
        }
        .account_id()
    }

    /// True if the signature is a valid account binding AND any present avatar is within
    /// the size bound. A `None` (cleared) avatar is always within bound.
    pub fn verify(&self) -> bool {
        if let Some(a) = &self.avatar {
            if a.len() > MAX_AVATAR_BYTES {
                return false;
            }
        }
        let avatar_bytes = self.avatar.as_deref().unwrap_or(&[]);
        DeviceIdentity::verify(
            &self.account_ed25519_pub,
            &signing_input(&self.account_ed25519_pub, self.updated_at, avatar_bytes),
            &self.signature,
        )
    }

    /// `PROFILE_MAGIC ‖ bincode(self)` — the plaintext sealed to a peer device.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(PROFILE_MAGIC.len() + 96);
        out.extend_from_slice(PROFILE_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("profile payload serializes"),
        );
        out
    }

    /// Recover a profile from opened plaintext, or `None` if the bytes are not a framed
    /// profile (so the message/file decoders can fall through to their own framings).
    pub fn decode(bytes: &[u8]) -> Option<ProfilePayload> {
        let rest = bytes.strip_prefix(PROFILE_MAGIC)?;
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize::<ProfilePayload>(rest)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_verifies() {
        let account = Account::generate();
        let p = ProfilePayload::new(&account, Some(b"data:image/jpeg;base64,AAAA".to_vec()), 100);
        assert!(p.verify());
        assert_eq!(p.account_id(), account.account_id());
        assert_eq!(ProfilePayload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn cleared_avatar_round_trips_and_verifies() {
        let account = Account::generate();
        let p = ProfilePayload::new(&account, None, 200);
        assert!(p.verify());
        assert_eq!(ProfilePayload::decode(&p.encode()), Some(p));
    }

    #[test]
    fn tampered_avatar_fails_verification() {
        let account = Account::generate();
        let mut p = ProfilePayload::new(&account, Some(b"original".to_vec()), 100);
        p.avatar = Some(b"swapped".to_vec());
        assert!(!p.verify(), "a swapped avatar must break the signature");
    }

    #[test]
    fn forged_account_pub_fails_verification() {
        let real = Account::generate();
        let other = Account::generate();
        let mut p = ProfilePayload::new(&real, Some(b"x".to_vec()), 100);
        // Claim a different account's key but keep the real signature → mismatch.
        p.account_ed25519_pub = other.public().ed25519_pub;
        assert!(!p.verify());
    }

    #[test]
    fn oversize_avatar_fails_verification() {
        let account = Account::generate();
        let big = vec![0u8; MAX_AVATAR_BYTES + 1];
        // Sign it honestly; verify must still reject it on the size bound.
        let p = ProfilePayload::new(&account, Some(big), 100);
        assert!(!p.verify(), "an oversize avatar must be rejected");
    }

    #[test]
    fn updated_at_is_bound_into_the_signature() {
        let account = Account::generate();
        let mut p = ProfilePayload::new(&account, Some(b"x".to_vec()), 100);
        p.updated_at = 999; // replay under a newer version
        assert!(!p.verify());
    }

    #[test]
    fn not_a_profile_decodes_none() {
        assert_eq!(ProfilePayload::decode(b"MTB1 some message body"), None);
        assert_eq!(ProfilePayload::decode(b""), None);
    }
}

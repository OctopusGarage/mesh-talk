//! Account identity (multi-device): an Ed25519 account keypair shared across a
//! user's devices, and device certificates (the account key signs each device's
//! public key, binding the device to the account). The `account_id` is the user's
//! stable cross-device handle. Pure crypto — persistence + networking are later plans.

use crate::identity::device::DeviceIdentity;
use bincode::Options;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain separator for the device-certificate signed message.
const CERT_DOMAIN: &[u8] = b"mesh-talk-device-cert-v1";

/// serde helper for the 64-byte signature (serde derives arrays only up to len 32,
/// so `[u8; 64]` needs a manual tuple (de)serialization).
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

/// Build the message the account signs to certify a device: `DOMAIN ‖ account_pub ‖
/// device_pub`. Binding both keys prevents a cert being replayed under another account.
fn cert_message(account_pub: &[u8; 32], device_pub: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(CERT_DOMAIN.len() + 64);
    m.extend_from_slice(CERT_DOMAIN);
    m.extend_from_slice(account_pub);
    m.extend_from_slice(device_pub);
    m
}

/// The public half of an account (its Ed25519 verifying key).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountPublic {
    pub ed25519_pub: [u8; 32],
}

impl AccountPublic {
    /// Stable account id = hex of the first 16 bytes of SHA-256("mesh-talk-account-v1"
    /// ‖ ed25519_pub). 32 hex chars. The user's cross-device handle.
    pub fn account_id(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"mesh-talk-account-v1");
        h.update(self.ed25519_pub);
        hex::encode(&h.finalize()[..16])
    }
}

/// A user's account secret (Ed25519). Held in memory; persisted via the keystore
/// (a later plan). Shared across the user's devices.
pub struct Account {
    signing_key: SigningKey,
}

impl Account {
    pub fn generate() -> Self {
        Account {
            signing_key: SigningKey::generate(&mut rand_core::OsRng),
        }
    }

    pub fn from_secret_bytes(ed25519_secret: [u8; 32]) -> Self {
        Account {
            signing_key: SigningKey::from_bytes(&ed25519_secret),
        }
    }

    pub fn secret_bytes(&self) -> [u8; 32] {
        self.signing_key.to_bytes()
    }

    pub fn public(&self) -> AccountPublic {
        AccountPublic {
            ed25519_pub: self.signing_key.verifying_key().to_bytes(),
        }
    }

    pub fn account_id(&self) -> String {
        self.public().account_id()
    }

    /// Sign an arbitrary (already domain-separated) message with the account key.
    /// Used to authenticate account-level artifacts like the profile/avatar so a peer
    /// device cannot forge another account's profile.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }

    /// Certify a device: sign the device's Ed25519 public key with the account key.
    pub fn certify(&self, device_ed25519_pub: &[u8; 32]) -> DeviceCertificate {
        let account_pub = self.public().ed25519_pub;
        let msg = cert_message(&account_pub, device_ed25519_pub);
        let signature = self.signing_key.sign(&msg).to_bytes();
        DeviceCertificate {
            account_ed25519_pub: account_pub,
            device_ed25519_pub: *device_ed25519_pub,
            signature,
        }
    }
}

/// Proof that a device belongs to an account: the account key's signature over the
/// device's public key. Advertised on the network alongside the device identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificate {
    pub account_ed25519_pub: [u8; 32],
    pub device_ed25519_pub: [u8; 32],
    #[serde(with = "sig_bytes")]
    pub signature: [u8; 64],
}

impl DeviceCertificate {
    /// True if the signature is a valid account-over-device binding.
    pub fn verify(&self) -> bool {
        let msg = cert_message(&self.account_ed25519_pub, &self.device_ed25519_pub);
        DeviceIdentity::verify(&self.account_ed25519_pub, &msg, &self.signature)
    }

    /// The account id this cert binds to (only meaningful if `verify()` is true).
    pub fn account_id(&self) -> String {
        AccountPublic {
            ed25519_pub: self.account_ed25519_pub,
        }
        .account_id()
    }

    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("device cert serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_certifies_a_device_and_verifies() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let cert = account.certify(&device.public().ed25519_pub);
        assert!(cert.verify());
        assert_eq!(cert.account_id(), account.account_id());
    }

    #[test]
    fn a_tampered_cert_fails() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let mut cert = account.certify(&device.public().ed25519_pub);
        cert.device_ed25519_pub[0] ^= 0xFF; // claim a different device
        assert!(!cert.verify());
    }

    #[test]
    fn a_cert_from_a_different_account_doesnt_verify_as_ours() {
        let account = Account::generate();
        let other = Account::generate();
        let device = DeviceIdentity::generate();
        // `other` signs, but the cert claims `account`'s key → signature mismatch.
        let real = other.certify(&device.public().ed25519_pub);
        let mut forged = real.clone();
        forged.account_ed25519_pub = account.public().ed25519_pub;
        assert!(!forged.verify());
    }

    #[test]
    fn account_round_trips_from_secret_bytes() {
        let account = Account::generate();
        let restored = Account::from_secret_bytes(account.secret_bytes());
        assert_eq!(restored.account_id(), account.account_id());
        // A cert from the restored account still verifies.
        let device = DeviceIdentity::generate();
        assert!(restored.certify(&device.public().ed25519_pub).verify());
    }

    #[test]
    fn cert_codec_round_trips_and_rejects_trailing() {
        let account = Account::generate();
        let device = DeviceIdentity::generate();
        let cert = account.certify(&device.public().ed25519_pub);
        assert_eq!(
            DeviceCertificate::decode(&cert.encode()),
            Some(cert.clone())
        );
        let mut junk = cert.encode();
        junk.push(0xAB);
        assert_eq!(DeviceCertificate::decode(&junk), None);
    }
}

//! Optional session-bound account proof appended to the unchanged legacy auth prefix.
use super::{
    auth::{build_auth, verify_auth, AuthMessage},
    TransportError, MAX_PLAINTEXT,
};
use crate::{
    discovery::{
        announce::{decode, encode},
        Announce,
    },
    identity::device::{DeviceIdentity, PublicIdentity},
};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::io::Cursor;

const MAGIC: &[u8; 4] = b"MTPA";
const VERSION: u8 = 1;
const MAX_EXTENSION: usize = 4096;
const DOMAIN: &[u8] = b"mesh-talk-transport-presence-v1";

/// A Noise-authenticated device and its optional, session-bound signed announcement.
/// Construction is private so admission predicates receive verified proofs only.
/// A missing announcement is a legacy device, not proof of any account membership.
#[derive(Debug, Clone)]
pub struct VerifiedPeer {
    public: PublicIdentity,
    announcement: Option<Announce>,
}
impl VerifiedPeer {
    /// The exact Ed25519/X25519 pair authenticated by this connection.
    pub fn public_identity(&self) -> &PublicIdentity {
        &self.public
    }
    /// Verified device-signed account proof; does not establish endpoint freshness.
    pub fn announcement(&self) -> Option<&Announce> {
        self.announcement.as_ref()
    }
}

#[derive(Serialize, Deserialize)]
struct Extension {
    announce: Vec<u8>,
    signature: Vec<u8>,
}

fn context(hash: &[u8; 32], encoded: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(DOMAIN.len() + hash.len() + encoded.len());
    bytes.extend(DOMAIN);
    bytes.extend(hash);
    bytes.extend(encoded);
    bytes
}
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_EXTENSION as u64)
        .reject_trailing_bytes()
}
pub(super) fn validate_own(
    identity: &DeviceIdentity,
    presence: Option<&Announce>,
) -> Result<(), TransportError> {
    if presence.is_some_and(|announce| !valid(announce, &identity.public())) {
        return Err(TransportError::IdentityMismatch);
    }
    Ok(())
}
fn valid(announce: &Announce, public: &PublicIdentity) -> bool {
    announce.name.len() <= 1024
        && announce.tcp_port != 0
        && announce.account_cert.is_some()
        && announce.public() == *public
        && announce.verify()
}
pub(super) fn build(
    identity: &DeviceIdentity,
    hash: &[u8; 32],
    presence: Option<&Announce>,
) -> Result<Vec<u8>, TransportError> {
    validate_own(identity, presence)?;
    let mut bytes = bincode::serialize(&build_auth(identity, hash))
        .map_err(|e| TransportError::Serialization(e.to_string()))?;
    if let Some(announce) = presence {
        let encoded = encode(announce);
        let signature = identity.sign(&context(hash, &encoded)).to_vec();
        let body = codec()
            .serialize(&Extension {
                announce: encoded,
                signature,
            })
            .map_err(|e| TransportError::Serialization(e.to_string()))?;
        if MAGIC.len() + 1 + body.len() > MAX_EXTENSION {
            return Err(TransportError::IdentityMismatch);
        }
        bytes.extend(MAGIC);
        bytes.push(VERSION);
        bytes.extend(body);
    }
    Ok(bytes)
}
pub(super) fn verify(
    bytes: &[u8],
    hash: &[u8; 32],
    remote: &[u8; 32],
) -> Result<VerifiedPeer, TransportError> {
    if bytes.len() > MAX_PLAINTEXT {
        return Err(TransportError::PlaintextTooLarge(bytes.len()));
    }
    // Cursor retains the consumed prefix length. The legacy format intentionally
    // accepts trailing bytes, so old peers can still authenticate new senders.
    let mut cursor = Cursor::new(bytes);
    let legacy: AuthMessage = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .allow_trailing_bytes()
        .deserialize_from(&mut cursor)
        .map_err(|_| TransportError::IdentityMismatch)?;
    let public = verify_auth(&legacy, hash, remote)?;
    let remaining = &bytes[cursor.position() as usize..];
    let announcement = if remaining.is_empty() {
        None
    } else {
        if remaining.len() > MAX_EXTENSION
            || remaining.len() < MAGIC.len() + 1
            || &remaining[..MAGIC.len()] != MAGIC
            || remaining[MAGIC.len()] != VERSION
        {
            return Err(TransportError::IdentityMismatch);
        }
        let extension: Extension = codec()
            .deserialize(&remaining[MAGIC.len() + 1..])
            .map_err(|_| TransportError::IdentityMismatch)?;
        let announce = decode(&extension.announce).ok_or(TransportError::IdentityMismatch)?;
        if !valid(&announce, &public) {
            return Err(TransportError::IdentityMismatch);
        }
        let signature: [u8; 64] = extension
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| TransportError::IdentityMismatch)?;
        if !DeviceIdentity::verify(
            &public.ed25519_pub,
            &context(hash, &extension.announce),
            &signature,
        ) {
            return Err(TransportError::IdentityMismatch);
        }
        Some(announce)
    };
    Ok(VerifiedPeer {
        public,
        announcement,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        identity::account::Account,
        transport::auth::{build_auth, AuthMessage},
    };

    fn signed_extension(
        identity: &DeviceIdentity,
        hash: &[u8; 32],
        announce: &Announce,
    ) -> Vec<u8> {
        let encoded = crate::discovery::announce::encode(announce);
        let mut input = b"mesh-talk-transport-presence-v1".to_vec();
        input.extend(hash);
        input.extend(&encoded);
        let signature = identity.sign(&input).to_vec();
        let mut bytes = bincode::serialize(&build_auth(identity, hash)).unwrap();
        bytes.extend(b"MTPA\x01");
        bytes.extend(bincode::serialize(&(encoded, signature)).unwrap());
        bytes
    }

    #[test]
    fn peer_validation_rejects_signed_cert_swap_wrong_device_keys_and_bad_announce() {
        let id = DeviceIdentity::generate();
        let account = Account::generate();
        let good = Announce::new_with_account(&id, &account, "Alice", 1234);
        let mut forged = good.clone();
        forged.account_cert = Some(Account::generate().certify(&good.ed25519_pub));
        let mut wrong_x = good.clone();
        wrong_x.x25519_pub[0] ^= 1;
        let alternate_x = DeviceIdentity::from_secret_bytes(
            id.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        for bad in [
            forged,
            wrong_x,
            Announce::new_with_account(&alternate_x, &account, "Alice", 1234),
            Announce::new_with_account(&DeviceIdentity::generate(), &account, "Alice", 1234),
            Announce::new(&id, "Alice", 1234),
            Announce::new_with_account(&id, &account, "Alice", 0),
            Announce::new_with_account(&id, &account, "a".repeat(1025), 1234),
        ] {
            let bytes = signed_extension(&id, &[42; 32], &bad);
            assert!(verify(&bytes, &[42; 32], &id.public().x25519_pub).is_err());
        }
        assert!(verify(
            &signed_extension(&id, &[42; 32], &good),
            &[42; 32],
            &id.public().x25519_pub
        )
        .is_ok());
    }

    #[test]
    fn verified_presence_and_exact_legacy_prefix_roundtrip() {
        let id = DeviceIdentity::generate();
        let account = Account::generate();
        let announce = Announce::new_with_account(&id, &account, "Alice", 1234);
        let hash = [42; 32];
        let legacy = bincode::serialize(&build_auth(&id, &hash)).unwrap();
        assert_eq!(build(&id, &hash, None).unwrap(), legacy);
        let bytes = build(&id, &hash, Some(&announce)).unwrap();
        assert!(bytes.starts_with(&legacy));
        let old: AuthMessage = bincode::deserialize(&bytes).unwrap();
        assert_eq!(old.public, id.public());
        assert_eq!(old.signature, build_auth(&id, &hash).signature);
        let verified = verify(&bytes, &hash, &id.public().x25519_pub).unwrap();
        assert_eq!(verified.public_identity(), &id.public());
        assert_eq!(verified.announcement(), Some(&announce));
        assert!(verify(&legacy, &hash, &id.public().x25519_pub)
            .unwrap()
            .announcement()
            .is_none());
    }

    #[test]
    fn rejects_bad_own_proofs_and_unsigned_or_unbounded_presence() {
        let id = DeviceIdentity::generate();
        let account = Account::generate();
        let good = Announce::new_with_account(&id, &account, "Alice", 1234);
        let mut cert_swap = good.clone();
        cert_swap.account_cert = Some(Account::generate().certify(&good.ed25519_pub));
        let mut wrong_x = good.clone();
        wrong_x.x25519_pub[0] ^= 1;
        let mut tamper = good.clone();
        tamper.name.push('!');
        for invalid in [
            cert_swap,
            wrong_x,
            tamper,
            Announce::new(&id, "Alice", 1234),
            Announce::new_with_account(&DeviceIdentity::generate(), &account, "Alice", 1234),
            Announce::new_with_account(&id, &account, "Alice", 0),
            Announce::new_with_account(&id, &account, "界".repeat(342), 1234),
        ] {
            assert!(build(&id, &[42; 32], Some(&invalid)).is_err());
        }
        assert!(build(
            &id,
            &[42; 32],
            Some(&Announce::new_with_account(
                &id,
                &account,
                "a".repeat(1024),
                1234
            ))
        )
        .is_ok());
    }

    #[test]
    fn strict_extension_parser_rejects_trailing_unknown_versions_and_malformed_payloads() {
        let id = DeviceIdentity::generate();
        let hash = [42; 32];
        let announce = Announce::new_with_account(&id, &Account::generate(), "Alice", 1234);
        let valid = build(&id, &hash, Some(&announce)).unwrap();
        let prefix = bincode::serialize(&build_auth(&id, &hash)).unwrap().len();
        let mut trailing = valid.clone();
        trailing.push(0);
        let mut version = valid.clone();
        version[prefix + 4] = 99;
        let mut magic = valid.clone();
        magic[prefix] ^= 1;
        let mut tampered = valid.clone();
        *tampered.last_mut().unwrap() ^= 1;
        let mut absurd = valid[..prefix + 5].to_vec();
        absurd.extend(u64::MAX.to_le_bytes());
        for invalid in [
            trailing,
            version,
            magic,
            tampered,
            valid[..prefix + 4].to_vec(),
            absurd,
            [valid[..prefix].to_vec(), vec![0; 4097]].concat(),
        ] {
            assert!(verify(&invalid, &hash, &id.public().x25519_pub).is_err());
        }
        assert!(verify(&valid, &[43; 32], &id.public().x25519_pub).is_err());
        assert!(verify(&valid, &hash, &[0; 32]).is_err());
    }

    #[test]
    fn extension_cannot_be_replayed_onto_fresh_valid_legacy_auth() {
        let id = DeviceIdentity::generate();
        let announce = Announce::new_with_account(&id, &Account::generate(), "Alice", 1234);
        let first = build(&id, &[1; 32], Some(&announce)).unwrap();
        let prefix = bincode::serialize(&build_auth(&id, &[1; 32]))
            .unwrap()
            .len();
        let mut replay = bincode::serialize(&build_auth(&id, &[2; 32])).unwrap();
        replay.extend(&first[prefix..]);
        assert!(verify(&replay, &[2; 32], &id.public().x25519_pub).is_err());
    }
}

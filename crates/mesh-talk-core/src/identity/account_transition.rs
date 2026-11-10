//! Public, exact-device account changes authorized by both account roots.
use super::{
    account::{Account, DeviceCertificate},
    device::{DeviceIdentity, PublicIdentity},
};
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBindingTransition {
    pub public: PublicIdentity,
    pub previous_certificate: DeviceCertificate,
    pub current_certificate: DeviceCertificate,
    pub previous_transition: Option<[u8; 32]>,
    pub nonce: [u8; 32],
    pub old_signature: Vec<u8>,
    pub new_signature: Vec<u8>,
    pub device_signature: Vec<u8>,
}
impl AccountBindingTransition {
    pub fn create(
        old: &Account,
        new: &Account,
        device: &DeviceIdentity,
        previous: Option<[u8; 32]>,
    ) -> Self {
        let mut nonce = [0; 32];
        rand_core::OsRng.fill_bytes(&mut nonce);
        let mut transition = Self {
            public: device.public(),
            previous_certificate: old.certify(&device.public().ed25519_pub),
            current_certificate: new.certify(&device.public().ed25519_pub),
            previous_transition: previous,
            nonce,
            old_signature: Vec::new(),
            new_signature: Vec::new(),
            device_signature: Vec::new(),
        };
        transition.old_signature = old.sign(&transition.message(b"old-root")).to_vec();
        transition.new_signature = new.sign(&transition.message(b"new-root")).to_vec();
        transition.device_signature = device.sign(&transition.message(b"device")).to_vec();
        transition
    }
    fn message(&self, role: &[u8]) -> Vec<u8> {
        let mut message = b"mesh-talk-account-binding-transition-v1".to_vec();
        message.push(1);
        message.push(role.len() as u8);
        message.extend_from_slice(role);
        message.extend_from_slice(&self.public.ed25519_pub);
        message.extend_from_slice(&self.public.x25519_pub);
        message.extend(self.previous_certificate.encode());
        message.extend(self.current_certificate.encode());
        message.push(u8::from(self.previous_transition.is_some()));
        message.extend(self.previous_transition.unwrap_or_default());
        message.extend(self.nonce);
        message
    }
    pub fn verify(&self) -> bool {
        if self.previous_certificate.account_ed25519_pub
            == self.current_certificate.account_ed25519_pub
            || self.previous_certificate.device_ed25519_pub != self.public.ed25519_pub
            || self.current_certificate.device_ed25519_pub != self.public.ed25519_pub
            || !self.previous_certificate.verify()
            || !self.current_certificate.verify()
        {
            return false;
        }
        [
            (
                &self.previous_certificate.account_ed25519_pub,
                b"old-root".as_slice(),
                &self.old_signature,
            ),
            (
                &self.current_certificate.account_ed25519_pub,
                b"new-root".as_slice(),
                &self.new_signature,
            ),
            (
                &self.public.ed25519_pub,
                b"device".as_slice(),
                &self.device_signature,
            ),
        ]
        .into_iter()
        .all(|(public, role, signature)| {
            let Ok(signature) = <[u8; 64]>::try_from(signature.as_slice()) else {
                return false;
            };
            DeviceIdentity::verify(public, &self.message(role), &signature)
        })
    }
    pub fn id(&self) -> [u8; 32] {
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-account-binding-transition-id-v1");
        hash.update(self.message(b"id"));
        for signature in [
            &self.old_signature,
            &self.new_signature,
            &self.device_signature,
        ] {
            hash.update((signature.len() as u64).to_be_bytes());
            hash.update(signature);
        }
        hash.finalize().into()
    }
    pub fn previous_account_id(&self) -> String {
        self.previous_certificate.account_id()
    }
    pub fn current_account_id(&self) -> String {
        self.current_certificate.account_id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transition_requires_both_roots_and_exact_device_authorization() {
        let old = Account::generate();
        let new = Account::generate();
        let device = DeviceIdentity::generate();
        let transition = AccountBindingTransition::create(&old, &new, &device, None);
        assert!(
            transition.verify(),
            "an explicitly authorized account change must verify"
        );
        assert_eq!(transition.previous_account_id(), old.account_id());
        assert_eq!(transition.current_account_id(), new.account_id());
        for field in 0..9 {
            let mut forged = transition.clone();
            match field {
                0 => forged.public.x25519_pub[0] ^= 1,
                1 => forged.public.ed25519_pub[0] ^= 1,
                2 => {
                    forged.previous_certificate =
                        Account::generate().certify(&device.public().ed25519_pub)
                }
                3 => {
                    forged.current_certificate =
                        Account::generate().certify(&device.public().ed25519_pub)
                }
                4 => forged.previous_transition = Some([9; 32]),
                5 => forged.nonce[0] ^= 1,
                6 => forged.old_signature = vec![0; 64],
                7 => forged.new_signature = vec![0; 64],
                _ => forged.device_signature = vec![0; 64],
            }
            assert!(
                !forged.verify(),
                "every bound identity, predecessor and signature is required"
            );
            assert_ne!(forged.id(), transition.id());
        }
        assert!(!AccountBindingTransition::create(&old, &old, &device, None).verify());
        let next = AccountBindingTransition::create(
            &new,
            &Account::generate(),
            &device,
            Some(transition.id()),
        );
        assert!(next.verify());
        assert_eq!(next.previous_transition, Some(transition.id()));
    }
}

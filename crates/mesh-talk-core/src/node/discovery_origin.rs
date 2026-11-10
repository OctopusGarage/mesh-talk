//! Short-lived, explicitly consented registration-origin evidence.
//!
//! These signatures authenticate a registration claim, not a physical location.
//! Existing Noise channels carry the optional metadata; relays can read it.

use crate::identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};

pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub const ORIGIN_LEASE_SECS: u64 = 15;
const MAX_OFFICE_NAME_BYTES: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginConsent {
    participant: PublicIdentity,
    office: PublicIdentity,
    expires_at_secs: u64,
    signature: Vec<u8>,
}

fn signed_bytes(domain: &[u8], value: &impl Serialize) -> Vec<u8> {
    let mut bytes = domain.to_vec();
    bytes.extend(bincode::serialize(value).expect("bounded origin serialization"));
    bytes
}

fn verify(public: &PublicIdentity, bytes: &[u8], signature: &[u8]) -> bool {
    let Ok(signature) = <&[u8; 64]>::try_from(signature) else {
        return false;
    };
    DeviceIdentity::verify(&public.ed25519_pub, bytes, signature)
}

impl OriginConsent {
    pub fn new(
        participant: &DeviceIdentity,
        office: &PublicIdentity,
        expires_at_secs: u64,
    ) -> Self {
        let mut consent = Self {
            participant: participant.public(),
            office: office.clone(),
            expires_at_secs,
            signature: Vec::new(),
        };
        consent.signature = participant.sign(&consent.signing_bytes()).to_vec();
        consent
    }
    fn signing_bytes(&self) -> Vec<u8> {
        signed_bytes(
            b"mesh-talk-origin-consent-v1",
            &(&self.participant, &self.office, self.expires_at_secs),
        )
    }
    fn valid_for(&self, participant: &PublicIdentity, office: &PublicIdentity, now: u64) -> bool {
        self.participant == *participant
            && self.office == *office
            && self.expires_at_secs > now
            && self.expires_at_secs <= now.saturating_add(ORIGIN_LEASE_SECS)
            && verify(participant, &self.signing_bytes(), &self.signature)
    }
}

/// Participant-consented, office-attested registration origin. Never an address
/// or location proof, and never used as routing or messaging authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationOrigin {
    office: PublicIdentity,
    office_name: String,
    consent: OriginConsent,
    signature: Vec<u8>,
}

impl RegistrationOrigin {
    pub fn new(office: &DeviceIdentity, name: &str, consent: OriginConsent) -> Option<Self> {
        if office.public() != consent.office || !valid_name(name) {
            return None;
        }
        let mut proof = Self {
            office: office.public(),
            office_name: name.into(),
            consent,
            signature: Vec::new(),
        };
        proof.signature = office.sign(&proof.signing_bytes()).to_vec();
        Some(proof)
    }
    fn signing_bytes(&self) -> Vec<u8> {
        signed_bytes(
            b"mesh-talk-registration-origin-v1",
            &(&self.office, &self.office_name, &self.consent),
        )
    }
    pub fn valid_for(&self, participant: &PublicIdentity, now_secs: u64) -> bool {
        valid_name(&self.office_name)
            && self.consent.valid_for(participant, &self.office, now_secs)
            && verify(&self.office, &self.signing_bytes(), &self.signature)
    }
    pub(crate) fn is_from(&self, office: &PublicIdentity) -> bool {
        self.office == *office
    }
    pub fn office_user_id(&self) -> String {
        self.office.user_id()
    }
    pub fn office_name(&self) -> &str {
        &self.office_name
    }
    pub fn expires_at_secs(&self) -> u64 {
        self.consent.expires_at_secs
    }
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_OFFICE_NAME_BYTES
        && !name.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn registration_origin_requires_both_signatures_and_exact_binding() {
        let user = DeviceIdentity::generate();
        let office = DeviceIdentity::generate();
        let other = DeviceIdentity::generate();
        let consent = OriginConsent::new(&user, &office.public(), 100);
        let origin = RegistrationOrigin::new(&office, "Windows 10 office", consent).unwrap();
        assert!(origin.valid_for(&user.public(), 90));
        assert_eq!(origin.office_name(), "Windows 10 office");
        assert_eq!(origin.office_user_id(), office.user_id());
        assert!(!origin.valid_for(&other.public(), 90));
        assert!(!origin.valid_for(&user.public(), 100));
        assert!(!origin.valid_for(&user.public(), 84));
        let mut tampered = origin.clone();
        tampered.office_name = "Another site".into();
        assert!(!tampered.valid_for(&user.public(), 90));
        let mut tampered = origin;
        tampered.consent.expires_at_secs = 101;
        assert!(!tampered.valid_for(&user.public(), 90));
    }

    #[test]
    fn registration_origin_rejects_wrong_office_and_unbounded_names() {
        let user = DeviceIdentity::generate();
        let office = DeviceIdentity::generate();
        let other = DeviceIdentity::generate();
        let consent = OriginConsent::new(&user, &office.public(), 100);
        assert!(RegistrationOrigin::new(&other, "Other office", consent.clone()).is_none());
        assert!(RegistrationOrigin::new(&office, &"x".repeat(257), consent.clone()).is_none());
        assert!(RegistrationOrigin::new(&office, "Office\nInjected", consent).is_none());
    }
}

//! Operator-approved relay membership. Certificates authorize reconciliation;
//! they do not attest software, host integrity or conversation membership.
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::transport::SecureChannel;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncWrite};

const VERSION: u8 = 1;
const MAX_MEMBERSHIP_BYTES: u64 = 4096;
const MAX_POLICY_BYTES: u64 = 65_536;
const MAX_REVOKED: usize = 1024;
const MAX_VALIDITY: u64 = 90 * 86400;
const REQUEST: &[u8] = b"MTNM1Q";
const RESPONSE: &[u8] = b"MTNM1R";
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

fn fingerprint(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Public, locally approved trust snapshot. Distribute revocations to every relay
/// and restart it; a policy file is not a remotely accepted authority update.
#[derive(Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    version: u8,
    authority: [u8; 32],
    revoked: BTreeSet<String>,
}

impl NetworkPolicy {
    pub fn new(authority: [u8; 32]) -> Self {
        Self {
            version: VERSION,
            authority,
            revoked: BTreeSet::new(),
        }
    }

    pub fn authority(&self) -> [u8; 32] {
        self.authority
    }

    pub fn revoked(&self) -> impl Iterator<Item = &str> {
        self.revoked.iter().map(String::as_str)
    }

    pub fn revoke(&mut self, device_id: &str) -> Result<(), String> {
        if !fingerprint(device_id) {
            return Err("invalid revoked device fingerprint".into());
        }
        if self.revoked.len() >= MAX_REVOKED && !self.revoked.contains(device_id) {
            return Err("network policy exceeds 1024 revocations".into());
        }
        self.revoked.insert(device_id.to_owned());
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        bincode::DefaultOptions::new()
            .with_limit(MAX_POLICY_BYTES)
            .serialize(self)
            .map_err(|e| e.to_string())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_POLICY_BYTES {
            return Err("network policy is too large".into());
        }
        let policy: Self = bincode::DefaultOptions::new()
            .with_limit(MAX_POLICY_BYTES)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| format!("invalid network policy: {e}"))?;
        if policy.version != VERSION
            || policy.revoked.len() > MAX_REVOKED
            || policy.revoked.iter().any(|id| !fingerprint(id))
            || ed25519_dalek::VerifyingKey::from_bytes(&policy.authority).is_err()
        {
            return Err("invalid network policy".into());
        }
        Ok(policy)
    }
}

/// Nondelegating, expiring approval of one authenticated device fingerprint.
#[derive(Clone, Serialize, Deserialize)]
pub struct NetworkMembership {
    version: u8,
    authority: [u8; 32],
    device_id: String,
    issued_at: u64,
    expires_at: u64,
    signature: Vec<u8>,
}

impl NetworkMembership {
    pub fn issue(
        authority: &DeviceIdentity,
        device_id: &str,
        now: u64,
        validity: Duration,
    ) -> Result<Self, String> {
        if !fingerprint(device_id) || validity.as_secs() == 0 || validity.as_secs() > MAX_VALIDITY {
            return Err(
                "membership requires a full device fingerprint and 1 second to 90 days validity"
                    .into(),
            );
        }
        let mut certificate = Self {
            version: VERSION,
            authority: authority.public().ed25519_pub,
            device_id: device_id.to_owned(),
            issued_at: now,
            expires_at: now
                .checked_add(validity.as_secs())
                .ok_or("membership expiry overflow")?,
            signature: Vec::new(),
        };
        certificate.signature = authority.sign(&certificate.signing_bytes()).to_vec();
        Ok(certificate)
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-relay-membership-v1\0".to_vec();
        bytes.push(self.version);
        bytes.extend_from_slice(&self.authority);
        bytes.extend_from_slice(self.device_id.as_bytes());
        bytes.extend_from_slice(&self.issued_at.to_be_bytes());
        bytes.extend_from_slice(&self.expires_at.to_be_bytes());
        bytes
    }

    /// Check public approval without loading a relay's private identity. The
    /// transport caller must also bind device_id to the actual Noise peer.
    pub fn verify_approval(&self, policy: &NetworkPolicy, now: u64) -> Result<(), String> {
        let signature: &[u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| "invalid membership signature length")?;
        if self.version != VERSION
            || policy.version != VERSION
            || self.authority != policy.authority
            || !fingerprint(&self.device_id)
            || policy.revoked.contains(&self.device_id)
            || now < self.issued_at
            || now >= self.expires_at
            || self.expires_at.saturating_sub(self.issued_at) > MAX_VALIDITY
            || !DeviceIdentity::verify(&policy.authority, &self.signing_bytes(), signature)
        {
            return Err(
                "relay membership is invalid, expired, revoked or from another network".into(),
            );
        }
        Ok(())
    }

    pub fn verify(
        &self,
        policy: &NetworkPolicy,
        peer: &PublicIdentity,
        now: u64,
    ) -> Result<(), String> {
        self.verify_approval(policy, now)?;
        if self.device_id != peer.user_id() {
            return Err("membership does not match authenticated relay identity".into());
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        bincode::DefaultOptions::new()
            .with_limit(MAX_MEMBERSHIP_BYTES)
            .serialize(self)
            .map_err(|e| e.to_string())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() as u64 > MAX_MEMBERSHIP_BYTES {
            return Err("membership is too large".into());
        }
        bincode::DefaultOptions::new()
            .with_limit(MAX_MEMBERSHIP_BYTES)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .map_err(|e| e.to_string())
    }
}

/// A relay's local certificate plus its operator-approved startup policy.
#[derive(Clone)]
pub struct RelayNetwork {
    policy: NetworkPolicy,
    membership: NetworkMembership,
}

impl RelayNetwork {
    pub fn new(
        policy: NetworkPolicy,
        membership: NetworkMembership,
        own: &PublicIdentity,
        now: u64,
    ) -> Result<Self, String> {
        membership.verify(&policy, own, now)?;
        Ok(Self { policy, membership })
    }

    pub fn is_membership_request(bytes: &[u8]) -> bool {
        bytes.starts_with(REQUEST)
    }

    /// Authenticate the remote membership before disclosing inventory or presence.
    /// Never fall back to unauthenticated or pin-only reconciliation on failure.
    pub async fn connect<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<S>,
    ) -> Result<(), String> {
        self.membership.verify_approval(&self.policy, unix_now())?;
        let mut request = REQUEST.to_vec();
        request.extend(self.membership.encode()?);
        tokio::time::timeout(AUTH_TIMEOUT, async {
            channel.send(&request).await.map_err(|e| e.to_string())?;
            let response = channel.recv().await.map_err(|e| e.to_string())?;
            let bytes = response
                .strip_prefix(RESPONSE)
                .ok_or("missing relay membership response")?;
            NetworkMembership::decode(bytes)?.verify(
                &self.policy,
                channel.peer_identity(),
                unix_now(),
            )
        })
        .await
        .map_err(|_| "relay membership exchange timed out".to_string())?
    }

    pub(crate) async fn accept<S: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<S>,
        request: &[u8],
    ) -> Result<(), String> {
        self.membership.verify_approval(&self.policy, unix_now())?;
        let bytes = request
            .strip_prefix(REQUEST)
            .ok_or("missing relay membership request")?;
        NetworkMembership::decode(bytes)?.verify(
            &self.policy,
            channel.peer_identity(),
            unix_now(),
        )?;
        let mut response = RESPONSE.to_vec();
        response.extend(self.membership.encode()?);
        tokio::time::timeout(AUTH_TIMEOUT, channel.send(&response))
            .await
            .map_err(|_| "relay membership exchange timed out".to_string())?
            .map_err(|e| e.to_string())
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: u64 = 1_800_000_000;

    #[test]
    fn membership_requires_authority_identity_time_and_unrevoked_policy() {
        let authority = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mut policy = NetworkPolicy::new(authority.public().ed25519_pub);
        let certificate =
            NetworkMembership::issue(&authority, &relay.user_id(), NOW, Duration::from_secs(3600))
                .unwrap();
        assert!(certificate.verify(&policy, &relay.public(), NOW).is_ok());
        assert!(
            certificate
                .verify(&policy, &relay.public(), NOW - 1)
                .is_err()
        );
        assert!(
            certificate
                .verify(&policy, &relay.public(), NOW + 3600)
                .is_err()
        );
        assert!(
            certificate
                .verify(&policy, &DeviceIdentity::generate().public(), NOW)
                .is_err()
        );
        let wrong = NetworkPolicy::new(DeviceIdentity::generate().public().ed25519_pub);
        assert!(certificate.verify(&wrong, &relay.public(), NOW).is_err());
        policy.revoke(&relay.user_id()).unwrap();
        assert!(certificate.verify(&policy, &relay.public(), NOW).is_err());
        assert!(RelayNetwork::new(policy, certificate, &relay.public(), NOW).is_err());
    }

    #[test]
    fn tampering_and_unbounded_credentials_are_rejected() {
        let authority = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let policy = NetworkPolicy::new(authority.public().ed25519_pub);
        let certificate =
            NetworkMembership::issue(&authority, &relay.user_id(), NOW, Duration::from_secs(3600))
                .unwrap();
        let encoded = certificate.encode().unwrap();
        let decoded = NetworkMembership::decode(&encoded).unwrap();
        assert!(decoded.verify(&policy, &relay.public(), NOW).is_ok());
        let mut tampered = encoded.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(
            NetworkMembership::decode(&tampered)
                .unwrap()
                .verify(&policy, &relay.public(), NOW)
                .is_err()
        );
        let mut trailing = encoded;
        trailing.push(0);
        assert!(NetworkMembership::decode(&trailing).is_err());
        assert!(NetworkMembership::decode(&vec![0; 4097]).is_err());
        assert!(
            NetworkMembership::issue(&authority, "short", NOW, Duration::from_secs(1)).is_err()
        );
        assert!(
            NetworkMembership::issue(&authority, &relay.user_id(), NOW, Duration::ZERO).is_err()
        );
        assert!(
            NetworkMembership::issue(
                &authority,
                &relay.user_id(),
                NOW,
                Duration::from_secs(91 * 86400)
            )
            .is_err()
        );
    }

    #[test]
    fn public_policy_round_trips_revocations_without_private_authority_material() {
        let authority = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let mut policy = NetworkPolicy::new(authority.public().ed25519_pub);
        policy.revoke(&relay.user_id()).unwrap();
        let encoded = policy.encode().unwrap();
        let restored = NetworkPolicy::decode(&encoded).unwrap();
        assert_eq!(restored.authority(), authority.public().ed25519_pub);
        assert_eq!(
            restored.revoked().collect::<Vec<_>>(),
            vec![relay.user_id().as_str()]
        );
        assert!(NetworkPolicy::decode(&vec![0; 65537]).is_err());
    }
}

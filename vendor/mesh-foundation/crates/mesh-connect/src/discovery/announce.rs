//! Signed, bounded advertisements. Applications own capability meaning and trust.
use crate::capability::{Capability, valid};
use bincode::Options;
use mesh_identity::device::{DeviceIdentity, PublicIdentity};
use serde::{Deserialize, Serialize};
const DOMAIN: &[u8] = b"mesh-foundation-discovery-v1";
const MAGIC: &[u8] = b"MFAN\x01";
const MAX_FRAME: usize = 8192;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Announce {
    pub node_id: String,
    pub ed25519_pub: [u8; 32],
    pub x25519_pub: [u8; 32],
    pub name: String,
    pub tcp_port: u16,
    pub namespace: [u8; 32],
    pub capabilities: Vec<Capability>,
    pub metadata: Vec<u8>,
    pub sig: Vec<u8>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    InvalidAdvertisement,
    FrameLimit,
}
impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidAdvertisement => "invalid discovery advertisement",
            Self::FrameLimit => "discovery frame exceeds budget",
        })
    }
}
impl std::error::Error for DiscoveryError {}
impl Announce {
    pub fn new(
        identity: &DeviceIdentity,
        name: impl Into<String>,
        tcp_port: u16,
    ) -> Result<Self, DiscoveryError> {
        Self::for_protocol(identity, [0; 32], name, tcp_port, vec![], vec![])
    }
    pub fn for_protocol(
        identity: &DeviceIdentity,
        namespace: [u8; 32],
        name: impl Into<String>,
        tcp_port: u16,
        capabilities: Vec<Capability>,
        metadata: Vec<u8>,
    ) -> Result<Self, DiscoveryError> {
        let p = identity.public();
        let mut a = Self {
            node_id: p.node_id(),
            ed25519_pub: p.ed25519_pub,
            x25519_pub: p.x25519_pub,
            name: name.into(),
            tcp_port,
            namespace,
            capabilities,
            metadata,
            sig: vec![],
        };
        if !a.shape() {
            return Err(DiscoveryError::InvalidAdvertisement);
        }
        a.sig = identity.sign(&a.input()).to_vec();
        Ok(a)
    }
    fn shape(&self) -> bool {
        self.name.len() <= 1024
            && self.metadata.len() <= 2048
            && self.node_id.len() <= 128
            && valid(&self.capabilities)
    }
    fn input(&self) -> Vec<u8> {
        let mut a = self.clone();
        a.sig.clear();
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(bincode::serialize(&a).expect("advertisement serialization"));
        bytes
    }
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }
    pub fn verify(&self) -> bool {
        self.sig.len() == 64
            && self.shape()
            && self.node_id == self.public().node_id()
            && self
                .sig
                .as_slice()
                .try_into()
                .is_ok_and(|sig| DeviceIdentity::verify(&self.ed25519_pub, &self.input(), sig))
    }
    pub fn valid_presence(&self) -> bool {
        self.tcp_port != 0 && self.verify()
    }
}
/// Validate every variable field before allocating a serialized frame.
pub fn encode(a: &Announce) -> Result<Vec<u8>, DiscoveryError> {
    if !a.shape() || a.sig.len() != 64 {
        return Err(DiscoveryError::InvalidAdvertisement);
    }
    let size = bincode::serialized_size(a).map_err(|_| DiscoveryError::FrameLimit)?;
    if size > (MAX_FRAME - MAGIC.len()) as u64 {
        return Err(DiscoveryError::FrameLimit);
    }
    let mut out = MAGIC.to_vec();
    out.extend(bincode::serialize(a).map_err(|_| DiscoveryError::FrameLimit)?);
    Ok(out)
}
pub fn decode(bytes: &[u8]) -> Option<Announce> {
    if bytes.len() > MAX_FRAME || !bytes.starts_with(MAGIC) {
        return None;
    }
    let a: Announce = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_FRAME as u64)
        .reject_trailing_bytes()
        .deserialize(&bytes[MAGIC.len()..])
        .ok()?;
    a.verify().then_some(a)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_namespace_and_bounds() {
        let id = DeviceIdentity::generate();
        let a = Announce::new(&id, "forum", 123).unwrap();
        assert_eq!(decode(&encode(&a).unwrap()), Some(a.clone()));
        let mut forged = a;
        forged.namespace = [1; 32];
        assert!(decode(&encode(&forged).unwrap()).is_none());
        assert!(Announce::for_protocol(&id, [0; 32], "x", 1, vec![], vec![0; 2049]).is_err());
        assert!(decode(b"MFAN\x02bad").is_none());
        assert!(Announce::new(&id, "x".repeat(1025), 1).is_err());
        let mut oversized = Announce::new(&id, "forum", 1).unwrap();
        oversized.sig = vec![0; 100_000];
        assert_eq!(
            encode(&oversized),
            Err(DiscoveryError::InvalidAdvertisement)
        );
        oversized.sig = vec![0; 64];
        oversized.metadata = vec![0; 100_000];
        assert_eq!(
            encode(&oversized),
            Err(DiscoveryError::InvalidAdvertisement)
        );
    }
}

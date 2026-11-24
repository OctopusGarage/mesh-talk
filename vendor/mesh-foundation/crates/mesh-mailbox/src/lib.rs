//! Bounded opaque store-and-forward records, source snapshots and durable cursors.
//!
//! Hosts own authentication, encryption, access policy, key distribution and
//! scheduling. A content hash proves integrity, never permission or authorship.
//! The generic wire/disk profile is independent from application event formats.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Application-owned opaque routing scope; it grants no access by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopeId([u8; 32]);
impl ScopeId {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
/// Content identity within an opaque scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordId([u8; 32]);
impl RecordId {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
/// Caller-encoded bytes. Encrypt/authenticate the payload at the host boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: RecordId,
    pub scope: ScopeId,
    pub body: Vec<u8>,
}
impl Record {
    pub fn new(scope: ScopeId, body: Vec<u8>) -> Self {
        Self {
            id: Self::content_id(scope, &body),
            scope,
            body,
        }
    }
    fn content_id(scope: ScopeId, body: &[u8]) -> RecordId {
        let mut hash = Sha256::new();
        hash.update(b"mesh-foundation-mailbox-record-v1\0");
        hash.update(scope.as_bytes());
        hash.update(body);
        RecordId(hash.finalize().into())
    }
    pub fn verify_integrity(&self) -> bool {
        self.body.len() <= rpc::BODY_BUDGET.saturating_sub(128)
            && self.id == Self::content_id(self.scope, &self.body)
    }
}
pub mod capacity;
pub mod client;
pub mod delta;
pub mod rpc;
pub mod service;
pub mod spool;

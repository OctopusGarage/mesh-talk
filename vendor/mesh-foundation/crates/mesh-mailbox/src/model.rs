//! Opaque content, scopes and finite source checkpoints. No transport or lifecycle policy.
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
        self.id == Self::content_id(self.scope, &self.body)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailboxCursor {
    pub store_uuid: [u8; 16],
    pub scope: ScopeId,
    pub epoch: i64,
    pub revision: i64,
    pub after: i64,
    pub window: Option<(i64, i64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MailboxDeltaPage {
    pub cursor: MailboxCursor,
    pub ids: Vec<RecordId>,
    pub complete: bool,
    pub reset: bool,
}

impl MailboxCursor {
    pub fn accepts(&self, current: &Self) -> bool {
        self.store_uuid == current.store_uuid
            && self.scope == current.scope
            && self.epoch == current.epoch
            && self.revision >= 0
            && self.revision <= current.revision
            && self.after >= 0
            && self.after <= current.after
            && self.window.is_none_or(|(upper, revision)| {
                self.after <= upper
                    && upper <= current.after
                    && self.revision <= revision
                    && revision <= current.revision
            })
    }
}

impl MailboxDeltaPage {
    /// Validate a bounded page shape; this establishes neither authorization nor
    /// continuity with a previous checkpoint, which the client checks separately.
    pub fn is_well_formed(&self, scope: ScopeId, limit: usize) -> bool {
        let c = &self.cursor;
        c.scope == scope
            && c.epoch >= 0
            && c.after >= 0
            && c.revision >= 0
            && self.ids.len() <= limit
            && self.complete == c.window.is_none()
            && !c
                .window
                .is_some_and(|(upper, revision)| upper <= c.after || revision < c.revision)
            && self
                .ids
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.ids.len()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BatchError {
    Records,
    Bytes,
}
pub(crate) fn validate_batch(
    scope: ScopeId,
    ids: &[RecordId],
    records: &[Record],
    max_bytes: usize,
) -> Result<usize, BatchError> {
    if records.len() != ids.len() {
        return Err(BatchError::Records);
    }
    let bytes = records
        .iter()
        .try_fold(0usize, |sum, r| sum.checked_add(r.body.len()))
        .ok_or(BatchError::Bytes)?;
    if bytes > max_bytes {
        return Err(BatchError::Bytes);
    }
    if !records
        .iter()
        .zip(ids)
        .all(|(r, id)| r.scope == scope && r.id == *id && r.verify_integrity())
    {
        return Err(BatchError::Records);
    }
    Ok(bytes)
}

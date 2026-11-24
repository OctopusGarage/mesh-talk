//! Replaceable authorized storage and asynchronous transport seams.
use crate::model::{MailboxCursor, MailboxDeltaPage, Record, RecordId, ScopeId};
use std::future::Future;
/// Implementations recheck scope access, retention and source identity in the same
/// read transaction. `max_bytes` bounds aggregate raw payload bytes, not framing.
pub trait MailboxStore {
    type Error;
    /// Convenience for a trusted inventory reader without a payload allowance.
    fn delta(
        &self,
        scope: &ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        self.delta_bounded(scope, prior, limit, usize::MAX)
    }
    /// Select byte-aligned IDs using metadata before loading their bodies. A
    /// non-fitting first record is an explicit error, never an empty completion.
    fn delta_bounded(
        &self,
        scope: &ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> Result<MailboxDeltaPage, Self::Error>;
    fn bodies(
        &self,
        scope: &ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> Result<Vec<Record>, Self::Error>;
}
/// One operation per method. Implementations bound I/O/decoding before allocation;
/// the client independently validates results before its atomic commit callback.
/// Requested IDs must be returned in order. Authentication and cancellation-safe
/// exchange retirement belong to the carrier owner. No timer is created here.
pub trait MailboxTransport {
    type Error;
    fn delta(
        &mut self,
        scope: ScopeId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        max_bytes: usize,
    ) -> impl Future<Output = Result<MailboxDeltaPage, Self::Error>> + Send;
    fn bodies(
        &mut self,
        scope: ScopeId,
        cursor: &MailboxCursor,
        ids: &[RecordId],
        max_bytes: usize,
    ) -> impl Future<Output = Result<Vec<Record>, Self::Error>> + Send;
}

//! Bounded recent-history and older-page retrieval for an authorized source.
//!
//! A page covers a finite source-local ordering, not global event time or
//! whole-scope equality. Controls, causal dependencies and key recovery remain
//! separate host obligations. Source-provided timestamps never define coverage.
use crate::pull::Record;
use std::collections::BTreeSet;

/// Identity of a finite authorized source snapshot. An opaque token grants no
/// authority: hosts must bind it to the authenticated peer and current policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryBinding {
    pub scope: [u8; 32],
    pub source: [u8; 32],
    pub authority: [u8; 32],
    pub snapshot: [u8; 32],
    /// Exclusive source-local position, fixed across all older pages.
    pub upper: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryCursor {
    pub binding: HistoryBinding,
    /// Exclusive descending keyset boundary. Never an OFFSET into a new set.
    pub before: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryLimits {
    pub records: usize,
    pub bytes: usize,
    pub examined: usize,
}
impl Default for HistoryLimits {
    fn default() -> Self {
        Self {
            records: 200,
            bytes: 1024 * 1024,
            examined: 2000,
        }
    }
}
impl HistoryLimits {
    fn valid(self) -> bool {
        self.records > 0 && self.bytes > 0 && self.examined >= self.records
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryRequest {
    pub binding: HistoryBinding,
    pub before: u64,
    pub limits: HistoryLimits,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRecord {
    pub position: u64,
    pub record: Record,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage {
    pub binding: HistoryBinding,
    pub records: Vec<HistoryRecord>,
    /// None means the finite source snapshot was exhausted, not that every
    /// replica's authorized history has converged.
    pub next: Option<HistoryCursor>,
    pub examined: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HistoryProgress {
    pub committed: usize,
    pub bytes: usize,
    pub next: Option<HistoryCursor>,
    pub exhausted: bool,
    pub examined: usize,
}
#[derive(Debug, PartialEq, Eq)]
pub enum HistoryError<Fetch, Commit> {
    InvalidLimits,
    InvalidCursor,
    InvalidPage,
    Unauthorized,
    Fetch(Fetch),
    Commit(Commit),
}
/// Fetch exactly one bounded page. No background task or automatic historical
/// drain is created. A caller explicitly selects `next` to request older history.
///
/// `fetch` must enforce transport limits before allocation; this validator checks
/// the returned semantic page. It must also report policy/snapshot invalidation
/// explicitly rather than returning a fabricated empty page. `commit` verifies
/// record authenticity and atomically persists bodies AND `page.next`. It is
/// synchronous, so cancellation cannot fall between commit and acknowledgement.
/// A commit error must leave both unchanged. Duplicate records across retries
/// must be idempotent at the durable store boundary.
pub async fn fetch_page<F, C>(
    binding: HistoryBinding,
    cursor: Option<HistoryCursor>,
    limits: HistoryLimits,
    mut authorize: impl FnMut() -> bool,
    mut fetch: impl AsyncFnMut(HistoryRequest) -> Result<HistoryPage, F>,
    mut commit: impl FnMut(&HistoryPage) -> Result<(), C>,
) -> Result<HistoryProgress, HistoryError<F, C>> {
    if !limits.valid() {
        return Err(HistoryError::InvalidLimits);
    }
    let before = match cursor {
        Some(c) if c.binding != binding || c.before > binding.upper => {
            return Err(HistoryError::InvalidCursor);
        }
        Some(c) => c.before,
        None => binding.upper,
    };
    if !authorize() {
        return Err(HistoryError::Unauthorized);
    }
    let request = HistoryRequest {
        binding,
        before,
        limits,
    };
    let page = fetch(request).await.map_err(HistoryError::Fetch)?;
    if !authorize() {
        return Err(HistoryError::Unauthorized);
    }
    let bytes = validate_page(request, &page).ok_or(HistoryError::InvalidPage)?;
    commit(&page).map_err(HistoryError::Commit)?;
    Ok(HistoryProgress {
        committed: page.records.len(),
        bytes,
        next: page.next,
        exhausted: page.next.is_none(),
        examined: page.examined,
    })
}
fn validate_page(request: HistoryRequest, page: &HistoryPage) -> Option<usize> {
    if page.binding != request.binding
        || page.records.len() > request.limits.records
        || page.examined > request.limits.examined
        || page.examined < page.records.len()
    {
        return None;
    }
    let mut upper = request.before;
    let mut ids = BTreeSet::new();
    let mut bytes = 0usize;
    for entry in &page.records {
        if entry.position >= upper || !ids.insert(entry.record.id) {
            return None;
        }
        upper = entry.position;
        bytes = bytes.checked_add(entry.record.body.len())?;
        if bytes > request.limits.bytes {
            return None;
        }
    }
    if let Some(next) = page.next
        && (next.binding != request.binding
            || next.before >= request.before
            || next.before > upper
            || page.examined == 0)
    {
        return None;
    }
    Some(bytes)
}

impl<F: std::fmt::Display, C: std::fmt::Display> std::fmt::Display for HistoryError<F, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidLimits => f.write_str("invalid history page limits"),
            Self::InvalidCursor => {
                f.write_str("history cursor does not belong to this source snapshot")
            }
            Self::InvalidPage => f.write_str("invalid or over-budget history page"),
            Self::Unauthorized => f.write_str("history snapshot authorization revoked"),
            Self::Fetch(error) => write!(f, "history page fetch failed: {error}"),
            Self::Commit(error) => write!(f, "history body/cursor commit failed: {error}"),
        }
    }
}
impl<F: std::error::Error + 'static, C: std::error::Error + 'static> std::error::Error
    for HistoryError<F, C>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Fetch(error) => Some(error),
            Self::Commit(error) => Some(error),
            _ => None,
        }
    }
}

// MIT. Mesh Foundation range primitive from commit
// 4bf8f92269d81b8f20e0f807e04d7a7cf6fe2588. Application adaptations use an
// imperative query checkpoint and explicit blocked-frontier scheduling.
//! Incrementally maintained binary-prefix summaries for authorized ID sets.
//! Hosts bind an index to one scope and authorization generation; rebuilding an
//! index is explicit work, never performed by a summary query.
type EventId = [u8; 32];
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub use mesh_replication::range::{Prefix, RemoteError, Summary, SummaryFrame};

pub(super) fn empty() -> Summary {
    Summary {
        count: 0,
        digest: Sha256::digest(b"mesh-range-empty-v1").into(),
    }
}
pub(super) fn combine(bits: u16, a: Summary, b: Summary) -> Summary {
    let mut h = Sha256::new();
    h.update(b"mesh-range-node-v1");
    h.update(bits.to_le_bytes());
    h.update(a.digest);
    h.update(b.digest);
    Summary {
        count: a.count + b.count,
        digest: h.finalize().into(),
    }
}
#[derive(Default)]
pub struct RangeIndex {
    ids: BTreeSet<EventId>,
    nodes: BTreeMap<Prefix, Summary>,
    revision: u64,
}
impl RangeIndex {
    /// Number of cached branching and leaf summaries. For N IDs this is at most
    /// 2*N-1; unary paths are reconstructed with at most 256 fixed-depth hashes.
    pub fn stored_summaries(&self) -> usize {
        self.nodes.len()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn summary(&self, p: Prefix) -> Summary {
        if let Some(summary) = self.nodes.get(&p) {
            return *summary;
        }
        let mut upper = p.key();
        for bit in usize::from(p.bits())..256 {
            upper[bit / 8] |= 128 >> (bit % 8);
        }
        let mut range = self.ids.range(p.key()..=upper);
        let Some(first) = range.next() else {
            return empty();
        };
        let last = range.next_back().unwrap_or(first);
        let mut depth = 256u16;
        if first != last {
            depth = (0..256)
                .find(|bit| (first[bit / 8] ^ last[bit / 8]) & (128 >> (bit % 8)) != 0)
                .unwrap() as u16;
        }
        let base = Prefix::new(depth, *first).unwrap();
        let mut summary = *self.nodes.get(&base).expect("branch/leaf summary exists");
        for bits in (p.bits()..depth).rev() {
            let right = first[usize::from(bits) / 8] & (128 >> (bits % 8)) != 0;
            summary = if right {
                combine(bits, empty(), summary)
            } else {
                combine(bits, summary, empty())
            };
        }
        summary
    }
    pub fn insert(&mut self, id: EventId) -> bool {
        if !self.ids.insert(id) {
            return false;
        }
        self.update(id);
        true
    }
    pub fn remove(&mut self, id: &EventId) -> bool {
        if !self.ids.remove(id) {
            return false;
        }
        self.update(*id);
        true
    }
    fn update(&mut self, id: EventId) {
        let leaf = Prefix::new(256, id).unwrap();
        if self.ids.contains(&id) {
            self.nodes.insert(
                leaf,
                Summary {
                    count: 1,
                    digest: Sha256::digest([b"mesh-range-id-v1".as_slice(), &id].concat()).into(),
                },
            );
        } else {
            self.nodes.remove(&leaf);
        }
        let mut carry = self.nodes.get(&leaf).copied().unwrap_or_else(empty);
        for bits in (0..256).rev() {
            let p = Prefix::new(bits, id).unwrap();
            let children = p.children().unwrap();
            let right = id[usize::from(bits) / 8] & (128 >> (bits % 8)) != 0;
            let (a, b) = if right {
                (self.summary(children[0]), carry)
            } else {
                (carry, self.summary(children[1]))
            };
            carry = if a.count + b.count == 0 {
                empty()
            } else {
                combine(bits, a, b)
            };
            if a.count == 0 || b.count == 0 {
                self.nodes.remove(&p);
            } else {
                self.nodes.insert(p, carry);
            }
        }
        self.revision = self
            .revision
            .checked_add(1)
            .expect("range index revision exhausted");
    }
}
#[derive(Clone, Debug)]
pub struct Checkpoint {
    scope: [u8; 32],
    authority: [u8; 32],
    local: u64,
    local_root: Summary,
    remote_root: Option<Summary>,
    remote: u64,
    pending: Vec<Prefix>,
    remote_view: Option<[u8; 32]>,
    repair_after: Option<i64>,
    repair_anchors: Vec<EventId>,
    repair_warm: bool,
    overlap_known: u64,
    overlap_missing: u64,
    blocked: BTreeSet<EventId>,
    saturated: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Complete,
    More,
    Invalidated,
}
#[derive(Debug)]
pub struct Difference {
    pub progress: Progress,
    pub missing: Vec<EventId>,
    pub examined: usize,
}
impl Checkpoint {
    pub fn new(
        scope: [u8; 32],
        authority: [u8; 32],
        local: &RangeIndex,
        remote: &RangeIndex,
    ) -> Self {
        Self {
            scope,
            authority,
            local: local.revision,
            local_root: local.summary(Prefix::root()),
            remote_root: Some(remote.summary(Prefix::root())),
            remote: remote.revision,
            pending: vec![Prefix::root()],
            remote_view: None,
            repair_after: None,
            repair_anchors: Vec::new(),
            repair_warm: false,
            overlap_known: 0,
            overlap_missing: 0,
            blocked: BTreeSet::new(),
            saturated: false,
        }
    }
    /// Compare at most `nodes` cached summaries. Each emitted ID is present in
    /// remote but absent locally. Checkpoint is discovery progress, not a durable
    /// receipt: hosts must commit discovered work before persisting it.
    pub fn advance(
        &mut self,
        scope: [u8; 32],
        authority: [u8; 32],
        local: &RangeIndex,
        remote: &RangeIndex,
        nodes: usize,
    ) -> Difference {
        if self.scope != scope
            || self.authority != authority
            || self.local_root != local.summary(Prefix::root())
            || self.local != local.revision
            || self.remote_root != Some(remote.summary(Prefix::root()))
            || self.remote != remote.revision
        {
            return Difference {
                progress: Progress::Invalidated,
                missing: vec![],
                examined: 0,
            };
        }
        let mut missing = Vec::new();
        let mut examined = 0;
        while examined < nodes {
            let Some(p) = self.pending.pop() else { break };
            examined += 1;
            let a = local.summary(p);
            let b = remote.summary(p);
            if a == b || b.count == 0 {
                continue;
            }
            if let Some([left, right]) = p.children() {
                self.push_children(left, right);
            } else if a.count == 0 {
                missing.push(p.key());
            }
        }
        Difference {
            progress: if self.pending.is_empty() && !self.saturated {
                Progress::Complete
            } else {
                Progress::More
            },
            missing,
            examined,
        }
    }
}

/// A separate versioned profile; legacy wire discriminants are untouched.
impl Checkpoint {
    /// Query only divergent remote prefixes; no remote ID collection is retained.
    /// `view` is the pinned remote snapshot identity; `authorize` requalifies the
    /// host's local scope policy before and after every suspended exchange.
    /// Each queried prefix spends one RPC. An error leaves that prefix pending.
    #[allow(clippy::too_many_arguments)] // Explicit snapshot/policy binding and bounded I/O callbacks.
    pub async fn advance_remote<E>(
        &mut self,
        local: &RangeIndex,
        view: [u8; 32],
        authority: [u8; 32],
        remote_revision: u64,
        nodes: usize,
        authorize: impl FnMut() -> bool,
        query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame, E>,
    ) -> Result<Difference, RemoteError<E>> {
        let mut work = self.clone();
        let result = work
            .advance_remote_inner(
                local,
                view,
                authority,
                remote_revision,
                nodes,
                authorize,
                query,
            )
            .await;
        if result.is_ok() {
            *self = work;
        }
        result
    }
    #[allow(clippy::too_many_arguments)]
    async fn advance_remote_inner<E>(
        &mut self,
        local: &RangeIndex,
        view: [u8; 32],
        authority: [u8; 32],
        remote_revision: u64,
        nodes: usize,
        mut authorize: impl FnMut() -> bool,
        mut query: impl AsyncFnMut(Prefix) -> Result<SummaryFrame, E>,
    ) -> Result<Difference, RemoteError<E>> {
        if self.local_root != local.summary(Prefix::root())
            || self.local != local.revision
            || self.remote != remote_revision
            || self.authority != authority
            || self.remote_view.is_some_and(|v| v != view)
            || !authorize()
        {
            return Err(RemoteError::Invalidated);
        }
        self.remote_view = Some(view);
        let mut missing = Vec::new();
        let mut examined = 0;
        while examined < nodes {
            let Some(&prefix) = self.pending.last() else {
                break;
            };
            if !authorize() {
                return Err(RemoteError::Invalidated);
            }
            let frame = query(prefix).await.map_err(RemoteError::Exchange)?;
            if !authorize() {
                return Err(RemoteError::Invalidated);
            }
            if frame.scope != self.scope || frame.view != view || frame.prefix != prefix {
                return Err(RemoteError::InvalidFrame);
            }
            if let Some(id) = frame.singleton
                && singleton_summary(prefix, id) != Some(frame.summary)
            {
                return Err(RemoteError::InvalidFrame);
            }
            if !frame.valid_leaves() {
                return Err(RemoteError::InvalidFrame);
            }
            self.pending.pop();
            examined += 1;
            let a = local.summary(prefix);
            let b = frame.summary;
            if a == b || b.count == 0 {
                continue;
            }
            if !frame.leaves.is_empty() {
                missing.extend(
                    frame
                        .leaves
                        .iter()
                        .filter(|id| !local.ids.contains(*id))
                        .copied(),
                );
                continue;
            }
            if let Some(id) = frame.singleton {
                if b.count != 1 || singleton_summary(prefix, id) != Some(b) {
                    return Err(RemoteError::InvalidFrame);
                }
                if !local.ids.contains(&id) {
                    missing.push(id);
                }
                continue;
            }
            if let Some([left, right]) = prefix.children() {
                self.push_children(left, right);
            } else if b.count != 1 {
                return Err(RemoteError::InvalidFrame);
            } else if a.count == 0 {
                missing.push(prefix.key());
            }
        }
        Ok(Difference {
            progress: if self.pending.is_empty() && !self.saturated {
                Progress::Complete
            } else {
                Progress::More
            },
            missing,
            examined,
        })
    }
}

impl Checkpoint {
    pub fn for_remote(
        scope: [u8; 32],
        authority: [u8; 32],
        local: &RangeIndex,
        remote_revision: u64,
    ) -> Self {
        Self {
            scope,
            authority,
            local: local.revision,
            local_root: local.summary(Prefix::root()),
            remote_root: None,
            remote: remote_revision,
            pending: vec![Prefix::root()],
            remote_view: None,
            repair_after: None,
            repair_anchors: Vec::new(),
            repair_warm: false,
            overlap_known: 0,
            overlap_missing: 0,
            blocked: BTreeSet::new(),
            saturated: false,
        }
    }
    /// Only call after these exact IDs have committed durably. External mutations
    /// invalidate the checkpoint; this never rebinds arbitrary changed indexes.
    pub fn acknowledge_insertions(&mut self, local: &mut RangeIndex, ids: &[EventId]) -> bool {
        if self.local_root != local.summary(Prefix::root()) || self.local != local.revision {
            return false;
        }
        for id in ids {
            local.insert(*id);
        }
        self.local = local.revision;
        self.local_root = local.summary(Prefix::root());
        true
    }
}

fn singleton_summary(prefix: Prefix, id: EventId) -> Option<Summary> {
    if Prefix::new(prefix.bits(), id)? != prefix {
        return None;
    }
    let mut summary = Summary {
        count: 1,
        digest: Sha256::digest([b"mesh-range-id-v1".as_slice(), &id].concat()).into(),
    };
    for bits in (prefix.bits()..256).rev() {
        summary = if id[usize::from(bits) / 8] & (128 >> (bits % 8)) != 0 {
            combine(bits, empty(), summary)
        } else {
            combine(bits, summary, empty())
        };
    }
    Some(summary)
}
impl RangeIndex {
    pub fn frame(&self, scope: [u8; 32], view: [u8; 32], prefix: Prefix) -> SummaryFrame {
        let summary = self.summary(prefix);
        let singleton = if summary.count == 1 {
            self.ids.range(prefix.key()..).next().copied()
        } else {
            None
        };
        SummaryFrame {
            scope,
            view,
            prefix,
            summary,
            singleton,
            leaves: if (2..=32).contains(&summary.count) {
                self.ids
                    .range(prefix.key()..)
                    .take(summary.count)
                    .copied()
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}
impl Checkpoint {
    pub fn next_query(&self) -> Option<Prefix> {
        (!self.saturated)
            .then(|| self.pending.last().copied())
            .flatten()
    }
    pub fn has_pending(&self) -> bool {
        self.repair_after.is_some() || self.saturated || !self.pending.is_empty()
    }
    pub fn defer_missing(&mut self, ids: &[[u8; 32]]) {
        for id in ids {
            if self.saturated {
                break;
            }
            let prefix = Prefix::new(256, *id).unwrap();
            if !self.pending.contains(&prefix) {
                if self.pending.len() >= 4096 {
                    self.saturated = true;
                    break;
                }
                self.pending.insert(0, prefix);
            }
        }
    }
    pub fn accept_query(
        &mut self,
        local: &RangeIndex,
        scope: [u8; 32],
        view: [u8; 32],
        authority: [u8; 32],
        revision: u64,
        frame: SummaryFrame,
    ) -> Result<Vec<EventId>, RemoteError<()>> {
        if self.local_root != local.summary(Prefix::root())
            || self.local != local.revision
            || self.scope != scope
            || self.authority != authority
            || self.remote != revision
            || self.remote_view.is_some_and(|v| v != view)
        {
            return Err(RemoteError::Invalidated);
        }
        let prefix = self.next_query().ok_or(RemoteError::InvalidFrame)?;
        if frame.scope != scope
            || frame.view != view
            || frame.prefix != prefix
            || !frame.valid_leaves()
        {
            return Err(RemoteError::InvalidFrame);
        }
        self.remote_view = Some(view);
        self.pending.pop();
        if local.summary(prefix) == frame.summary || frame.summary.count == 0 {
            return Ok(vec![]);
        }
        if !frame.leaves.is_empty() {
            return Ok(frame
                .leaves
                .into_iter()
                .filter(|id| !local.ids.contains(id))
                .collect());
        }
        if let Some(id) = frame.singleton {
            return Ok(if local.ids.contains(&id) {
                vec![]
            } else {
                vec![id]
            });
        }
        if let Some([left, right]) = prefix.children() {
            if self.pending.len() > 4094 {
                self.saturated = true;
            } else {
                self.push_children(left, right);
            }
            Ok(vec![])
        } else if frame.summary.count == 1 {
            Ok(vec![prefix.key()])
        } else {
            Err(RemoteError::InvalidFrame)
        }
    }
}

impl Checkpoint {
    pub fn mark_unavailable(&mut self, ids: &[EventId]) {
        for id in ids {
            if self.blocked.contains(id) {
                continue;
            }
            if self.blocked.len() >= 4096 {
                self.saturated = true;
                break;
            }
            self.blocked.insert(*id);
        }
    }
    pub fn has_unblocked_pending(&self) -> bool {
        self.repair_after.is_some()
            || !self.saturated
                && self
                    .pending
                    .iter()
                    .any(|prefix| prefix.bits() != 256 || !self.blocked.contains(&prefix.key()))
    }
}

impl Checkpoint {
    pub fn valid_remote_binding(
        &self,
        local: &RangeIndex,
        scope: [u8; 32],
        view: [u8; 32],
        authority: [u8; 32],
        revision: u64,
    ) -> bool {
        self.local_root == local.summary(Prefix::root())
            && self.local == local.revision
            && self.scope == scope
            && self.authority == authority
            && self.remote == revision
            && self.remote_view.is_none_or(|pinned| pinned == view)
    }
}

/// Fallible reads for a disk-backed authorized snapshot. Storage failure is
/// separate from protocol invalidation and must never be treated as equality.
pub trait RangeRead {
    type Error;
    fn revision(&self) -> u64;
    fn summary(&self, prefix: Prefix) -> Result<Summary, Self::Error>;
    fn contains(&self, id: &EventId) -> Result<bool, Self::Error>;
}
impl RangeRead for RangeIndex {
    type Error = std::convert::Infallible;
    fn revision(&self) -> u64 {
        self.revision
    }
    fn summary(&self, prefix: Prefix) -> Result<Summary, Self::Error> {
        Ok(self.summary(prefix))
    }
    fn contains(&self, id: &EventId) -> Result<bool, Self::Error> {
        Ok(self.ids.contains(id))
    }
}
impl Checkpoint {
    pub fn for_reader<R: RangeRead + ?Sized>(
        scope: [u8; 32],
        authority: [u8; 32],
        local: &R,
        remote_revision: u64,
    ) -> Result<Self, R::Error> {
        Ok(Self {
            scope,
            authority,
            local: local.revision(),
            local_root: local.summary(Prefix::root())?,
            remote_root: None,
            remote: remote_revision,
            pending: vec![Prefix::root()],
            remote_view: None,
            repair_after: None,
            repair_anchors: Vec::new(),
            repair_warm: false,
            overlap_known: 0,
            overlap_missing: 0,
            blocked: BTreeSet::new(),
            saturated: false,
        })
    }
    pub fn valid_reader_binding<R: RangeRead + ?Sized>(
        &self,
        local: &R,
        scope: [u8; 32],
        view: [u8; 32],
        authority: [u8; 32],
        revision: u64,
    ) -> Result<bool, R::Error> {
        Ok(self.local_root == local.summary(Prefix::root())?
            && self.local == local.revision()
            && self.scope == scope
            && self.authority == authority
            && self.remote == revision
            && self.remote_view.is_none_or(|v| v == view))
    }
    /// Reads all needed local facts before mutating discovery state. A failed
    /// SQL read therefore retains the exact prefix for retry.
    pub fn accept_reader<R: RangeRead + ?Sized>(
        &mut self,
        local: &R,
        scope: [u8; 32],
        view: [u8; 32],
        authority: [u8; 32],
        revision: u64,
        frame: SummaryFrame,
    ) -> Result<Result<Vec<EventId>, RemoteError<()>>, R::Error> {
        if !self.valid_reader_binding(local, scope, view, authority, revision)? {
            return Ok(Err(RemoteError::Invalidated));
        }
        let Some(prefix) = self.next_query() else {
            return Ok(Err(RemoteError::InvalidFrame));
        };
        if frame.scope != scope
            || frame.view != view
            || frame.prefix != prefix
            || !frame.valid_leaves()
        {
            return Ok(Err(RemoteError::InvalidFrame));
        }
        let equal = local.summary(prefix)? == frame.summary || frame.summary.count == 0;
        if equal && frame.summary.count > 0 {
            self.overlap_known = self
                .overlap_known
                .saturating_add(frame.summary.count as u64);
        }
        let mut missing = Vec::new();
        if !equal {
            for id in frame.leaves.iter().copied().chain(frame.singleton) {
                if !local.contains(&id)? {
                    self.overlap_missing = self.overlap_missing.saturating_add(1);
                    missing.push(id);
                } else {
                    self.overlap_known = self.overlap_known.saturating_add(1);
                }
            }
            if frame.leaves.is_empty()
                && frame.singleton.is_none()
                && prefix.bits() == 256
                && frame.summary.count != 1
            {
                return Ok(Err(RemoteError::InvalidFrame));
            }
        }
        if prefix.bits() == 0 {
            self.remote_root = Some(frame.summary);
        }
        self.remote_view = Some(view);
        self.pending.pop();
        if !equal && frame.leaves.is_empty() && frame.singleton.is_none() {
            if let Some([left, right]) = prefix.children() {
                self.push_children(left, right);
            } else {
                missing.push(prefix.key());
            }
        }
        Ok(Ok(missing))
    }
}
#[cfg(test)]
mod reader_tests {
    use super::*;
    struct Reader {
        index: RangeIndex,
        fail: std::cell::Cell<bool>,
    }
    impl RangeRead for Reader {
        type Error = &'static str;
        fn revision(&self) -> u64 {
            self.index.revision()
        }
        fn summary(&self, p: Prefix) -> Result<Summary, Self::Error> {
            if self.fail.get() {
                Err("storage unavailable")
            } else {
                Ok(self.index.summary(p))
            }
        }
        fn contains(&self, id: &EventId) -> Result<bool, Self::Error> {
            if self.fail.get() {
                Err("storage unavailable")
            } else {
                Ok(self.index.ids.contains(id))
            }
        }
    }
    #[test]
    fn failed_reader_does_not_consume_frontier_or_claim_equality() {
        let local = Reader {
            index: RangeIndex::default(),
            fail: std::cell::Cell::new(false),
        };
        let mut remote = RangeIndex::default();
        remote.insert([3; 32]);
        let mut checkpoint =
            Checkpoint::for_reader([1; 32], [2; 32], &local, remote.revision()).unwrap();
        let frame = remote.frame([1; 32], [4; 32], Prefix::root());
        local.fail.set(true);
        assert!(
            checkpoint
                .accept_reader(
                    &local,
                    [1; 32],
                    [4; 32],
                    [2; 32],
                    remote.revision(),
                    frame.clone()
                )
                .is_err()
        );
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        local.fail.set(false);
        assert_eq!(
            checkpoint
                .accept_reader(&local, [1; 32], [4; 32], [2; 32], remote.revision(), frame)
                .unwrap()
                .unwrap(),
            vec![[3; 32]]
        );
    }
}

impl Checkpoint {
    pub(crate) fn heap_bytes(&self) -> usize {
        self.pending
            .capacity()
            .saturating_mul(std::mem::size_of::<Prefix>())
            .saturating_add(self.blocked.len().saturating_mul(128))
            .saturating_add(
                std::mem::size_of::<Self>() + self.repair_anchors.capacity().saturating_mul(32),
            )
    }
}

impl Checkpoint {
    fn push_children(&mut self, left: Prefix, right: Prefix) {
        if self.pending.len() > 4094 {
            self.saturated = true;
        } else {
            self.pending.push(right);
            self.pending.push(left);
        }
    }
}
#[cfg(test)]
mod bounded_frontier_tests {
    use super::*;
    #[test]
    fn saturated_blocked_frontier_never_claims_completion_or_exceeds_bounds() {
        let local = RangeIndex::default();
        let mut cp = Checkpoint::for_remote([1; 32], [2; 32], &local, 0);
        let ids: Vec<_> = (0..4097u64)
            .map(|n| {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&n.to_be_bytes());
                id
            })
            .collect();
        cp.mark_unavailable(&ids);
        cp.defer_missing(&ids);
        assert_eq!(cp.blocked.len(), 4096);
        assert!(cp.pending.len() <= 4096);
        assert!(cp.has_pending());
        assert!(!cp.has_unblocked_pending());
        assert!(cp.next_query().is_none());
        cp.pending.clear();
        assert!(
            cp.has_pending(),
            "saturation must not become an empty/equal proof"
        );
        assert!(cp.heap_bytes() >= 4096 * 128);
    }
    #[test]
    fn pending_frontier_itself_saturates_without_unbounded_navigation_state() {
        let local = RangeIndex::default();
        let mut cp = Checkpoint::for_remote([1; 32], [2; 32], &local, 0);
        let ids: Vec<_> = (0..5000u64)
            .map(|n| {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&n.to_be_bytes());
                id
            })
            .collect();
        cp.defer_missing(&ids);
        assert_eq!(cp.pending.len(), 4096);
        assert!(cp.has_pending());
        assert!(!cp.has_unblocked_pending());
    }
}

impl Checkpoint {
    pub fn ordered_anchors(&self) -> &[EventId] {
        &self.repair_anchors
    }
    pub fn ordered_is_warm(&self) -> bool {
        self.repair_warm
    }
    pub fn begin_ordered_repair_with_anchors(&mut self, anchors: &[EventId]) {
        self.begin_ordered_repair();
        self.repair_anchors = anchors.iter().copied().take(32).collect();
        self.repair_warm =
            self.overlap_known >= 8 && self.overlap_known >= self.overlap_missing.saturating_mul(2);
    }
    pub fn begin_ordered_repair(&mut self) {
        self.pending.clear();
        self.blocked.clear();
        self.saturated = false;
        self.repair_after = Some(-1);
    }
    pub fn ordered_cursor(&self) -> Option<i64> {
        self.repair_after
    }
    pub fn acknowledge_ordered_page(&mut self, next: i64, complete: bool) {
        self.repair_anchors.clear();
        self.repair_after = if complete { None } else { Some(next) };
        if complete {
            // Rank traversal is repair, not proof of the advertised Merkle root.
            self.blocked.clear();
            self.saturated = false;
            self.pending = vec![Prefix::root()];
        }
    }
}

#[cfg(test)]
mod ordered_repair_tests {
    use super::*;
    #[test]
    fn ordered_progress_requires_acknowledgment_and_clears_blocked_frontier() {
        let local = RangeIndex::default();
        let mut cp = Checkpoint::for_remote([1; 32], [2; 32], &local, 0);
        cp.overlap_known = 31;
        cp.overlap_missing = 1;
        cp.begin_ordered_repair_with_anchors(&[[4; 32]; 40]);
        assert_eq!(cp.ordered_anchors().len(), 32);
        assert!(cp.ordered_is_warm());
        cp.mark_unavailable(&[[3; 32]]);
        cp.begin_ordered_repair();
        assert_eq!(cp.ordered_cursor(), Some(-1));
        assert!(cp.has_pending() && cp.has_unblocked_pending());
        let interrupted = cp.clone();
        cp.acknowledge_ordered_page(99, false);
        assert_eq!(interrupted.ordered_cursor(), Some(-1));
        assert_eq!(cp.ordered_cursor(), Some(99));
        cp.acknowledge_ordered_page(200, true);
        assert_eq!(cp.ordered_cursor(), None);
        assert!(cp.has_pending());
    }
}

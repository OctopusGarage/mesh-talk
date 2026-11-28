//! Application-owned causal-repair checkpoint over SDK canonical range primitives.
//! SQL selection lifetime, blocked ancestors and recovery scheduling stay here.
type EventId = [u8; 32];
pub use mesh_replication::range::{
    Prefix, RangeIndex, RangeRead, ReaderBinding, RemoteError, Summary, SummaryFrame,
    singleton_summary,
};
use std::collections::BTreeSet;

pub(super) fn empty() -> Summary {
    Summary::empty()
}
pub(super) fn combine(bits: u16, left: Summary, right: Summary) -> Summary {
    Summary::combine_at_depth(bits, left, right).expect("valid non-leaf summary with bounded count")
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
            local: local.revision(),
            local_root: local.summary(Prefix::root()),
            remote_root: Some(remote.summary(Prefix::root())),
            remote: remote.revision(),
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
            || self.local != local.revision()
            || self.remote_root != Some(remote.summary(Prefix::root()))
            || self.remote != remote.revision()
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
        if !self.valid_remote_binding(local, self.scope, view, authority, remote_revision)
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
            frame
                .validate_for(self.scope, view, Some(prefix))
                .map_err(|_| RemoteError::InvalidFrame)?;
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
                        .filter(|id| !local.contains(id))
                        .copied(),
                );
                continue;
            }
            if let Some(id) = frame.singleton {
                if b.count != 1 || singleton_summary(prefix, id) != Some(b) {
                    return Err(RemoteError::InvalidFrame);
                }
                if !local.contains(&id) {
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
            local: local.revision(),
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
        if self.local_root != local.summary(Prefix::root()) || self.local != local.revision() {
            return false;
        }
        for id in ids {
            local.insert(*id);
        }
        self.local = local.revision();
        self.local_root = local.summary(Prefix::root());
        true
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
        if !self.valid_remote_binding(local, scope, view, authority, revision) {
            return Err(RemoteError::Invalidated);
        }
        let prefix = frame.validate_for(scope, view, self.next_query())?;

        self.remote_view = Some(view);
        self.pending.pop();
        if local.summary(prefix) == frame.summary || frame.summary.count == 0 {
            return Ok(vec![]);
        }
        if !frame.leaves.is_empty() {
            return Ok(frame
                .leaves
                .into_iter()
                .filter(|id| !local.contains(id))
                .collect());
        }
        if let Some(id) = frame.singleton {
            return Ok(if local.contains(&id) {
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
    fn reader_binding(&self) -> ReaderBinding {
        ReaderBinding {
            scope: self.scope,
            authority: self.authority,
            local_revision: self.local,
            local_root: self.local_root,
            remote_revision: self.remote,
            remote_view: self.remote_view,
        }
    }
    pub fn valid_remote_binding(
        &self,
        local: &RangeIndex,
        scope: [u8; 32],
        view: [u8; 32],
        authority: [u8; 32],
        revision: u64,
    ) -> bool {
        self.reader_binding()
            .valid_reader(local, scope, view, authority, revision)
            .expect("in-memory range reads are infallible")
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
        self.reader_binding()
            .valid_reader(local, scope, view, authority, revision)
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
        let prefix = match frame.validate_for(scope, view, self.next_query()) {
            Ok(prefix) => prefix,
            Err(error) => return Ok(Err(error)),
        };
        let equal = local.summary(prefix)? == frame.summary || frame.summary.count == 0;
        let mut overlap_known = self.overlap_known;
        let mut overlap_missing = self.overlap_missing;
        if equal && frame.summary.count > 0 {
            overlap_known = overlap_known.saturating_add(frame.summary.count as u64);
        }
        let mut missing = Vec::new();
        if !equal {
            for id in frame.leaves.iter().copied().chain(frame.singleton) {
                if !local.contains(&id)? {
                    overlap_missing = overlap_missing.saturating_add(1);
                    missing.push(id);
                } else {
                    overlap_known = overlap_known.saturating_add(1);
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
        // Counts decide repair strategy too. Commit them with the frontier,
        // never before a fallible leaf read that may retry this same prefix.
        self.overlap_known = overlap_known;
        self.overlap_missing = overlap_missing;
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
                Ok(self.index.contains(id))
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

#[cfg(test)]
mod boundary_tests {
    use super::*;

    const SCOPE: EventId = [1; 32];
    const AUTHORITY: EventId = [2; 32];
    const VIEW: EventId = [3; 32];

    fn branching_remote() -> RangeIndex {
        let mut remote = RangeIndex::default();
        // More than 32 leaves forces a summary exchange before leaf discovery.
        for n in 0..33u8 {
            let mut id = [0; 32];
            id[0] = n.saturating_mul(7);
            remote.insert(id);
        }
        remote
    }

    #[test]
    fn sparse_index_removal_collapses_branches_without_changing_survivor_summary() {
        let ids = [[0; 32], [1; 32], [128; 32], [255; 32]];
        let mut index = RangeIndex::default();
        for id in ids {
            assert!(index.insert(id));
        }
        for removed in &ids[..3] {
            assert!(index.remove(removed));
            let mut rebuilt = RangeIndex::default();
            for id in ids.into_iter().filter(|id| index.contains(id)) {
                rebuilt.insert(id);
            }
            for prefix in [Prefix::root(), Prefix::new(1, ids[3]).unwrap()] {
                assert_eq!(index.summary(prefix), rebuilt.summary(prefix));
            }
            assert!(index.stored_summaries() < 2 * index.summary(Prefix::root()).count);
        }
        assert_eq!(
            index.frame(SCOPE, VIEW, Prefix::root()).singleton,
            Some(ids[3])
        );
        assert!(index.remove(&ids[3]));
        assert_eq!(index.summary(Prefix::root()), empty());
        assert_eq!(index.stored_summaries(), 0);
        let revision = index.revision();
        assert!(!index.remove(&ids[3]));
        assert_eq!(index.revision(), revision);
    }

    #[test]
    fn local_comparison_resumes_within_budget_and_rejects_same_root_new_revision() {
        let mut local = RangeIndex::default();
        let mut remote = RangeIndex::default();
        remote.insert([255; 32]);
        let mut checkpoint = Checkpoint::new(SCOPE, AUTHORITY, &local, &remote);
        let zero = checkpoint.advance(SCOPE, AUTHORITY, &local, &remote, 0);
        assert_eq!(zero.progress, Progress::More);
        assert_eq!(zero.examined, 0);
        let mut missing = Vec::new();
        loop {
            let page = checkpoint.advance(SCOPE, AUTHORITY, &local, &remote, 7);
            assert!(page.examined <= 7);
            missing.extend(page.missing);
            if page.progress == Progress::Complete {
                break;
            }
        }
        assert_eq!(missing, vec![[255; 32]]);
        assert_eq!(
            checkpoint
                .advance(SCOPE, AUTHORITY, &local, &remote, 7)
                .examined,
            0
        );
        local.insert([9; 32]);
        local.remove(&[9; 32]);
        let stale = checkpoint.advance(SCOPE, AUTHORITY, &local, &remote, 7);
        assert_eq!(stale.progress, Progress::Invalidated);
        assert!(stale.missing.is_empty());
        assert_eq!(stale.examined, 0);
        assert!(!checkpoint.acknowledge_insertions(&mut local, &missing));
        assert_eq!(local.summary(Prefix::root()), empty());
    }

    #[tokio::test]
    async fn remote_exchange_failure_rolls_back_prior_queries_before_retry() {
        let local = RangeIndex::default();
        let remote = branching_remote();
        let mut checkpoint = Checkpoint::for_remote(SCOPE, AUTHORITY, &local, remote.revision());
        let mut calls = 0;
        let error = checkpoint
            .advance_remote(
                &local,
                VIEW,
                AUTHORITY,
                remote.revision(),
                2,
                || true,
                async |prefix| {
                    calls += 1;
                    if calls == 2 {
                        Err("disconnected")
                    } else {
                        Ok(remote.frame(SCOPE, VIEW, prefix))
                    }
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RemoteError::Exchange("disconnected")));
        assert_eq!(calls, 2);
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        // Failed work must not pin the remote view either.
        assert!(checkpoint.valid_remote_binding(
            &local,
            SCOPE,
            [4; 32],
            AUTHORITY,
            remote.revision()
        ));
        let mut missing = Vec::new();
        loop {
            let page = checkpoint
                .advance_remote(
                    &local,
                    VIEW,
                    AUTHORITY,
                    remote.revision(),
                    2,
                    || true,
                    async |prefix| Ok::<_, ()>(remote.frame(SCOPE, VIEW, prefix)),
                )
                .await
                .unwrap();
            assert!(page.examined <= 2);
            missing.extend(page.missing);
            if page.progress == Progress::Complete {
                break;
            }
        }
        assert_eq!(
            missing.into_iter().collect::<BTreeSet<_>>(),
            (0..33u8)
                .map(|n| {
                    let mut id = [0; 32];
                    id[0] = n.saturating_mul(7);
                    id
                })
                .collect::<BTreeSet<_>>()
        );
        assert!(!checkpoint.has_pending());
    }

    #[tokio::test]
    async fn cancelled_remote_exchange_retains_original_frontier_and_snapshot_binding() {
        let local = RangeIndex::default();
        let remote = branching_remote();
        let mut checkpoint = Checkpoint::for_remote(SCOPE, AUTHORITY, &local, remote.revision());
        let calls = std::cell::Cell::new(0);
        {
            let turn = checkpoint.advance_remote(
                &local,
                VIEW,
                AUTHORITY,
                remote.revision(),
                2,
                || true,
                async |prefix| {
                    calls.set(calls.get() + 1);
                    if calls.get() == 2 {
                        std::future::pending::<()>().await;
                    }
                    Ok::<_, ()>(remote.frame(SCOPE, VIEW, prefix))
                },
            );
            tokio::pin!(turn);
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(turn.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        }
        assert_eq!(calls.get(), 2);
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        assert!(checkpoint.valid_remote_binding(
            &local,
            SCOPE,
            [4; 32],
            AUTHORITY,
            remote.revision()
        ));
        let page = checkpoint
            .advance_remote(
                &local,
                VIEW,
                AUTHORITY,
                remote.revision(),
                1,
                || true,
                async |prefix| Ok::<_, ()>(remote.frame(SCOPE, VIEW, prefix)),
            )
            .await
            .unwrap();
        assert_eq!(page.examined, 1);
        assert_eq!(page.progress, Progress::More);
    }

    #[tokio::test]
    async fn authorization_revoked_during_exchange_discards_reply_and_allows_retry() {
        let local = RangeIndex::default();
        let mut remote = RangeIndex::default();
        remote.insert([5; 32]);
        let mut checkpoint = Checkpoint::for_remote(SCOPE, AUTHORITY, &local, remote.revision());
        let authorized = std::cell::Cell::new(true);
        let error = checkpoint
            .advance_remote(
                &local,
                VIEW,
                AUTHORITY,
                remote.revision(),
                1,
                || authorized.get(),
                async |prefix| {
                    authorized.set(false);
                    Ok::<_, ()>(remote.frame(SCOPE, VIEW, prefix))
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RemoteError::Invalidated));
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        let page = checkpoint
            .advance_remote(
                &local,
                VIEW,
                AUTHORITY,
                remote.revision(),
                1,
                || true,
                async |prefix| Ok::<_, ()>(remote.frame(SCOPE, VIEW, prefix)),
            )
            .await
            .unwrap();
        assert_eq!(page.missing, vec![[5; 32]]);
        assert_eq!(page.progress, Progress::Complete);
    }

    #[test]
    fn accepted_discovery_requires_exact_local_acknowledgment_before_continuation() {
        let mut local = RangeIndex::default();
        local.insert([5; 32]);
        let mut remote = RangeIndex::default();
        remote.insert([5; 32]);
        remote.insert([6; 32]);
        let mut checkpoint = Checkpoint::for_remote(SCOPE, AUTHORITY, &local, remote.revision());
        let mut malformed = remote.frame(SCOPE, VIEW, Prefix::root());
        malformed.leaves.swap(0, 1);
        assert!(matches!(
            checkpoint.accept_query(&local, SCOPE, VIEW, AUTHORITY, remote.revision(), malformed,),
            Err(RemoteError::InvalidFrame)
        ));
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        let missing = checkpoint
            .accept_query(
                &local,
                SCOPE,
                VIEW,
                AUTHORITY,
                remote.revision(),
                remote.frame(SCOPE, VIEW, Prefix::root()),
            )
            .unwrap();
        assert_eq!(missing, vec![[6; 32]]);
        assert!(checkpoint.acknowledge_insertions(&mut local, &missing));
        checkpoint.defer_missing(&missing);
        let leaf = checkpoint.next_query().unwrap();
        assert!(
            checkpoint
                .accept_query(
                    &local,
                    SCOPE,
                    VIEW,
                    AUTHORITY,
                    remote.revision(),
                    remote.frame(SCOPE, VIEW, leaf),
                )
                .unwrap()
                .is_empty()
        );
        assert!(!checkpoint.has_pending());
        assert_eq!(
            local.summary(Prefix::root()),
            remote.summary(Prefix::root())
        );
    }

    #[test]
    fn reader_retry_does_not_double_count_overlap_or_mark_cold_repair_warm() {
        struct FailFifthContains {
            index: RangeIndex,
            calls: std::cell::Cell<usize>,
        }
        impl RangeRead for FailFifthContains {
            type Error = &'static str;
            fn revision(&self) -> u64 {
                self.index.revision()
            }
            fn summary(&self, prefix: Prefix) -> Result<Summary, Self::Error> {
                Ok(self.index.summary(prefix))
            }
            fn contains(&self, id: &EventId) -> Result<bool, Self::Error> {
                let call = self.calls.get() + 1;
                self.calls.set(call);
                if call == 5 {
                    Err("interrupted leaf read")
                } else {
                    Ok(self.index.contains(id))
                }
            }
        }
        let mut local = FailFifthContains {
            index: RangeIndex::default(),
            calls: std::cell::Cell::new(0),
        };
        let mut remote = RangeIndex::default();
        for n in 0..8u8 {
            remote.insert([n; 32]);
            if n < 4 {
                local.index.insert([n; 32]);
            }
        }
        let mut checkpoint =
            Checkpoint::for_reader(SCOPE, AUTHORITY, &local, remote.revision()).unwrap();
        let frame = remote.frame(SCOPE, VIEW, Prefix::root());
        assert_eq!(
            checkpoint
                .accept_reader(
                    &local,
                    SCOPE,
                    VIEW,
                    AUTHORITY,
                    remote.revision(),
                    frame.clone(),
                )
                .unwrap_err(),
            "interrupted leaf read"
        );
        assert_eq!(checkpoint.next_query(), Some(Prefix::root()));
        let missing = checkpoint
            .accept_reader(&local, SCOPE, VIEW, AUTHORITY, remote.revision(), frame)
            .unwrap()
            .unwrap();
        assert_eq!(missing, (4..8u8).map(|n| [n; 32]).collect::<Vec<_>>());
        checkpoint.begin_ordered_repair_with_anchors(&missing);
        assert!(
            !checkpoint.ordered_is_warm(),
            "four shared IDs and four missing IDs are cold; failed reads must not inflate overlap"
        );
    }
}

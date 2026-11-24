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

fn empty() -> Summary {
    Summary {
        count: 0,
        digest: Sha256::digest(b"mesh-range-empty-v1").into(),
    }
}
fn combine(bits: u16, a: Summary, b: Summary) -> Summary {
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
    blocked: BTreeSet<EventId>,
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
            blocked: BTreeSet::new(),
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
                self.pending.push(right);
                self.pending.push(left);
            } else if a.count == 0 {
                missing.push(p.key());
            }
        }
        Difference {
            progress: if self.pending.is_empty() {
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
                self.pending.push(right);
                self.pending.push(left);
            } else if b.count != 1 {
                return Err(RemoteError::InvalidFrame);
            } else if a.count == 0 {
                missing.push(prefix.key());
            }
        }
        Ok(Difference {
            progress: if self.pending.is_empty() {
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
            blocked: BTreeSet::new(),
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
        self.pending.last().copied()
    }
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn defer_missing(&mut self, ids: &[[u8; 32]]) {
        for id in ids {
            let prefix = Prefix::new(256, *id).unwrap();
            if !self.pending.contains(&prefix) {
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
            self.pending.push(right);
            self.pending.push(left);
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
        self.blocked.extend(ids.iter().copied());
    }
    pub fn has_unblocked_pending(&self) -> bool {
        self.pending
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

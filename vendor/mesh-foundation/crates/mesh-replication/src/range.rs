//! Incrementally maintained binary-prefix summaries for authorized ID sets.
//! Hosts bind an index to one scope and authorization generation; rebuilding an
//! index is explicit work, never performed by a summary query.
use crate::EventId;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Prefix {
    bits: u16,
    key: EventId,
}
impl Prefix {
    pub fn bits(self) -> u16 {
        self.bits
    }
    pub fn key(self) -> EventId {
        self.key
    }
    pub fn root() -> Self {
        Self {
            bits: 0,
            key: [0; 32],
        }
    }
    pub fn new(bits: u16, mut key: EventId) -> Option<Self> {
        if bits > 256 {
            return None;
        }
        for bit in usize::from(bits)..256 {
            key[bit / 8] &= !(128 >> (bit % 8));
        }
        Some(Self { bits, key })
    }
    pub fn children(self) -> Option<[Self; 2]> {
        if self.bits == 256 {
            return None;
        }
        let mut right = self.key;
        right[usize::from(self.bits) / 8] |= 128 >> (self.bits % 8);
        Some([
            Self::new(self.bits + 1, self.key)?,
            Self::new(self.bits + 1, right)?,
        ])
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub count: usize,
    pub digest: [u8; 32],
}
impl Summary {
    /// Canonical empty subtree, shared by memory and persistent adapters.
    pub fn empty() -> Self {
        empty()
    }
    /// Canonical leaf for an opaque record identifier.
    pub fn leaf(id: EventId) -> Self {
        Self {
            count: 1,
            digest: Sha256::digest([b"mesh-range-id-v1".as_slice(), &id].concat()).into(),
        }
    }
    /// Combine the ordered children of a non-leaf prefix. Reject impossible
    /// parent depths and count overflow before computing a canonical digest.
    pub fn combine(parent: Prefix, left: Self, right: Self) -> Option<Self> {
        parent.children()?;
        left.count.checked_add(right.count)?;
        Some(combine(parent.bits(), left, right))
    }
}

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
    /// Membership lookup without exposing the index representation.
    pub fn contains(&self, id: &EventId) -> bool {
        self.ids.contains(id)
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn summary(&self, p: Prefix) -> Summary {
        if let Some(summary) = self.nodes.get(&p) {
            return *summary;
        }
        let mut upper = p.key;
        for bit in usize::from(p.bits)..256 {
            upper[bit / 8] |= 128 >> (bit % 8);
        }
        let mut range = self.ids.range(p.key..=upper);
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
        for bits in (p.bits..depth).rev() {
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
                missing.push(p.key);
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
pub const PROFILE: &[u8; 4] = b"MRG1";
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryFrame {
    pub scope: [u8; 32],
    pub view: [u8; 32],
    pub prefix: Prefix,
    pub summary: Summary,
    pub singleton: Option<EventId>,
    pub leaves: Vec<EventId>,
}
impl SummaryFrame {
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(142);
        bytes.extend_from_slice(PROFILE);
        bytes.extend_from_slice(&self.scope);
        bytes.extend_from_slice(&self.view);
        bytes.extend_from_slice(&self.prefix.bits.to_le_bytes());
        bytes.extend_from_slice(&self.prefix.key);
        bytes.extend_from_slice(&(self.summary.count as u64).to_le_bytes());
        bytes.extend_from_slice(&self.summary.digest);
        if let Some(id) = self.singleton {
            bytes.extend_from_slice(&id);
        }
        for id in &self.leaves {
            bytes.extend_from_slice(id);
        }
        bytes
    }
    /// Bounded frame length limits allocation before decode. The caller authorizes the
    /// view token independently; it is not a remote authorization grant.
    pub fn decode(bytes: &[u8], scope: [u8; 32], view: [u8; 32]) -> Option<Self> {
        if !(142..=1166).contains(&bytes.len())
            || !(bytes.len() - 142).is_multiple_of(32)
            || &bytes[..4] != PROFILE
            || bytes[4..36] != scope
            || bytes[36..68] != view
        {
            return None;
        }
        let bits = u16::from_le_bytes(bytes[68..70].try_into().ok()?);
        let key: EventId = bytes[70..102].try_into().ok()?;
        let prefix = Prefix::new(bits, key)?;
        if prefix.key != key {
            return None;
        }
        let count = usize::try_from(u64::from_le_bytes(bytes[102..110].try_into().ok()?)).ok()?;
        let frame = Self {
            scope,
            view,
            prefix,
            leaves: if bytes.len() > 174 {
                bytes[142..].as_chunks::<32>().0.to_vec()
            } else {
                Vec::new()
            },
            singleton: if bytes.len() == 174 {
                Some(bytes[142..174].try_into().ok()?)
            } else {
                None
            },
            summary: Summary {
                count,
                digest: bytes[110..142].try_into().ok()?,
            },
        };
        frame.valid_leaves().then_some(frame)
    }
    pub fn valid_leaves(&self) -> bool {
        if self.leaves.is_empty() {
            return self
                .singleton
                .is_none_or(|id| singleton_summary(self.prefix, id) == Some(self.summary));
        }
        if self.singleton.is_some()
            || !(2..=32).contains(&self.leaves.len())
            || self.summary.count != self.leaves.len()
            || self.leaves.windows(2).any(|w| w[0] >= w[1])
            || self
                .leaves
                .iter()
                .any(|id| Prefix::new(self.prefix.bits, *id) != Some(self.prefix))
        {
            return false;
        }
        let mut index = RangeIndex::default();
        for id in &self.leaves {
            index.insert(*id);
        }
        index.summary(self.prefix) == self.summary
    }
}

#[derive(Debug)]
pub enum RemoteError<E> {
    Exchange(E),
    InvalidFrame,
    Invalidated,
}
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
                missing.push(prefix.key);
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

/// Canonical summary for one ID in this prefix; rejects an ID outside it.
pub fn singleton_summary(prefix: Prefix, id: EventId) -> Option<Summary> {
    if Prefix::new(prefix.bits, id)? != prefix {
        return None;
    }
    let mut summary = Summary {
        count: 1,
        digest: Sha256::digest([b"mesh-range-id-v1".as_slice(), &id].concat()).into(),
    };
    for bits in (prefix.bits..256).rev() {
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
            self.ids.range(prefix.key..).next().copied()
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
                    .range(prefix.key..)
                    .take(summary.count)
                    .copied()
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
}

/// Request one cached range summary over a negotiated dedicated range session.
pub async fn query_summary<S: crate::session::ReconciliationSession>(
    session: &mut S,
    scope: [u8; 32],
    view: [u8; 32],
    prefix: Prefix,
) -> Result<SummaryFrame, RemoteError<S::Error>> {
    let mut bytes = Vec::with_capacity(102);
    bytes.extend_from_slice(b"MRGQ");
    bytes.extend_from_slice(&scope);
    bytes.extend_from_slice(&view);
    bytes.extend_from_slice(&prefix.bits.to_le_bytes());
    bytes.extend_from_slice(&prefix.key);
    session.send(&bytes).await.map_err(RemoteError::Exchange)?;
    let response = session.recv().await.map_err(RemoteError::Exchange)?;
    let frame = SummaryFrame::decode(&response, scope, view).ok_or(RemoteError::InvalidFrame)?;
    if frame.prefix != prefix {
        return Err(RemoteError::InvalidFrame);
    }
    Ok(frame)
}
/// Serve one request against a host-qualified immutable snapshot. Negotiate the
/// MRG1 capability outside the dedicated session before using this profile.
pub async fn serve_summary<S: crate::session::ReconciliationSession>(
    session: &mut S,
    index: &RangeIndex,
    scope: [u8; 32],
    view: [u8; 32],
    mut authorize: impl FnMut() -> bool,
) -> Result<(), RemoteError<S::Error>> {
    if !authorize() {
        return Err(RemoteError::Invalidated);
    }
    let bytes = session.recv().await.map_err(RemoteError::Exchange)?;
    if !authorize() {
        return Err(RemoteError::Invalidated);
    }
    if bytes.len() != 102
        || &bytes[..4] != b"MRGQ"
        || bytes[4..36] != scope
        || bytes[36..68] != view
    {
        return Err(RemoteError::InvalidFrame);
    }
    let bits = u16::from_le_bytes(bytes[68..70].try_into().unwrap());
    let key: EventId = bytes[70..102].try_into().unwrap();
    let prefix = Prefix::new(bits, key).ok_or(RemoteError::InvalidFrame)?;
    if prefix.key != key {
        return Err(RemoteError::InvalidFrame);
    }
    let response = index.frame(scope, view, prefix).encode();
    if !authorize() {
        return Err(RemoteError::Invalidated);
    }
    session.send(&response).await.map_err(RemoteError::Exchange)
}

/// Fallible reads for a disk-backed authorized snapshot. Storage failure is
/// separate from protocol invalidation and must never be treated as equality.
/// Every read and revision must describe the same pinned authorized snapshot.
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
    /// Next undiscovered summary; absence says nothing about body durability.
    pub fn next_query(&self) -> Option<Prefix> {
        self.pending.last().copied()
    }
    /// Whether summary discovery remains, independent of returned missing IDs.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
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
    /// SQL read therefore retains the exact prefix for retry. Returned missing
    /// IDs must remain paired with the advanced checkpoint until durable commit;
    /// otherwise discard/restart the checkpoint after cancellation or failure.
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
        let mut missing = Vec::new();
        if !equal {
            for id in frame.leaves.iter().copied().chain(frame.singleton) {
                if !local.contains(&id)? {
                    missing.push(id);
                }
            }
            if frame.leaves.is_empty()
                && frame.singleton.is_none()
                && prefix.bits == 256
                && frame.summary.count != 1
            {
                return Ok(Err(RemoteError::InvalidFrame));
            }
        }
        self.remote_view = Some(view);
        self.pending.pop();
        if !equal && frame.leaves.is_empty() && frame.singleton.is_none() {
            if let Some([left, right]) = prefix.children() {
                self.pending.push(right);
                self.pending.push(left);
            } else {
                missing.push(prefix.key);
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

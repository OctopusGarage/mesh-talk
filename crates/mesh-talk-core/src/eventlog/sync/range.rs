//! Bounded preparation of qualified authorized range snapshots.
use super::network::{IdPageBudget, SyncPage};
use crate::eventlog::{EventId, LogError};
use rand_core::RngCore;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
pub mod index;
pub(crate) mod sql;
use index::RangeRead;
const INDEX_BYTES: usize = 32 * 1024 * 1024;
const ID_BYTES: usize = 1024;
pub(crate) fn is_index_capacity_error(error: &LogError) -> bool {
    matches!(error,LogError::PageBudgetTooSmall{required} if *required==INDEX_BYTES+ID_BYTES)
}
#[cfg(test)]
thread_local! {static FAIL_RESERVATION:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
static RETAINED_INDEX_BYTES: AtomicUsize = AtomicUsize::new(0);
#[derive(Default)]
struct IndexReservation(usize);
impl IndexReservation {
    fn grow_by(&mut self, ids: usize) -> Result<(), LogError> {
        let bytes = ids
            .checked_mul(ID_BYTES)
            .ok_or(LogError::InvalidPageBudget)?;
        #[cfg(test)]
        if FAIL_RESERVATION.with(|flag| flag.replace(false)) {
            return Err(LogError::PageBudgetTooSmall {
                required: INDEX_BYTES + ID_BYTES,
            });
        }
        let mut observed = RETAINED_INDEX_BYTES.load(Ordering::Acquire);
        loop {
            let next = observed
                .checked_add(bytes)
                .filter(|n| *n <= INDEX_BYTES)
                .ok_or(LogError::PageBudgetTooSmall {
                    required: INDEX_BYTES + ID_BYTES,
                })?;
            match RETAINED_INDEX_BYTES.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        self.0 += bytes;
        Ok(())
    }
}
impl Drop for IndexReservation {
    fn drop(&mut self) {
        RETAINED_INDEX_BYTES.fetch_sub(self.0, Ordering::AcqRel);
    }
}
#[derive(Default)]
struct OwnedIndex {
    index: index::RangeIndex,
    reservation: IndexReservation,
    sql: Option<sql::SqlIndex>,
}
impl RangeRead for OwnedIndex {
    type Error = LogError;
    fn revision(&self) -> u64 {
        self.sql
            .as_ref()
            .map_or(self.index.revision(), RangeRead::revision)
    }
    fn summary(&self, p: index::Prefix) -> Result<index::Summary, LogError> {
        match &self.sql {
            Some(sql) => sql.summary(p),
            None => Ok(self.index.summary(p)),
        }
    }
    fn contains(&self, id: &[u8; 32]) -> Result<bool, LogError> {
        match &self.sql {
            Some(sql) => sql.contains(id),
            None => Ok(self.index.contains(id).unwrap()),
        }
    }
}
/// Immutable index lease. Accounting retires only after the final snapshot drops.
#[derive(Clone)]
pub struct RangeSnapshot(Arc<OwnedIndex>, [u8; 32]);
impl RangeSnapshot {
    pub fn index(&self) -> &(dyn RangeRead<Error = LogError> + Send + Sync) {
        &*self.0
    }
    pub fn token(&self) -> [u8; 32] {
        self.1
    }
}
pub enum RangeReceiverProgress {
    Unsupported,
    More,
    Ready(RangeSnapshot),
}
pub enum RangeProgress {
    Unsupported,
    More,
    Ready {
        token: [u8; 32],
        revision: u64,
        count: u64,
        digest: [u8; 32],
    },
}
pub struct RangeBuild<C> {
    cursor: Option<C>,
    owned: Arc<OwnedIndex>,
    token: [u8; 32],
    ready: bool,
    resetting: bool,
}
impl<C> Default for RangeBuild<C> {
    fn default() -> Self {
        let mut token = [0; 32];
        rand_core::OsRng.fill_bytes(&mut token);
        Self {
            cursor: None,
            owned: Arc::new(OwnedIndex::default()),
            token,
            ready: false,
            resetting: false,
        }
    }
}
impl<C> RangeBuild<C> {
    /// SQL page caches belong to the shared encrypted pager, not this lease.
    /// Memory-backend reservations remain conservatively charged to the owner.
    pub(crate) fn retained_heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + std::mem::size_of::<OwnedIndex>() + self.owned.reservation.0
    }

    pub(crate) fn bind_sql(&mut self, index: sql::SqlIndex) -> Result<(), LogError> {
        if let Some(bound) = &self.owned.sql {
            return if bound.same_selection_lane(&index) {
                Ok(())
            } else {
                Err(LogError::StaleCursor)
            };
        }
        let owned = Arc::get_mut(&mut self.owned).ok_or(LogError::StaleCursor)?;
        if owned.sql.is_none() && owned.index.summary(index::Prefix::root()).count == 0 {
            owned.sql = Some(index);
        }
        Ok(())
    }

    pub fn resume_after(&mut self, cursor: C) {
        self.cursor = Some(cursor);
        self.ready = false;
        rand_core::OsRng.fill_bytes(&mut self.token);
    }
    pub fn snapshot(&self) -> Option<RangeSnapshot> {
        self.ready
            .then(|| RangeSnapshot(self.owned.clone(), self.token))
    }
    pub fn advance(
        &mut self,
        expected: Option<u64>,
        page: impl FnMut(Option<&C>, IdPageBudget) -> Result<SyncPage<EventId, C>, LogError>,
    ) -> Result<RangeProgress, LogError> {
        self.advance_with_page_limit(expected, 4, page)
    }
    /// Reserve a turn's existing operation slots for preceding preparation.
    /// This can only reduce the original four-page ceiling.
    pub(crate) fn advance_with_page_limit(
        &mut self,
        expected: Option<u64>,
        pages: usize,
        mut page: impl FnMut(Option<&C>, IdPageBudget) -> Result<SyncPage<EventId, C>, LogError>,
    ) -> Result<RangeProgress, LogError> {
        if !(1..=4).contains(&pages) {
            return Err(LogError::InvalidPageBudget);
        }
        let result = self.advance_inner(expected, pages, &mut page);
        if result
            .as_ref()
            .is_err_and(|error| !is_index_capacity_error(error))
        {
            if self.owned.sql.is_some() {
                self.resetting = true;
                self.ready = false;
                self.cursor = None;
                rand_core::OsRng.fill_bytes(&mut self.token);
            } else {
                *self = Self::default();
            }
        }
        result
    }
    fn advance_inner(
        &mut self,
        expected: Option<u64>,
        pages: usize,
        page: &mut impl FnMut(Option<&C>, IdPageBudget) -> Result<SyncPage<EventId, C>, LogError>,
    ) -> Result<RangeProgress, LogError> {
        if self.resetting {
            let Some(owned) = Arc::get_mut(&mut self.owned) else {
                return Ok(RangeProgress::More);
            };
            if !owned
                .sql
                .as_mut()
                .ok_or(LogError::StaleCursor)?
                .clear_step()?
            {
                return Ok(RangeProgress::More);
            }
            self.resetting = false;
            return Ok(RangeProgress::More);
        }
        if !self.ready {
            let mut examined = 0;
            for _ in 0..pages {
                let remaining = 1000usize.saturating_sub(examined);
                if remaining == 0 {
                    break;
                }
                // At most 256 insertions per synchronous turn, independent of scan budget.
                let budget = IdPageBudget {
                    ids: remaining.min(32),
                    examined: remaining.min(250),
                };
                let next = page(self.cursor.as_ref(), budget)?;
                if next.examined > budget.examined
                    || next.items.len() > budget.ids
                    || (next.next.is_some() && next.examined == 0 && next.items.is_empty())
                {
                    return Err(LogError::InvalidPageBudget);
                }
                examined += next.examined;
                let owned = Arc::get_mut(&mut self.owned).ok_or(LogError::StaleCursor)?;
                // Reserve a whole page before mutating its index or cursor;
                // capacity eviction may then safely retry this exact page.
                if let Some(sql) = &mut owned.sql {
                    sql.insert_page(
                        &next
                            .items
                            .iter()
                            .map(|id| *id.as_bytes())
                            .collect::<Vec<_>>(),
                    )?;
                } else {
                    owned.reservation.grow_by(next.items.len())?;
                    for id in next.items {
                        if !owned.index.insert(*id.as_bytes()) {
                            return Err(LogError::StaleCursor);
                        }
                    }
                }
                self.cursor = next.next;
                if self.cursor.is_none() {
                    if let Some(n) = expected
                        && self.owned.summary(index::Prefix::root())?.count as u64 != n
                    {
                        return Err(LogError::StaleCursor);
                    }
                    self.ready = true;
                    break;
                }
            }
        }
        if !self.ready {
            return Ok(RangeProgress::More);
        }
        let summary = self.owned.summary(index::Prefix::root())?;
        Ok(RangeProgress::Ready {
            token: self.token,
            revision: self.owned.revision(),
            count: summary.count as u64,
            digest: summary.digest,
        })
    }
    pub fn answer(&self, scope: [u8; 32], request: &[u8]) -> Result<Vec<u8>, LogError> {
        if !self.ready
            || request.len() != 102
            || &request[..4] != b"MRGQ"
            || request[4..36] != scope
            || request[36..68] != self.token
        {
            return Err(LogError::StaleCursor);
        }
        let bits = u16::from_le_bytes(request[68..70].try_into().unwrap());
        let key: [u8; 32] = request[70..102].try_into().unwrap();
        let prefix = index::Prefix::new(bits, key).ok_or(LogError::InvalidPageBudget)?;
        if prefix.key() != key {
            return Err(LogError::InvalidPageBudget);
        }
        match &self.owned.sql {
            Some(sql) => Ok(sql.frame(scope, self.token, prefix)?.encode()),
            None => Ok(self.owned.index.frame(scope, self.token, prefix).encode()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: usize) -> EventId {
        let mut id = [0; 32];
        id[..8].copy_from_slice(&(n as u64).to_be_bytes());
        EventId::new(id)
    }
    #[test]
    fn reduced_range_page_allowance_preserves_its_cursor_and_default_ceiling() {
        use std::cell::Cell;
        let mut build = RangeBuild::<usize>::default();
        let calls = Cell::new(0);
        let examined = Cell::new(0);
        let mut page = |after: Option<&usize>, budget: IdPageBudget| {
            calls.set(calls.get() + 1);
            assert!(budget.ids <= 32 && budget.examined <= 250);
            let after = after.copied().unwrap_or(0);
            let end = (after + budget.ids).min(130);
            examined.set(examined.get() + end - after);
            Ok(SyncPage {
                items: (after..end).map(id).collect(),
                next: (end < 130).then_some(end),
                examined: end - after,
            })
        };
        assert!(matches!(
            build
                .advance_with_page_limit(Some(130), 2, &mut page)
                .unwrap(),
            RangeProgress::More
        ));
        assert_eq!((calls.get(), examined.get()), (2, 64));
        assert!(build.snapshot().is_none());
        assert!(matches!(
            build
                .advance_with_page_limit(Some(130), 2, &mut page)
                .unwrap(),
            RangeProgress::More
        ));
        assert_eq!((calls.get(), examined.get()), (4, 128));
        assert!(matches!(
            build
                .advance_with_page_limit(Some(130), 2, &mut page)
                .unwrap(),
            RangeProgress::Ready { count: 130, .. }
        ));
        assert_eq!(
            (calls.get(), examined.get()),
            (5, 130),
            "reducing a turn must not restart already examined records"
        );
        assert!(matches!(
            build.advance_with_page_limit(Some(130), 5, &mut page),
            Err(LogError::InvalidPageBudget)
        ));
    }

    #[test]
    fn bounded_range_build_never_exposes_a_partial_snapshot() {
        let mut build = RangeBuild::<usize>::default();
        let mut examined = 0;
        let mut page = |after: Option<&usize>, budget: IdPageBudget| {
            let after = after.copied().unwrap_or(0);
            let end = (after + budget.ids).min(257);
            examined += end - after;
            Ok(SyncPage {
                items: (after..end).map(id).collect(),
                next: (end < 257).then_some(end),
                examined: end - after,
            })
        };
        assert!(matches!(
            build.advance(Some(257), &mut page).unwrap(),
            RangeProgress::More
        ));
        assert!(build.snapshot().is_none());
        assert!(matches!(
            build.advance(Some(257), &mut page).unwrap(),
            RangeProgress::More
        ));
        assert!(matches!(
            build.advance(Some(257), &mut page).unwrap(),
            RangeProgress::Ready { count: 257, .. }
        ));
        assert_eq!(examined, 257);
        let snapshot = build.snapshot().unwrap();
        assert_eq!(
            snapshot
                .index()
                .summary(index::Prefix::root())
                .unwrap()
                .count,
            257
        );
    }
    #[test]
    fn capacity_retry_preserves_prior_pages_without_partial_insertions() {
        let mut build = RangeBuild::<usize>::default();
        let mut page = |after: Option<&usize>, budget: IdPageBudget| {
            let after = after.copied().unwrap_or(0);
            let end = (after + budget.ids).min(129);
            Ok(SyncPage {
                items: (after..end).map(id).collect(),
                next: (end < 129).then_some(end),
                examined: end - after,
            })
        };
        assert!(matches!(
            build.advance(Some(129), &mut page).unwrap(),
            RangeProgress::More
        ));
        FAIL_RESERVATION.with(|flag| flag.set(true));
        assert!(
            build
                .advance(Some(129), &mut page)
                .as_ref()
                .is_err_and(is_index_capacity_error)
        );
        assert!(matches!(
            build.advance(Some(129), &mut page).unwrap(),
            RangeProgress::Ready { count: 129, .. }
        ));
    }
    #[test]
    fn empty_nonadvancing_page_invalidates_partial_builder() {
        let mut build = RangeBuild::<usize>::default();
        assert!(
            build
                .advance(None, |_, _| Ok(SyncPage {
                    items: vec![],
                    next: Some(0),
                    examined: 0
                }))
                .is_err()
        );
        assert!(build.snapshot().is_none());
    }
    #[test]
    fn wrong_scope_or_snapshot_cannot_read_a_summary() {
        let mut build = RangeBuild::<usize>::default();
        let token = match build
            .advance(Some(0), |_, _| {
                Ok(SyncPage {
                    items: vec![],
                    next: None,
                    examined: 0,
                })
            })
            .unwrap()
        {
            RangeProgress::Ready { token, .. } => token,
            _ => panic!(),
        };
        let request = [
            b"MRGQ".as_slice(),
            &[7; 32],
            &token,
            &0u16.to_le_bytes(),
            &[0; 32],
        ]
        .concat();
        assert!(build.answer([7; 32], &request).is_ok());
        assert!(build.answer([8; 32], &request).is_err());
        let mut forged = request;
        forged[36] ^= 1;
        assert!(build.answer([7; 32], &forged).is_err());
    }
}

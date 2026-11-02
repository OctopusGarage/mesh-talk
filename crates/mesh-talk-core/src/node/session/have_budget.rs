//! Process-owner RAM admission for remote inventories. Admission never waits.
use super::{EventId, MAX_HAVE_IDS_TOTAL, SessionError};
use std::{collections::HashSet, ops::Deref, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const RESERVATION_BYTES: usize = 32 * 1024 * 1024;
/// Four full reservations bound dedicated relay Have memory across all endpoints.
/// This is a logical allocation budget; allocator/RSS overhead is measured separately.
#[derive(Clone)]
pub struct RemoteHaveBudget(Arc<Semaphore>);
impl Default for RemoteHaveBudget {
    fn default() -> Self {
        Self(Arc::new(Semaphore::new(4)))
    }
}
impl RemoteHaveBudget {
    pub(crate) fn try_admit(&self) -> Result<OwnedSemaphorePermit, SessionError> {
        self.0.clone().try_acquire_owned().map_err(|_| overload())
    }
}
fn overload() -> SessionError {
    SessionError::ResourceUnavailable
}

pub(super) struct ReceivedHave {
    ids: HashSet<EventId>,
    // Must outlive both collection conversion and every network await using ids.
    _permit: Option<OwnedSemaphorePermit>,
    #[cfg(test)]
    accounted: usize,
}
impl ReceivedHave {
    pub(super) fn from_vec(
        ids: Vec<EventId>,
        permit: Option<OwnedSemaphorePermit>,
    ) -> Result<Self, SessionError> {
        if ids.len() > MAX_HAVE_IDS_TOTAL || ids.capacity() > MAX_HAVE_IDS_TOTAL {
            return Err(overload());
        }
        let mut set = HashSet::new();
        set.try_reserve(ids.len()).map_err(|_| overload())?;
        // Conservative supported-implementation load/control/alignment allowance.
        // Full admission already precedes both allocations. Verify actual capacities,
        // not len, before conversion; the permit remains held on every error path.
        let bytes = ids
            .capacity()
            .checked_mul(size_of::<EventId>())
            .and_then(|n| {
                set.capacity()
                    .checked_mul(2 * (size_of::<EventId>() + size_of::<usize>() + 1))
                    .and_then(|m| n.checked_add(m))
            })
            .and_then(|n| n.checked_add(4 * crate::transport::MAX_PLAINTEXT))
            .ok_or_else(overload)?;
        if bytes > RESERVATION_BYTES {
            return Err(overload());
        }
        set.extend(ids);
        Ok(Self {
            ids: set,
            _permit: permit,
            #[cfg(test)]
            accounted: bytes,
        })
    }
    #[cfg(test)]
    pub(super) fn accounted_bytes(&self) -> usize {
        self.accounted
    }
}
impl Deref for ReceivedHave {
    type Target = HashSet<EventId>;
    fn deref(&self) -> &Self::Target {
        &self.ids
    }
}

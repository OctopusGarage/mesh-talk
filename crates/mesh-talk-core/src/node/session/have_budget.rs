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
    comparison_fingerprint: Option<[u8; 32]>,
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
        Self::from_vec_for_comparison(ids, permit, false)
    }
    pub(super) fn from_vec_for_comparison(
        mut ids: Vec<EventId>,
        permit: Option<OwnedSemaphorePermit>,
        eligible: bool,
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
        let comparison_fingerprint = if eligible {
            use sha2::{Digest, Sha256};
            ids.sort_unstable();
            if ids.windows(2).any(|pair| pair[0] == pair[1]) {
                None
            } else {
                let mut hash = Sha256::new();
                hash.update((ids.len() as u64).to_le_bytes());
                for id in &ids {
                    hash.update(id.as_bytes());
                }
                Some(hash.finalize().into())
            }
        } else {
            None
        };
        set.extend(ids);
        Ok(Self {
            ids: set,
            comparison_fingerprint,
            _permit: permit,
            #[cfg(test)]
            accounted: bytes,
        })
    }
    pub(super) fn comparison_fingerprint(&self) -> Option<[u8; 32]> {
        self.comparison_fingerprint
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

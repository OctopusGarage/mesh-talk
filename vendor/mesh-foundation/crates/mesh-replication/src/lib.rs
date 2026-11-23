//! Bounded exact inventory comparison for an already authorized event range.
//! A digest or inventory match does not grant read, write, or custody authority.

use sha2::{Digest, Sha256};

pub type EventId = [u8; 32];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InventoryError {
    PageLimit,
    TotalLimit,
    Allocation,
    Duplicate,
}

impl std::fmt::Display for InventoryError {
    fn fmt(&self, format: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        format.write_str(match self {
            Self::PageLimit => "inventory page exceeds its ID budget",
            Self::TotalLimit => "inventory exceeds its total ID budget",
            Self::Allocation => "unable to allocate bounded inventory",
            Self::Duplicate => "inventory contains duplicate event IDs",
        })
    }
}

impl std::error::Error for InventoryError {}

/// One peer's IDs for a single finite, authorized range. The caller owns the
/// frame and round budgets and discards this builder after a failed exchange.
pub struct Inventory {
    ids: Vec<EventId>,
    total_limit: usize,
    page_limit: usize,
}

impl Inventory {
    pub fn new(total_limit: usize, page_limit: usize) -> Self {
        Self {
            ids: Vec::new(),
            total_limit,
            page_limit,
        }
    }

    /// Accept a whole page or leave the logical inventory unchanged.
    pub fn add_page(&mut self, page: &[EventId]) -> Result<(), InventoryError> {
        if page.len() > self.page_limit {
            return Err(InventoryError::PageLimit);
        }
        let required = self
            .ids
            .len()
            .checked_add(page.len())
            .filter(|&count| count <= self.total_limit)
            .ok_or(InventoryError::TotalLimit)?;
        if required > self.ids.capacity() {
            let capacity = required
                .max(self.ids.capacity().saturating_mul(2))
                .min(self.total_limit);
            self.ids
                .try_reserve_exact(capacity - self.ids.len())
                .map_err(|_| InventoryError::Allocation)?;
            if self.ids.capacity() > self.total_limit {
                return Err(InventoryError::Allocation);
            }
        }
        self.ids.extend_from_slice(page);
        Ok(())
    }

    pub fn finish(mut self) -> Result<CompleteInventory, InventoryError> {
        self.ids.sort_unstable();
        if self.ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(InventoryError::Duplicate);
        }
        let mut digest = InventoryDigest::new(self.ids.len() as u64);
        for id in &self.ids {
            digest.update(id);
        }
        Ok(CompleteInventory {
            ids: self.ids,
            fingerprint: digest.finish(),
        })
    }
}

/// Streaming Mesh Talk-compatible digest for a finite, ordered inventory.
/// The caller owns the snapshot and supplies exactly the declared number of
/// IDs in lexicographic order. This type only computes a summary.
pub struct InventoryDigest(Sha256);

impl InventoryDigest {
    pub fn new(count: u64) -> Self {
        let mut digest = Sha256::new();
        digest.update(count.to_le_bytes());
        Self(digest)
    }

    pub fn update(&mut self, id: &EventId) {
        self.0.update(id);
    }

    pub fn finish(self) -> [u8; 32] {
        self.0.finalize().into()
    }
}

/// An immutable, sorted ID set. No event payloads are retained.
pub struct CompleteInventory {
    ids: Vec<EventId>,
    fingerprint: [u8; 32],
}

impl CompleteInventory {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// IDs held here and absent from `peer`, in canonical order.
    /// Both arguments must describe the same authorized range.
    pub fn missing_from<'a>(&'a self, peer: &'a Self) -> impl Iterator<Item = EventId> + 'a {
        let mut remote = peer.ids.iter().peekable();
        self.ids.iter().copied().filter(move |local| {
            while remote.peek().is_some_and(|id| *id < local) {
                remote.next();
            }
            remote.peek().is_none_or(|id| *id != local)
        })
    }
}

/// Mesh Talk signed event compatibility codec.
pub mod legacy_event;

/// Transport-independent framed reconciliation.
pub mod session;

/// Mesh Talk-compatible synchronization frame codec.
pub mod legacy_wire;

/// Requester engine for Mesh Talk-compatible sync rounds.
pub mod legacy_round;

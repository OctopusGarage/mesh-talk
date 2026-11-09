//! Runtime work evidence; ordinary database writes remain conservative fences.
//! Only guarded disposable selection transactions may acknowledge their writes.
use super::{Connection, MainWriteStamp};
use std::{
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(Default))]
pub(crate) struct WorkStamp {
    owner: u64,
    generation: u64,
}
impl WorkStamp {
    pub(crate) fn unchanged_since(&self, current: &Self) -> bool {
        self == current
    }
}

struct Clock {
    owner: Option<u64>,
    generation: Option<u64>,
    last: Option<MainWriteStamp>,
}
impl Default for Clock {
    fn default() -> Self {
        static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);
        let mut observed = NEXT_OWNER.load(Ordering::Relaxed);
        let owner = loop {
            let Some(next) = observed.checked_add(1) else {
                break None;
            };
            match NEXT_OWNER.compare_exchange_weak(
                observed,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break Some(observed),
                Err(current) => observed = current,
            }
        };
        Self {
            owner,
            generation: Some(0),
            last: None,
        }
    }
}
impl Clock {
    fn observe(&mut self, stamp: Option<MainWriteStamp>) -> Option<WorkStamp> {
        let Some(stamp) = stamp else {
            self.generation = None;
            return None;
        };
        if self.last.is_some_and(|old| !old.unchanged_since(&stamp)) {
            self.generation = self.generation.and_then(|n| n.checked_add(1));
        }
        self.last = Some(stamp);
        Some(WorkStamp {
            owner: self.owner?,
            generation: self.generation?,
        })
    }
    fn acknowledge_selection(&mut self, stamp: Option<MainWriteStamp>) {
        if stamp.is_none() {
            self.generation = None;
        }
        self.last = stamp;
    }
}

pub(super) struct OwnedConnection {
    connection: Connection,
    clock: Clock,
}
impl OwnedConnection {
    pub(super) fn new(connection: Connection) -> Self {
        Self {
            connection,
            clock: Clock::default(),
        }
    }
    pub(super) fn observe_work(&mut self, stamp: Option<MainWriteStamp>) -> Option<WorkStamp> {
        self.clock.observe(stamp)
    }
    pub(super) fn acknowledge_selection(&mut self, stamp: Option<MainWriteStamp>) {
        self.clock.acknowledge_selection(stamp);
    }
    pub(super) fn into_connection(self) -> Connection {
        self.connection
    }
}
impl Deref for OwnedConnection {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        &self.connection
    }
}
impl DerefMut for OwnedConnection {
    fn deref_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn raw(n: u32) -> Option<MainWriteStamp> {
        Some(MainWriteStamp {
            version: n,
            writes: n as i32,
        })
    }
    #[test]
    fn derived_acknowledgement_preserves_prior_writes_and_owner_identity() {
        let mut clock = Clock::default();
        let first = clock.observe(raw(1)).unwrap();
        clock.acknowledge_selection(raw(2));
        assert_eq!(clock.observe(raw(2)), Some(first));
        let changed = clock.observe(raw(3)).unwrap();
        assert_ne!(changed, first);
        clock.acknowledge_selection(raw(4));
        assert_eq!(clock.observe(raw(4)), Some(changed));
        assert_ne!(Clock::default().observe(raw(4)), Some(changed));
    }
    #[test]
    fn unknown_or_exhausted_work_evidence_cannot_be_reenabled_by_acknowledgement() {
        let mut clock = Clock::default();
        assert!(clock.observe(raw(1)).is_some());
        assert!(clock.observe(None).is_none());
        clock.acknowledge_selection(raw(2));
        assert!(clock.observe(raw(2)).is_none());
        let mut clock = Clock::default();
        clock.observe(raw(1));
        clock.generation = Some(u64::MAX);
        assert!(clock.observe(raw(2)).is_none());
        clock.acknowledge_selection(raw(3));
        assert!(clock.observe(raw(3)).is_none());
    }
}

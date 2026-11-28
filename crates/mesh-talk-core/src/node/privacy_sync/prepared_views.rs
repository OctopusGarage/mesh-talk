//! Node-owned, version-qualified derived plans. Values are checked out exclusively;
//! admission returns retired values so SQL destruction never runs under its mutex.
use super::network::HistorySyncStamp;
use crate::{eventlog::ConversationId, identity::device::PublicIdentity};
use std::{collections::VecDeque, time::Instant};

const MAX_ENTRIES: usize = 64;
// Other 32 MiB is reserved globally for prepared authorized range indexes.
const MAX_BYTES: usize = 32 * 1024 * 1024;
const MAX_SQL_BYTES: usize = 512 * 1024 * 1024;
#[cfg(test)]
thread_local! {static REJECT_IDLE:std::cell::Cell<bool>=const {std::cell::Cell::new(false)};}
#[cfg(test)]
pub(in crate::node) struct RejectIdleCache;
#[cfg(test)]
impl RejectIdleCache {
    pub(in crate::node) fn enter() -> Self {
        REJECT_IDLE.with(|flag| flag.set(true));
        Self
    }
}
#[cfg(test)]
impl Drop for RejectIdleCache {
    fn drop(&mut self) {
        REJECT_IDLE.with(|flag| flag.set(false));
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    Ordinary,
    History,
}

#[derive(Clone, PartialEq, Eq)]
pub(in crate::node) struct Scope {
    pub(super) peer: PublicIdentity,
    pub(super) purpose: Purpose,
    pub(super) conversation: ConversationId,
    pub(super) inbound: bool,
    pub(super) pull_only: bool,
    pub(super) denied: bool,
}
struct Entry<T> {
    scope: Scope,
    stamp: HistorySyncStamp,
    bytes: usize,
    sql_bytes: usize,
    returned: Instant,
    value: T,
}
pub(in crate::node) struct PreparedCache<T> {
    entries: VecDeque<Entry<T>>,
    bytes: usize,
    sql_bytes: usize,
    #[cfg(test)]
    test_elapsed: std::time::Duration,
}
impl<T> Default for PreparedCache<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            sql_bytes: 0,
            #[cfg(test)]
            test_elapsed: std::time::Duration::ZERO,
        }
    }
}
impl<T> PreparedCache<T> {
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn accepts_bytes(bytes: usize) -> bool {
        #[cfg(test)]
        if REJECT_IDLE.with(|flag| flag.get()) {
            return false;
        }
        bytes <= MAX_BYTES
    }
    /// Extract one oldest idle owner. Destruction must happen after releasing
    /// the cache mutex; this never reaches an active checked-out view.
    pub(super) fn retire_oldest(&mut self) -> Option<T> {
        let index = self
            .entries
            .iter()
            .enumerate()
            .min_by_key(|(index, entry)| (entry.returned, *index))
            .map(|(index, _)| index)?;
        let entry = self.entries.remove(index)?;
        self.bytes -= entry.bytes;
        self.sql_bytes -= entry.sql_bytes;
        Some(entry.value)
    }
    fn extract(&mut self, mut remove: impl FnMut(&Entry<T>) -> bool, retired: &mut Vec<T>) {
        let mut index = 0;
        while index < self.entries.len() {
            if remove(&self.entries[index]) {
                let entry = self.entries.remove(index).expect("bounded prepared entry");
                self.bytes -= entry.bytes;
                self.sql_bytes -= entry.sql_bytes;
                retired.push(entry.value);
            } else {
                index += 1;
            }
        }
    }
    /// A candidate only. The caller must independently requalify source and
    /// policy before using any cached permission or index.
    pub(super) fn take_candidate(&mut self, scope: &Scope) -> Option<T> {
        let index = self
            .entries
            .iter()
            .position(|entry| &entry.scope == scope)?;
        let entry = self.entries.remove(index)?;
        self.bytes -= entry.bytes;
        self.sql_bytes -= entry.sql_bytes;
        Some(entry.value)
    }
    pub(super) fn take(&mut self, scope: &Scope, stamp: &HistorySyncStamp) -> (Option<T>, Vec<T>) {
        let mut retired = Vec::new();
        self.extract(
            |entry| &entry.scope == scope && &entry.stamp != stamp,
            &mut retired,
        );
        let value = self
            .entries
            .iter()
            .position(|entry| &entry.scope == scope && &entry.stamp == stamp)
            .map(|index| {
                let entry = self
                    .entries
                    .remove(index)
                    .expect("qualified prepared entry");
                self.bytes -= entry.bytes;
                self.sql_bytes -= entry.sql_bytes;
                entry.value
            });
        (value, retired)
    }
    pub(super) fn insert(
        &mut self,
        scope: Scope,
        stamp: HistorySyncStamp,
        bytes: usize,
        value: T,
        now: Instant,
    ) -> Vec<T> {
        self.insert_range(scope, stamp, bytes, 0, value, now)
    }
    /// RAM is a hard admission bound. SQL work is disk-backed and has a soft
    /// idle quota: one oversized plan may remain so bounded turns can progress.
    pub(super) fn insert_range(
        &mut self,
        scope: Scope,
        stamp: HistorySyncStamp,
        bytes: usize,
        sql_bytes: usize,
        value: T,
        now: Instant,
    ) -> Vec<T> {
        let mut retired = Vec::new();
        self.extract(|entry| entry.scope == scope, &mut retired);
        if !Self::accepts_bytes(bytes) {
            retired.push(value);
            return retired;
        }
        while self.entries.len() >= MAX_ENTRIES
            || self.bytes > MAX_BYTES - bytes
            || (sql_bytes > 0
                && !self.entries.is_empty()
                && self.sql_bytes.saturating_add(sql_bytes) > MAX_SQL_BYTES)
        {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(index, entry)| (entry.returned, *index))
                .map(|(index, _)| index)
                .expect("nonempty bounded prepared cache");
            let entry = self.entries.remove(oldest).expect("oldest prepared entry");
            self.bytes -= entry.bytes;
            self.sql_bytes -= entry.sql_bytes;
            retired.push(entry.value);
        }
        self.bytes += bytes;
        self.sql_bytes += sql_bytes;
        self.entries.push_back(Entry {
            scope,
            stamp,
            bytes,
            sql_bytes,
            returned: {
                #[cfg(test)]
                {
                    now + self.test_elapsed
                }
                #[cfg(not(test))]
                {
                    now
                }
            },
            value,
        });
        retired
    }
    #[cfg(test)]
    pub(in crate::node) fn age_for_test(&mut self, elapsed: std::time::Duration) {
        // Advance a test clock rather than subtracting from a platform Instant:
        // freshly booted Windows runners cannot represent time before boot.
        // Age affects LRU capacity pressure, never version validity.
        self.test_elapsed += elapsed;
    }

    pub(super) fn clear(&mut self) -> Vec<T> {
        self.bytes = 0;
        self.sql_bytes = 0;
        std::mem::take(&mut self.entries)
            .into_iter()
            .map(|entry| entry.value)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope(tag: u8) -> Scope {
        Scope {
            peer: crate::identity::device::DeviceIdentity::generate().public(),
            purpose: Purpose::Ordinary,
            conversation: ConversationId::new([tag; 32]),
            inbound: false,
            pull_only: false,
            denied: false,
        }
    }
    fn stamp(tag: u8) -> HistorySyncStamp {
        HistorySyncStamp {
            fingerprint: [tag; 32],
        }
    }
    #[test]
    fn checkout_is_exclusive_and_qualifies_versions_and_modes_without_idle_expiry() {
        let mut cache = PreparedCache::default();
        let key = scope(1);
        let now = Instant::now();
        assert!(cache.insert(key.clone(), stamp(1), 1, 7, now).is_empty());
        let mut other = key.clone();
        other.purpose = Purpose::History;
        assert!(cache.take(&other, &stamp(1)).0.is_none());
        other = key.clone();
        other.pull_only = true;
        assert!(cache.take(&other, &stamp(1)).0.is_none());
        assert_eq!(cache.take(&key, &stamp(1)), (Some(7), vec![]));
        assert_eq!(cache.take(&key, &stamp(1)), (None, vec![]));
        cache.insert(key.clone(), stamp(1), 1, 8, now);
        assert_eq!(cache.take(&key, &stamp(2)), (None, vec![8]));
        cache.insert(key.clone(), stamp(2), 1, 9, now);
        cache.age_for_test(std::time::Duration::from_secs(60 * 60));
        cache.insert(scope(2), stamp(1), 1, 10, now);
        assert_eq!(cache.take(&key, &stamp(2)), (Some(9), vec![]));
        assert_eq!(cache.clear(), vec![10]);
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn entry_byte_and_duplicate_limits_return_every_retired_value() {
        let mut cache = PreparedCache::default();
        let now = Instant::now();
        for index in 0..MAX_ENTRIES {
            let mut scope = scope(index as u8);
            if index % 2 != 0 {
                scope.purpose = Purpose::History;
            }
            assert!(cache.insert(scope, stamp(1), 1, index, now).is_empty());
        }
        assert_eq!(
            cache.insert(scope(100), stamp(1), 1, MAX_ENTRIES, now),
            vec![0]
        );
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(cache.clear().len(), MAX_ENTRIES);
        let key = scope(1);
        let mut history = key.clone();
        history.purpose = Purpose::History;
        cache.insert(history, stamp(1), MAX_BYTES, 1, now);
        assert_eq!(cache.insert(scope(2), stamp(1), 1, 2, now), vec![1]);
        assert!(cache.insert(key.clone(), stamp(1), 1, 3, now).is_empty());
        assert_eq!(cache.insert(key.clone(), stamp(2), 1, 4, now), vec![3]);
        assert_eq!(
            cache.insert(key, stamp(3), MAX_BYTES + 1, 5, now),
            vec![4, 5]
        );
        assert_eq!(cache.clear(), vec![2]);
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn pressure_retirement_is_oldest_first_and_updates_byte_accounting() {
        let mut cache = PreparedCache::default();
        let now = Instant::now();
        let second = scope(2);
        cache.insert(scope(1), stamp(1), 3, 11, now);
        cache.insert(
            second.clone(),
            stamp(1),
            7,
            22,
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!(cache.retire_oldest(), Some(11));
        assert_eq!(cache.bytes, 7);
        assert_eq!(cache.take(&second, &stamp(1)), (Some(22), vec![]));
        assert_eq!(cache.bytes, 0);
        assert_eq!(cache.retire_oldest(), None);
    }
}

#[cfg(test)]
mod range_budget_tests {
    use super::*;
    fn key(n: u8) -> Scope {
        Scope {
            peer: crate::identity::device::DeviceIdentity::generate().public(),
            purpose: Purpose::Ordinary,
            conversation: ConversationId::new([n; 32]),
            inbound: false,
            pull_only: false,
            denied: false,
        }
    }
    fn stamp() -> HistorySyncStamp {
        HistorySyncStamp {
            fingerprint: [1; 32],
        }
    }
    #[test]
    fn range_disk_soft_quota_retires_oldest_but_preserves_one_oversized_plan() {
        let mut cache = PreparedCache::default();
        let now = Instant::now();
        let a = key(1);
        let b = key(2);
        let huge = key(3);
        assert!(
            cache
                .insert_range(a.clone(), stamp(), 64 * 1024, MAX_SQL_BYTES / 2, 1, now)
                .is_empty()
        );
        assert!(
            cache
                .insert_range(b.clone(), stamp(), 64 * 1024, MAX_SQL_BYTES / 2, 2, now)
                .is_empty()
        );
        assert_eq!(cache.sql_bytes, MAX_SQL_BYTES);
        assert_eq!(
            cache.insert_range(huge.clone(), stamp(), 64 * 1024, MAX_SQL_BYTES + 1, 3, now),
            vec![1, 2]
        );
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.bytes, 64 * 1024);
        assert_eq!(cache.sql_bytes, MAX_SQL_BYTES + 1);
        assert_eq!(cache.take(&huge, &stamp()).0, Some(3));
        assert_eq!(cache.bytes, 0);
        assert_eq!(cache.sql_bytes, 0);
        assert_eq!(
            cache.insert_range(a, stamp(), MAX_BYTES + 1, 1, 4, now),
            vec![4]
        );
        assert_eq!(cache.sql_bytes, 0);
    }
    #[test]
    fn range_charge_counters_follow_every_retirement_and_checkout_path() {
        let mut cache = PreparedCache::default();
        let now = Instant::now();
        let a = key(1);
        let b = key(2);
        cache.insert_range(a.clone(), stamp(), 10, 20, 1, now);
        cache.insert_range(b.clone(), stamp(), 30, 40, 2, now);
        assert_eq!(cache.retire_oldest(), Some(1));
        assert_eq!((cache.bytes, cache.sql_bytes), (30, 40));
        assert_eq!(cache.take_candidate(&b), Some(2));
        assert_eq!((cache.bytes, cache.sql_bytes), (0, 0));
        cache.insert_range(a.clone(), stamp(), 10, 20, 3, now);
        let different = HistorySyncStamp {
            fingerprint: [2; 32],
        };
        assert_eq!(cache.take(&a, &different), (None, vec![3]));
        assert_eq!((cache.bytes, cache.sql_bytes), (0, 0));
        cache.insert_range(a, stamp(), 10, 20, 4, now);
        assert_eq!(cache.clear(), vec![4]);
        assert_eq!((cache.bytes, cache.sql_bytes), (0, 0));
    }
}

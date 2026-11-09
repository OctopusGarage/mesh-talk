//! Node-owned, version-qualified derived plans. Values are checked out exclusively;
//! admission returns retired values so SQL destruction never runs under its mutex.
use super::network::HistorySyncStamp;
use crate::{eventlog::ConversationId, identity::device::PublicIdentity};
use std::{collections::VecDeque, time::Instant};

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq)]
pub(in crate::node) struct Scope {
    pub(super) peer: PublicIdentity,
    pub(super) conversation: ConversationId,
    pub(super) inbound: bool,
    pub(super) pull_only: bool,
    pub(super) denied: bool,
}
struct Entry<T> {
    scope: Scope,
    stamp: HistorySyncStamp,
    bytes: usize,
    returned: Instant,
    value: T,
}
pub(in crate::node) struct PreparedCache<T> {
    entries: VecDeque<Entry<T>>,
    bytes: usize,
    #[cfg(test)]
    test_elapsed: std::time::Duration,
}
impl<T> Default for PreparedCache<T> {
    fn default() -> Self {
        Self {
            entries: VecDeque::new(),
            bytes: 0,
            #[cfg(test)]
            test_elapsed: std::time::Duration::ZERO,
        }
    }
}
impl<T> PreparedCache<T> {
    pub(super) fn accepts_bytes(bytes: usize) -> bool {
        bytes <= MAX_BYTES
    }
    fn extract(&mut self, mut remove: impl FnMut(&Entry<T>) -> bool, retired: &mut Vec<T>) {
        let mut index = 0;
        while index < self.entries.len() {
            if remove(&self.entries[index]) {
                let entry = self.entries.remove(index).expect("bounded prepared entry");
                self.bytes -= entry.bytes;
                retired.push(entry.value);
            } else {
                index += 1;
            }
        }
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
        let mut retired = Vec::new();
        self.extract(|entry| entry.scope == scope, &mut retired);
        if bytes > MAX_BYTES {
            retired.push(value);
            return retired;
        }
        while self.entries.len() >= MAX_ENTRIES || self.bytes > MAX_BYTES - bytes {
            let oldest = self
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(index, entry)| (entry.returned, *index))
                .map(|(index, _)| index)
                .expect("nonempty bounded prepared cache");
            let entry = self.entries.remove(oldest).expect("oldest prepared entry");
            self.bytes -= entry.bytes;
            retired.push(entry.value);
        }
        self.bytes += bytes;
        self.entries.push_back(Entry {
            scope,
            stamp,
            bytes,
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
            assert!(
                cache
                    .insert(scope(index as u8), stamp(1), 1, index, now)
                    .is_empty()
            );
        }
        assert_eq!(
            cache.insert(scope(100), stamp(1), 1, MAX_ENTRIES, now),
            vec![0]
        );
        assert_eq!(cache.entries.len(), MAX_ENTRIES);
        assert_eq!(cache.clear().len(), MAX_ENTRIES);
        let key = scope(1);
        cache.insert(key.clone(), stamp(1), MAX_BYTES, 1, now);
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
}

//! Small completed-work evidence survives eviction of heavy mailbox owners.
//! A hit schedules work only; receipt authentication and live scope checks remain mandatory.
use super::Repair;
use bincode::Options;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 64;
const MAX_KEY_BYTES: usize = 256;
const REPAIR_INTERVAL: Duration = Duration::from_secs(300);
struct Settled {
    key: Vec<u8>,
    generation: u64,
    fingerprint: [u8; 32],
    checked: Instant,
}
#[derive(Default)]
pub(super) struct QuietRepairCache(VecDeque<Settled>);
fn fingerprint(repair: &Repair) -> Option<[u8; 32]> {
    if repair.active || repair.page.is_some() || repair.spent.iter().all(|n| *n == 0) {
        return None;
    }
    let bytes = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(1024)
        .serialize(repair)
        .ok()?;
    let mut hash = Sha256::new();
    hash.update(b"MeshTalkSettledRepairWorkV1\0");
    hash.update(bytes);
    Some(hash.finalize().into())
}
impl QuietRepairCache {
    pub(super) fn clear(&mut self) {
        self.0.clear();
    }
    pub(super) fn contains(
        &self,
        key: &[u8],
        generation: u64,
        repair: &Repair,
        now: Instant,
    ) -> bool {
        let Some(fingerprint) = fingerprint(repair) else {
            return false;
        };
        self.0.iter().any(|entry| {
            entry.key == key
                && entry.generation == generation
                && entry.fingerprint == fingerprint
                && now.saturating_duration_since(entry.checked) < REPAIR_INTERVAL
        })
    }
    pub(super) fn remember(&mut self, key: &[u8], generation: u64, repair: &Repair, now: Instant) {
        if key.is_empty() || key.len() > MAX_KEY_BYTES {
            return;
        }
        let Some(fingerprint) = fingerprint(repair) else {
            return;
        };
        self.0
            .retain(|entry| now.saturating_duration_since(entry.checked) < REPAIR_INTERVAL);
        if self.0.iter().any(|entry| {
            entry.key == key && entry.generation == generation && entry.fingerprint == fingerprint
        }) {
            // Identical observation cannot postpone the original sparse repair.
            return;
        }
        self.0.retain(|entry| entry.key != key);
        if self.0.len() == MAX_ENTRIES {
            self.0.pop_front();
        }
        self.0.push_back(Settled {
            key: key.to_vec(),
            generation,
            fingerprint,
            checked: now,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::{ConversationId, EventId},
        postoffice::MailboxCursor,
    };
    fn repair() -> Repair {
        let conversation = ConversationId::new([1; 32]);
        let captured = MailboxCursor {
            store_uuid: [2; 16],
            conversation,
            epoch: 3,
            first_seen: Some(4),
            revision: 5,
            after: 0,
            window: Some((6, 5)),
        };
        Repair {
            captured: captured.clone(),
            witness: [7; 32],
            active: false,
            scan: captured,
            page: None,
            rewind: false,
            focus: None,
            conversation,
            local_epoch: 0,
            global_epoch: None,
            owner: EventId::new([8; 32]),
            spent: [1; 32],
            scan_complete: true,
        }
    }
    #[test]
    fn exact_completed_work_never_covers_changed_inputs_or_active_work() {
        let now = Instant::now();
        let original = repair();
        let mut cache = QuietRepairCache::default();
        cache.remember(b"scope", 9, &original, now);
        assert!(cache.contains(b"scope", 9, &original, now));
        assert!(!cache.contains(b"other", 9, &original, now));
        assert!(!cache.contains(b"scope", 10, &original, now));
        for variant in 0..11 {
            let mut changed = original.clone();
            match variant {
                0 => changed.active = true,
                1 => changed.captured.store_uuid[0] ^= 1,
                2 => changed.captured.epoch += 1,
                3 => changed.captured.window = Some((7, 5)),
                4 => changed.witness[0] ^= 1,
                5 => changed.local_epoch += 1,
                6 => changed.global_epoch = Some(1),
                7 => changed.owner = EventId::new([9; 32]),
                8 => changed.spent[0] ^= 2,
                9 => changed.conversation = ConversationId::new([9; 32]),
                _ => changed.scan.after += 1,
            }
            assert!(
                !cache.contains(b"scope", 9, &changed, now),
                "changed input {variant}"
            );
        }
        assert!(cache.contains(b"scope", 9, &original, now));
        cache.clear();
        assert!(!cache.contains(b"scope", 9, &original, now));
    }
    #[test]
    fn repeated_observation_keeps_original_repair_deadline() {
        let now = Instant::now();
        let repair = repair();
        let mut cache = QuietRepairCache::default();
        cache.remember(b"scope", 0, &repair, now);
        let later = now + REPAIR_INTERVAL - Duration::from_secs(1);
        assert!(cache.contains(b"scope", 0, &repair, later));
        cache.remember(b"scope", 0, &repair, later);
        assert!(!cache.contains(b"scope", 0, &repair, now + REPAIR_INTERVAL));
        cache.remember(b"scope", 0, &repair, now + REPAIR_INTERVAL);
        assert!(cache.contains(b"scope", 0, &repair, now + REPAIR_INTERVAL));
    }
    #[test]
    fn scope_capacity_and_fingerprint_size_are_bounded() {
        let now = Instant::now();
        let repair = repair();
        let mut cache = QuietRepairCache::default();
        for n in 0..=MAX_ENTRIES {
            cache.remember(&(n as u16).to_be_bytes(), 0, &repair, now);
        }
        assert_eq!(cache.0.len(), MAX_ENTRIES);
        assert!(!cache.contains(&0u16.to_be_bytes(), 0, &repair, now));
        assert!(cache.contains(&(MAX_ENTRIES as u16).to_be_bytes(), 0, &repair, now));
        cache.remember(&[0; MAX_KEY_BYTES + 1], 0, &repair, now);
        assert_eq!(cache.0.len(), MAX_ENTRIES);
        let mut active = repair.clone();
        active.active = true;
        cache.remember(b"active", 0, &active, now);
        assert!(!cache.contains(b"active", 0, &active, now));
        assert_eq!(cache.0.len(), MAX_ENTRIES);
    }
}

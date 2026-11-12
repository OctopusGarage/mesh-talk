//! Runtime work hints for confirmed controls or qualified no-control outcomes. These skip
//! repeated preparation, never grant permission or certify new custody.
use super::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const MAX_QUIET_CONSENTS: usize = 256;
const MAX_SCOPE_BYTES: usize = 512;
const QUIET_CONSENT_TTL: Duration = Duration::from_secs(300);
type Offer = (ConversationId, EventId);

#[derive(Clone)]
pub(super) struct ControlScope {
    key: Vec<u8>,
    source: [u8; 32],
    work_stamp: crate::storage::database::WorkStamp,
    remote: RemoteBinding,
    floor: (i64, i64),
}
impl ControlScope {
    fn accepts_remote_floor(&self, current: &Self) -> bool {
        // The upper is the relay domain's commit cut, not this mailbox's
        // revision. Unrelated appends may advance it without changing this mailbox.
        self.floor.0 >= 0
            && self.floor.0 <= current.floor.0
            && self.floor.1 >= 0
            && self.floor.1 == current.floor.1
    }

    fn permits_reuse(&self, current: &Self) -> bool {
        self.key == current.key
            && self.source == current.source
            && self.work_stamp.unchanged_since(&current.work_stamp)
            && self.remote == current.remote
            && self.accepts_remote_floor(current)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuietConsentKind {
    Confirmed,
    NoControlDue,
}

struct QuietConsent {
    kind: QuietConsentKind,
    scope: ControlScope,
    offer: Offer,
    checked: Instant,
    #[cfg(test)]
    extra_age: Duration,
}

impl QuietConsent {
    fn elapsed_at(&self, now: Instant) -> Duration {
        let elapsed = now.saturating_duration_since(self.checked);
        #[cfg(test)]
        let elapsed = elapsed.saturating_add(self.extra_age);
        elapsed
    }
}

#[derive(Default)]
pub(super) struct QuietConsentCache(VecDeque<QuietConsent>);

impl QuietConsentCache {
    pub(super) fn len(&self) -> usize {
        self.0.len()
    }
    fn observed_reason(&self, scope: &ControlScope, offer: &Offer, now: Instant) -> usize {
        let Some(hint) = self
            .0
            .iter()
            .find(|hint| hint.scope.key == scope.key && hint.offer == *offer)
        else {
            return reason::CONSENT_ABSENT;
        };
        if !hint.scope.work_stamp.unchanged_since(&scope.work_stamp) {
            reason::CONSENT_MAIN_CHANGED
        } else if hint.scope.source != scope.source {
            reason::CONSENT_POLICY_CHANGED
        } else if hint.scope.remote != scope.remote {
            reason::CONSENT_REMOTE_CHANGED
        } else if !hint.scope.accepts_remote_floor(scope) {
            reason::CONSENT_FLOOR_CHANGED
        } else if hint.elapsed_at(now) >= QUIET_CONSENT_TTL {
            reason::CONSENT_EXPIRED
        } else {
            reason::CONSENT_HIT
        }
    }
    fn contains(&self, scope: &ControlScope, offer: &Offer, now: Instant) -> bool {
        self.0.iter().any(|hint| {
            hint.scope.permits_reuse(scope)
                && hint.offer == *offer
                && hint.elapsed_at(now) < QUIET_CONSENT_TTL
        })
    }

    fn remember(&mut self, scope: ControlScope, offer: Offer, now: Instant) -> bool {
        self.remember_outcome(scope, offer, now, QuietConsentKind::Confirmed)
    }

    fn remember_outcome(
        &mut self,
        scope: ControlScope,
        offer: Offer,
        now: Instant,
        kind: QuietConsentKind,
    ) -> bool {
        if scope.key.len() > MAX_SCOPE_BYTES {
            return false;
        }
        self.0
            .retain(|hint| hint.elapsed_at(now) < QUIET_CONSENT_TTL);
        // Negative scheduling knowledge must never consume a live positive's
        // slot, including a same-offer observation under a different scope.
        if kind == QuietConsentKind::NoControlDue
            && self.0.iter().any(|hint| {
                hint.kind == QuietConsentKind::Confirmed
                    && hint.scope.key == scope.key
                    && hint.offer == offer
            })
        {
            return false;
        }
        self.0
            .retain(|hint| !(hint.scope.key == scope.key && hint.offer == offer));
        if self.0.len() >= MAX_QUIET_CONSENTS {
            if let Some(index) = self
                .0
                .iter()
                .position(|hint| hint.kind == QuietConsentKind::NoControlDue)
            {
                self.0.remove(index);
            } else if kind == QuietConsentKind::Confirmed {
                self.0.pop_front();
            } else {
                return false;
            }
        }
        self.0.push_back(QuietConsent {
            kind,
            scope,
            offer,
            checked: now,
            #[cfg(test)]
            extra_age: Duration::ZERO,
        });
        true
    }
}

impl Node {
    #[cfg(test)]
    pub(in crate::node::opaque_relay) fn age_quiet_consent_hints_for_test(
        &self,
        elapsed: Duration,
    ) -> usize {
        let mut cache = self.opaque_upload_cache.lock().expect("upload cache");
        for hint in &mut cache.quiet_consents.0 {
            hint.extra_age = hint.extra_age.saturating_add(elapsed);
        }
        cache.quiet_consents.len()
    }

    pub(super) fn consent_control_scope(
        &self,
        p: &OpaqueMailboxProgress,
    ) -> Result<Option<ControlScope>, LogError> {
        let Some(database) = self.storage.participant_database() else {
            return Ok(None);
        };
        let Some(proof) = self
            .historical_author(&p.recipient.ed25519_pub)
            .filter(|proof| proof.public() == p.recipient)
        else {
            return Ok(None);
        };
        let (Some(remote), Some(floor)) = (state(p).remote.clone(), state(p).remote_floor) else {
            return Ok(None);
        };
        // Runtime-only owner evidence excludes guarded disposable selection work.
        // Default writes and unknown observations retain normal preparation.
        let Some(work_stamp) = database.work_hint_stamp()? else {
            return Ok(None);
        };
        let generation = self.privacy.generation.load(Ordering::Acquire);
        let projection = self.privacy.projection_revision.load(Ordering::Acquire);
        let bytes = bincode::serialize(&(
            generation,
            projection,
            &proof.account_cert,
            self.device_sync_policy_snapshot().1,
            self.delivery_suspended.load(Ordering::Acquire),
            self.group_file_sharing_status().paused,
        ))
        .map_err(|error| LogError::Serialization(error.to_string()))?;
        if !database
            .work_hint_stamp()?
            .is_some_and(|current| work_stamp.unchanged_since(&current))
            || generation != self.privacy.generation.load(Ordering::Acquire)
            || projection != self.privacy.projection_revision.load(Ordering::Acquire)
        {
            return Ok(None);
        }
        Ok(Some(ControlScope {
            key: state(p).key.clone(),
            source: Sha256::digest(bytes).into(),
            work_stamp,
            remote,
            floor,
        }))
    }

    pub(in crate::node::opaque_relay) fn unsettled_consent_offers(
        &self,
        p: &OpaqueMailboxProgress,
    ) -> Vec<Offer> {
        let offers = p.consent_offers.as_deref().unwrap_or(&[]);
        let Mode::Active(active) = &p.incremental else {
            return offers.to_vec();
        };
        let Some(scope) = active.control_scope.as_ref() else {
            return offers.to_vec();
        };
        let cache = self.opaque_upload_cache.lock().expect("upload cache");
        let now = Instant::now();
        if self.order_work_observation.is_some() {
            let classified: Vec<_> = offers
                .iter()
                .map(|offer| {
                    (
                        *offer,
                        cache.quiet_consents.observed_reason(scope, offer, now),
                    )
                })
                .collect();
            drop(cache);
            return classified
                .into_iter()
                .filter_map(|(offer, class)| {
                    self.observe_mailbox_work(class, None);
                    (class != reason::CONSENT_HIT).then_some(offer)
                })
                .collect();
        }
        offers
            .iter()
            .filter(|offer| !cache.quiet_consents.contains(scope, offer, now))
            .copied()
            .collect()
    }

    pub(in crate::node::opaque_relay) fn remember_no_control_due(
        &self,
        p: &OpaqueMailboxProgress,
        offers: &[(ConversationId, EventId)],
    ) -> Result<(), LogError> {
        if offers.is_empty() {
            return Ok(());
        }
        let Some(before) = state(p).control_scope.as_ref() else {
            return Ok(());
        };
        let after = self.consent_control_scope(p)?;
        if !after
            .as_ref()
            .is_some_and(|after| before.permits_reuse(after))
        {
            return Ok(());
        }
        let mut cache = self.opaque_upload_cache.lock().expect("upload cache");
        let now = Instant::now();
        for offer in offers {
            cache.quiet_consents.remember_outcome(
                before.clone(),
                *offer,
                now,
                QuietConsentKind::NoControlDue,
            );
        }
        Ok(())
    }

    pub(super) fn remember_quiet_consent(
        &self,
        p: &OpaqueMailboxProgress,
        selected: &SelectedOpaqueControl,
    ) -> Result<(), LogError> {
        let SelectedOpaqueControlKind::Consent(file, manifest) = &selected.kind else {
            return Ok(());
        };
        let Some(before) = state(p).control_scope.as_ref() else {
            return Ok(());
        };
        let after = self.consent_control_scope(p)?;
        if after
            .as_ref()
            .is_some_and(|after| before.permits_reuse(after))
        {
            // The caller already obtained actual durable HAVE/upload success.
            // No database work or authority callbacks run under this guard.
            self.opaque_upload_cache
                .lock()
                .expect("upload cache")
                .quiet_consents
                .remember(before.clone(), (*file, *manifest), Instant::now());
            self.observe_mailbox_work(reason::CONSENT_LEARNED, None);
        } else {
            self.observe_mailbox_work(reason::CONSENT_PREPARE_MOVED, None);
            if let Some(after) = after.as_ref() {
                let class = if !before.work_stamp.unchanged_since(&after.work_stamp) {
                    reason::CONSENT_MAIN_CHANGED
                } else if before.source != after.source {
                    reason::CONSENT_POLICY_CHANGED
                } else if before.remote != after.remote {
                    reason::CONSENT_REMOTE_CHANGED
                } else {
                    reason::CONSENT_FLOOR_CHANGED
                };
                self.observe_mailbox_work(class, None);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> ControlScope {
        ControlScope {
            key: vec![1; 128],
            source: [2; 32],
            work_stamp: Default::default(),
            remote: RemoteBinding {
                store_uuid: [3; 16],
                conversation: ConversationId::new([4; 32]),
                epoch: 1,
                first_seen: Some(5),
            },
            floor: (6, 7),
        }
    }

    #[test]
    fn quiet_consent_requires_exact_source_scope_and_remote_incarnation() {
        let mut cache = QuietConsentCache::default();
        let scope = scope();
        let offer = (ConversationId::new([8; 32]), EventId::new([9; 32]));
        let now = Instant::now();
        cache.remember(scope.clone(), offer, now);
        assert!(cache.contains(&scope, &offer, now));
        for changed in [
            ControlScope {
                key: vec![0],
                ..scope.clone()
            },
            ControlScope {
                source: [0; 32],
                ..scope.clone()
            },
            ControlScope {
                remote: RemoteBinding {
                    epoch: 2,
                    ..scope.remote.clone()
                },
                ..scope.clone()
            },
            ControlScope {
                remote: RemoteBinding {
                    store_uuid: [0; 16],
                    ..scope.remote.clone()
                },
                ..scope.clone()
            },
            ControlScope {
                floor: (7, 8),
                ..scope.clone()
            },
        ] {
            assert!(!cache.contains(&changed, &offer, now));
        }
        assert!(!cache.contains(&scope, &(offer.0, EventId::new([0; 32])), now));
        assert!(cache.contains(&scope, &offer, now + Duration::from_secs(59)));
        assert!(
            !cache.contains(&scope, &offer, now + QUIET_CONSENT_TTL),
            "ordinary reads cannot renew the external-state fallback deadline"
        );
    }

    #[test]
    fn no_control_due_pressure_preserves_live_confirmations() {
        let mut cache = QuietConsentCache::default();
        let scope = scope();
        let now = Instant::now();
        let offer = |index: usize| {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(index as u64).to_le_bytes());
            (ConversationId::new([8; 32]), EventId::new(id))
        };
        for index in 0..MAX_QUIET_CONSENTS {
            assert!(cache.remember(scope.clone(), offer(index), now));
        }
        assert!(!cache.remember_outcome(
            scope.clone(),
            offer(MAX_QUIET_CONSENTS),
            now,
            QuietConsentKind::NoControlDue
        ));
        assert!(!cache.remember_outcome(
            scope.clone(),
            offer(0),
            now,
            QuietConsentKind::NoControlDue
        ));
        assert_eq!(cache.len(), MAX_QUIET_CONSENTS);
        for index in 0..MAX_QUIET_CONSENTS {
            assert!(cache.contains(&scope, &offer(index), now));
            assert!(
                cache
                    .0
                    .iter()
                    .any(|hint| hint.offer == offer(index)
                        && hint.kind == QuietConsentKind::Confirmed)
            );
        }
        let mut cache = QuietConsentCache::default();
        for index in 0..MAX_QUIET_CONSENTS - 1 {
            assert!(cache.remember(scope.clone(), offer(index), now));
        }
        let negative = offer(MAX_QUIET_CONSENTS);
        assert!(cache.remember_outcome(
            scope.clone(),
            negative,
            now,
            QuietConsentKind::NoControlDue
        ));
        let new_positive = offer(MAX_QUIET_CONSENTS + 1);
        assert!(cache.remember(scope.clone(), new_positive, now));
        assert!(
            !cache.contains(&scope, &negative, now),
            "eviction reenables normal preparation"
        );
        assert!(cache.contains(&scope, &new_positive, now));
        for index in 0..MAX_QUIET_CONSENTS - 1 {
            assert!(
                cache.contains(&scope, &offer(index), now),
                "positive admission must evict negative scheduling knowledge first"
            );
        }
    }

    #[test]
    fn no_control_due_scope_changes_and_expiry_reenable_preparation_without_renewal() {
        let mut cache = QuietConsentCache::default();
        let scope = scope();
        let offer = (ConversationId::new([8; 32]), EventId::new([9; 32]));
        let now = Instant::now();
        assert!(cache.remember_outcome(scope.clone(), offer, now, QuietConsentKind::NoControlDue));
        for seconds in [1, 30, 59] {
            assert!(cache.contains(&scope, &offer, now + Duration::from_secs(seconds)));
        }
        assert!(!cache.contains(&scope, &offer, now + QUIET_CONSENT_TTL));
        for changed in [
            ControlScope {
                source: [0; 32],
                ..scope.clone()
            },
            ControlScope {
                key: vec![0],
                ..scope.clone()
            },
            ControlScope {
                floor: (7, 8),
                ..scope.clone()
            },
            ControlScope {
                remote: RemoteBinding {
                    epoch: 2,
                    ..scope.remote.clone()
                },
                ..scope.clone()
            },
        ] {
            assert!(!cache.contains(&changed, &offer, now));
        }
    }

    #[test]
    fn quiet_consent_accepts_only_monotonic_global_upper_with_exact_mailbox_revision() {
        for kind in [QuietConsentKind::Confirmed, QuietConsentKind::NoControlDue] {
            let mut cache = QuietConsentCache::default();
            let prior = scope();
            let offer = (ConversationId::new([8; 32]), EventId::new([9; 32]));
            let now = Instant::now();
            assert!(cache.remember_outcome(prior.clone(), offer, now, kind));
            let advanced = ControlScope {
                floor: (prior.floor.0 + 100, prior.floor.1),
                ..prior.clone()
            };
            assert!(cache.contains(&advanced, &offer, now));
            assert_eq!(
                cache.observed_reason(&advanced, &offer, now),
                reason::CONSENT_HIT,
                "trace-enabled and ordinary scheduling must make the same decision"
            );
            for changed in [
                ControlScope {
                    floor: (prior.floor.0 - 1, prior.floor.1),
                    ..prior.clone()
                },
                ControlScope {
                    floor: (prior.floor.0 + 1, prior.floor.1 + 1),
                    ..prior.clone()
                },
                ControlScope {
                    floor: (prior.floor.0, prior.floor.1 - 1),
                    ..prior.clone()
                },
                ControlScope {
                    remote: RemoteBinding {
                        epoch: prior.remote.epoch + 1,
                        ..prior.remote.clone()
                    },
                    ..advanced.clone()
                },
                ControlScope {
                    remote: RemoteBinding {
                        store_uuid: [0; 16],
                        ..prior.remote.clone()
                    },
                    ..advanced.clone()
                },
                ControlScope {
                    remote: RemoteBinding {
                        first_seen: Some(99),
                        ..prior.remote.clone()
                    },
                    ..advanced.clone()
                },
                ControlScope {
                    remote: RemoteBinding {
                        conversation: ConversationId::new([0; 32]),
                        ..prior.remote.clone()
                    },
                    ..advanced.clone()
                },
                ControlScope {
                    source: [0; 32],
                    ..advanced.clone()
                },
                ControlScope {
                    key: vec![0],
                    ..advanced.clone()
                },
            ] {
                assert!(!cache.contains(&changed, &offer, now));
                assert_ne!(
                    cache.observed_reason(&changed, &offer, now),
                    reason::CONSENT_HIT
                );
            }
            assert!(cache.contains(&advanced, &offer, now + Duration::from_secs(59)));
            assert!(
                !cache.contains(&advanced, &offer, now + QUIET_CONSENT_TTL),
                "upper advances must not renew the original recovery deadline"
            );
            assert_eq!(
                cache.observed_reason(&advanced, &offer, now + QUIET_CONSENT_TTL),
                reason::CONSENT_EXPIRED
            );
        }
    }

    #[test]
    fn quiet_consent_metadata_is_bounded_and_eviction_only_reenables_work() {
        let mut cache = QuietConsentCache::default();
        let scope = scope();
        let now = Instant::now();
        let offer = (ConversationId::new([8; 32]), EventId::new([9; 32]));
        cache.remember(scope.clone(), offer, now);
        for index in 0..MAX_QUIET_CONSENTS {
            let mut id = [0; 32];
            id[..8].copy_from_slice(&index.to_le_bytes());
            cache.remember(scope.clone(), (offer.0, EventId::new(id)), now);
        }
        assert_eq!(cache.0.len(), MAX_QUIET_CONSENTS);
        assert!(!cache.contains(&scope, &offer, now));
        cache.remember(
            ControlScope {
                key: vec![0; MAX_SCOPE_BYTES + 1],
                ..scope
            },
            offer,
            now,
        );
        assert_eq!(cache.0.len(), MAX_QUIET_CONSENTS);
    }
}

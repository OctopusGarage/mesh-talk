//! Runtime work hints for individually confirmed consent offers. These skip
//! repeated preparation, never grant permission or certify new custody.
use super::*;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const MAX_QUIET_CONSENTS: usize = 256;
const MAX_SCOPE_BYTES: usize = 512;
const QUIET_CONSENT_TTL: Duration = Duration::from_secs(60);
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
    fn same_inputs(&self, current: &Self) -> bool {
        self.key == current.key
            && self.source == current.source
            && self.work_stamp.unchanged_since(&current.work_stamp)
            && self.remote == current.remote
            && self.floor == current.floor
    }
}

struct QuietConsent {
    scope: ControlScope,
    offer: Offer,
    checked: Instant,
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
        } else if hint.scope.floor != scope.floor {
            reason::CONSENT_FLOOR_CHANGED
        } else if now.saturating_duration_since(hint.checked) >= QUIET_CONSENT_TTL {
            reason::CONSENT_EXPIRED
        } else {
            reason::CONSENT_HIT
        }
    }
    fn contains(&self, scope: &ControlScope, offer: &Offer, now: Instant) -> bool {
        self.0.iter().any(|hint| {
            hint.scope.same_inputs(scope)
                && hint.offer == *offer
                && now.saturating_duration_since(hint.checked) < QUIET_CONSENT_TTL
        })
    }

    fn remember(&mut self, scope: ControlScope, offer: Offer, now: Instant) {
        if scope.key.len() > MAX_SCOPE_BYTES {
            return;
        }
        self.0.retain(|hint| {
            now.saturating_duration_since(hint.checked) < QUIET_CONSENT_TTL
                && !(hint.scope.key == scope.key && hint.offer == offer)
        });
        if self.0.len() >= MAX_QUIET_CONSENTS {
            self.0.pop_front();
        }
        self.0.push_back(QuietConsent {
            scope,
            offer,
            checked: now,
        });
    }
}

impl Node {
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
            .is_some_and(|after| before.same_inputs(after))
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

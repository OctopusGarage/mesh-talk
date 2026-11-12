//! Consent-gated group attachments over the existing recipient-encrypted mailboxes.
#[cfg(test)]
use super::file_consent::Decision;
use super::{Node, ReceiveDecision, delivery_store::FileCard};
use crate::{
    discovery::Announce,
    eventlog::{ConversationId, Event, EventId, EventKind, LogError},
    identity::device::PublicIdentity,
    postoffice::opaque::RelayMailbox,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, atomic::Ordering},
};

type UploadKey = (EventId, [u8; 32]);
const MAX_GROUP_FILE_JOBS: usize = 10_000;
const MAX_AUTHORIZATION_VIEWS: usize = 4;
type AuthorizationKey = ([u8; 32], [u8; 32], [u8; 32]);

struct AuthorizationView {
    upload: super::privacy_sync::HistoryUpload,
    stamp: Option<super::privacy_sync::HistorySyncStamp>,
    ready: bool,
    used: std::time::Instant,
}

#[derive(Clone, Copy)]
enum AuthorizationProbe {
    Allowed,
    Denied,
    Preparing,
}
impl AuthorizationProbe {
    fn ready(self) -> Result<bool, LogError> {
        match self {
            Self::Allowed => Ok(true),
            Self::Denied => Ok(false),
            Self::Preparing => Err(LogError::StaleCursor),
        }
    }
}
#[derive(Default)]
struct AuthorizationAdvance {
    progressed: bool,
    completed: Option<AuthorizationKey>,
}

#[derive(Clone)]
struct GroupFileUpload {
    channel: ConversationId,
    waiting_dependency: bool,
    file: ConversationId,
    manifest: EventId,
    recipient: PublicIdentity,
    retry_after: Option<std::time::Instant>,
    retry_delay: std::time::Duration,
    // Exact receiver action captured by this dispatch, not current custody.
    demand: Option<EventId>,
    // Bounded by the existing job count. Cloned retry selections share only
    // exact local outgoing-membership evidence, never remote custody proof.
    authorization_stamp: Arc<Mutex<Option<super::privacy_sync::HistorySyncStamp>>>,
}

// Owned only by one active retry future, never by queued/cooling jobs.
#[derive(Clone)]
struct IntrinsicOffer {
    event: Event,
    row: super::received_log::ReceivedEntry,
    rewrite_revision: u64,
    file: ConversationId,
}
type AttemptOffer = Mutex<Option<Arc<IntrinsicOffer>>>;

#[derive(Clone, Copy, Default)]
struct GroupFileScan {
    cursor: Option<i64>,
    source: Option<(u64, u64)>,
    repair_after: Option<std::time::Instant>,
    // Retained overflow recovery; only the serialized producer consumes it.
    rescan_requested: bool,
}

#[derive(Default)]
pub(super) struct GroupFileDelivery {
    gate: tokio::sync::Mutex<()>,
    scan: Mutex<GroupFileScan>,
    cursor: Mutex<Option<UploadKey>>,
    jobs: Mutex<BTreeMap<UploadKey, GroupFileUpload>>,
    authorization: Mutex<BTreeMap<AuthorizationKey, AuthorizationView>>,
}

impl Node {
    /// Authority is bound to this exact retained offer, never merely a shared group.
    pub(super) fn group_file_offer(
        &self,
        file: ConversationId,
        manifest: EventId,
        peer: &PublicIdentity,
    ) -> Result<Option<(Event, super::received_log::ReceivedEntry)>, LogError> {
        Ok(self
            .group_file_offer_with_attempt(file, manifest, peer, None)?
            .map(|offer| {
                let offer = Arc::unwrap_or_clone(offer);
                (offer.event, offer.row)
            }))
    }

    fn group_file_offer_with_attempt(
        &self,
        file: ConversationId,
        manifest: EventId,
        peer: &PublicIdentity,
        attempt: Option<&AttemptOffer>,
    ) -> Result<Option<Arc<IntrinsicOffer>>, LogError> {
        if self.delivery_suspended.load(Ordering::Acquire) {
            return Ok(None);
        }
        let cached = attempt.and_then(|slot| slot.lock().expect("attempt offer lock").clone());
        let offer = if let Some(cached) = cached.filter(|cached| {
            cached.event.id == manifest
                && self
                    .received_files
                    .lock()
                    .expect("file rows lock")
                    .rewrite_revision()
                    == cached.rewrite_revision
        }) {
            // Retained existence remains live authority even when immutable
            // ciphertext, integrity and signature work were already validated.
            let present = self
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .header(&manifest)?;
            if !present.is_some_and(|header| {
                header.kind == EventKind::FileManifest
                    && header.conversation == cached.event.conversation_id
                    && header.author == cached.event.author
            }) {
                return Ok(None);
            }
            cached
        } else {
            let (row, rewrite_revision) = {
                let rows = self.received_files.lock().expect("file rows lock");
                (rows.entry(manifest)?, rows.rewrite_revision())
            };
            let Some(row) = row else {
                return Ok(None);
            };
            if super::files::validated_manifest(&row.plaintext)
                .is_none_or(|decoded| decoded.file_conv() != file)
                || self.recipient_expired(&row.plaintext, manifest, row.wall_clock)
            {
                return Ok(None);
            }
            let (event, authenticated_read) = {
                let log = self.log.lock().expect("log lock");
                (log.get(&manifest)?, log.repository().is_ok())
            };
            let Some(event) = event else {
                return Ok(None);
            };
            if event.kind != EventKind::FileManifest
                || event.conversation_id != row.conversation
                || event.author.user_id() != row.from
                // Indexed reads authenticate the exact body against its header.
                // Legacy replay trusts encrypted records, so preserve its explicit checks.
                || (!authenticated_read
                    && (!event.verify_integrity() || !event.verify_signature()))
            {
                return Ok(None);
            }
            let offer = Arc::new(IntrinsicOffer {
                event,
                row,
                rewrite_revision,
                file,
            });
            if let Some(slot) = attempt {
                *slot.lock().expect("attempt offer lock") = Some(offer.clone());
            }
            offer
        };
        let event = &offer.event;
        let row = &offer.row;
        if offer.file != file || self.recipient_expired(&row.plaintext, manifest, row.wall_clock) {
            return Ok(None);
        }
        if event.author.ed25519_pub() != &self.identity.public().ed25519_pub
            && event.author.ed25519_pub() != &peer.ed25519_pub
        {
            return Ok(None);
        }
        {
            let store = self.delivery.lock().expect("delivery lock");
            if store.file_erased(row.conversation, manifest)
                || store.manifest_event_erased(manifest)
            {
                return Ok(None);
            }
        }
        if self
            .account_suppression
            .lock()
            .expect("suppression lock")
            .contains(row.conversation, manifest)?
            || self
                .recalls
                .lock()
                .expect("recalls lock")
                .contains(row.conversation, manifest)?
            || !self.channel_content_visible(row.conversation, manifest)?
        {
            return Ok(None);
        }
        let Some(proof) = self.historical_author(&peer.ed25519_pub) else {
            return Ok(None);
        };
        if proof.public() != *peer {
            return Ok(None);
        }
        {
            // The checkpoint validates the exact signed canonical generation;
            // display membership may still precede an unapplied leave/transfer.
            let log = self.log.lock().expect("log lock");
            let Some(state) = self.channel_checkpoints.current_state(row.conversation)? else {
                return Ok(None);
            };
            let projection = self.account_group_projection_with_log(&log, row.conversation)?;
            if !self.channel_identity_is_current_in(&state, &projection)
                || !state.is_accepted_member(&peer.user_id())
                || !state.members().contains(peer)
                || (state.owner_account_id().is_some()
                    && state.member_account_id(&peer.user_id()) != proof.account_id().as_deref())
            {
                return Ok(None);
            }
        }
        Ok(Some(offer))
    }

    pub(super) fn group_mailbox_peers(&self) -> Vec<Announce> {
        let selected = (|| -> Result<Vec<Announce>, LogError> {
            let members = {
                let book = self.channels.lock().expect("channels lock");
                let mut members = Vec::new();
                for id in book.channel_ids() {
                    if let Some(state) = book.state(&id)
                        && self.channel_identity_is_current(state)?
                    {
                        members.extend(state.recipient_members());
                    }
                }
                members
            };
            let mut peers = Vec::new();
            for peer in members {
                if peer != self.identity.public()
                    && self.group_member(&peer)?
                    && let Some(proof) = self
                        .historical_author(&peer.ed25519_pub)
                        .filter(|proof| proof.public() == peer)
                {
                    peers.push(proof);
                }
            }
            Ok(peers)
        })();
        match selected {
            Ok(peers) => peers,
            Err(error) => {
                log::warn!("Group mailbox discovery deferred after storage failure: {error}");
                Vec::new()
            }
        }
    }

    /// Reserve the same verified-save evidence as private files, without a DM receipt.
    pub(super) fn retain_group_file_completion(
        &self,
        event: &Event,
        row: &super::received_log::ReceivedEntry,
    ) -> Result<(), LogError> {
        let manifest = super::files::validated_manifest(&row.plaintext)
            .ok_or_else(|| LogError::Serialization("invalid group file manifest".into()))?;
        let Some(proof) = self.historical_author(event.author.ed25519_pub()) else {
            // Signed group history can be opened without observing its author's
            // discovery proof. Keep the card; reserve completion when proof arrives.
            return Ok(());
        };
        let file = FileCard::incoming(
            event,
            row,
            &manifest,
            super::delivery_store::FileCompletionBinding {
                source: proof.public(),
                certificate: proof.account_cert,
                owner_account: self.account_id(),
            },
        );
        self.delivery
            .lock()
            .expect("delivery lock")
            .retain_incoming_group_file(file)
    }

    /// Rebuild bounded work from the durable group rows, including older sends.
    fn refill_group_file_delivery(&self) -> Result<(), LogError> {
        let mut scan = {
            let mut scan = self
                .group_file_delivery
                .scan
                .lock()
                .expect("group file scan lock");
            if scan.rescan_requested {
                scan.cursor = None;
                scan.source = None;
                scan.repair_after = None;
                scan.rescan_requested = false;
            }
            *scan
        };
        let page = {
            let rows = self.received_files.lock().expect("file rows lock");
            let source = (rows.revision(), rows.rewrite_revision());
            if scan.source.is_none_or(|old| old.1 != source.1)
                || (scan.repair_after.is_some() && scan.source != Some(source))
            {
                scan.cursor = None;
                scan.repair_after = None;
            }
            scan.source = Some(source);
            if scan
                .repair_after
                .is_some_and(|deadline| std::time::Instant::now() < deadline)
            {
                return Ok(());
            }
            // A full queue retains its discovery page; it must not certify EOF
            // or skip a recipient which could not yet be admitted.
            if self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock")
                .len()
                >= MAX_GROUP_FILE_JOBS
            {
                return Ok(());
            }
            rows.scan_page(
                None,
                scan.cursor,
                super::transcript_repository::TranscriptPageLimit::default(),
            )?
        };
        for row in page.rows {
            if row.from == self.user_id()
                && let Some(manifest) = super::files::validated_manifest(&row.plaintext)
            {
                self.queue_group_file_delivery(manifest.file_conv(), row.event_id)?;
                if self
                    .group_file_delivery
                    .jobs
                    .lock()
                    .expect("group file jobs lock")
                    .len()
                    >= MAX_GROUP_FILE_JOBS
                {
                    return Ok(());
                }
            }
        }
        scan.cursor = page.next;
        scan.repair_after = page
            .next
            .is_none()
            .then(|| std::time::Instant::now() + std::time::Duration::from_secs(60));
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            "group_file_refill_before_publish".into()
        });
        let mut current = self
            .group_file_delivery
            .scan
            .lock()
            .expect("group file scan lock");
        // A request rejected by a full active queue may arrive during the SQL
        // scan. Never overwrite its retained rescan with an older EOF.
        if !current.rescan_requested {
            *current = scan;
        }
        Ok(())
    }

    pub(super) fn queue_group_file_delivery(
        &self,
        file: ConversationId,
        manifest: EventId,
    ) -> Result<(), LogError> {
        let original = self.log.lock().expect("log lock").get(&manifest)?;
        let Some(original) = original.filter(|event| {
            event.kind == EventKind::FileManifest
                && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
        }) else {
            return Ok(());
        };
        let members = {
            let book = self.channels.lock().expect("channels lock");
            let Some(state) = book.state(&original.conversation_id) else {
                return Ok(());
            };
            if !self.channel_identity_is_current(state)? {
                return Ok(());
            }
            state.recipient_members()
        };
        for recipient in members
            .into_iter()
            .filter(|peer| *peer != self.identity.public())
        {
            let key = (manifest, recipient.ed25519_pub);
            {
                let jobs = self
                    .group_file_delivery
                    .jobs
                    .lock()
                    .expect("group file jobs lock");
                // Existing work revalidates current entitlement in the rotating
                // retry owner. Refill must not restart its prepared prefix.
                if jobs.contains_key(&key) {
                    continue;
                }
                if jobs.len() >= MAX_GROUP_FILE_JOBS {
                    break;
                }
            }
            let demand = self.group_file_dispatch_demand(file, manifest, &recipient)?;
            if self
                .files
                .lock()
                .expect("files lock")
                .group_custody_settled(file, manifest, &recipient, demand)?
            {
                continue;
            }
            let job = GroupFileUpload {
                channel: original.conversation_id,
                waiting_dependency: false,
                file,
                manifest,
                recipient,
                retry_after: None,
                retry_delay: std::time::Duration::from_secs(3),
                demand,
                authorization_stamp: Arc::new(Mutex::new(None)),
            };
            if !self.group_file_custody_allowed(&job)? {
                continue;
            }
            let mut jobs = self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock");
            if jobs.len() < MAX_GROUP_FILE_JOBS && !jobs.contains_key(&key) {
                jobs.insert(key, job);
                // A later recipient may still be pending. Publish this wake
                // at insertion instead of losing it on that early return.
                self.group_file_notify.notify_one();
            }
        }
        Ok(())
    }

    fn group_file_dispatch_demand(
        &self,
        file: ConversationId,
        manifest: EventId,
        recipient: &PublicIdentity,
    ) -> Result<Option<EventId>, LogError> {
        Ok(self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_decision_snapshot_for(manifest, recipient)?
            .filter(|(_, decision)| {
                decision.file == file
                    && decision.sender == self.identity.public()
                    && decision.decision == ReceiveDecision::Accepted
                    && decision.completion.is_none()
            })
            .map(|(id, _)| id))
    }

    /// A committed, authenticated receiver request is retained without entering
    /// file/history preparation or the network from the receiving callback.
    pub(super) fn request_group_file_custody(
        &self,
        channel: ConversationId,
        file: ConversationId,
        manifest: EventId,
        recipient: &PublicIdentity,
        demand: EventId,
    ) {
        let key = (manifest, recipient.ed25519_pub);
        let mut jobs = self
            .group_file_delivery
            .jobs
            .lock()
            .expect("group file jobs lock");
        if jobs.contains_key(&key) || jobs.len() < MAX_GROUP_FILE_JOBS {
            jobs.insert(
                key,
                GroupFileUpload {
                    channel,
                    waiting_dependency: false,
                    file,
                    manifest,
                    recipient: recipient.clone(),
                    retry_after: None,
                    retry_delay: std::time::Duration::from_secs(3),
                    demand: Some(demand),
                    authorization_stamp: Arc::new(Mutex::new(None)),
                },
            );
        } else {
            // The signed request is already durable. A retained scan wake is
            // sufficient recovery when bounded active admission is exhausted.
            drop(jobs);
            self.group_file_delivery
                .scan
                .lock()
                .expect("group file scan lock")
                .rescan_requested = true;
        }
        self.group_file_notify.notify_one();
    }

    fn retire_group_file_dispatch(&self, key: &UploadKey, job: &GroupFileUpload) -> bool {
        let mut jobs = self
            .group_file_delivery
            .jobs
            .lock()
            .expect("group file jobs lock");
        if jobs.get(key).is_some_and(|current| {
            current.file == job.file
                && current.recipient == job.recipient
                && current.demand == job.demand
        }) {
            jobs.remove(key);
            true
        } else {
            false
        }
    }

    pub(super) fn group_file_work_pending(&self) -> bool {
        let (due, count) = {
            let jobs = self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock");
            (
                jobs.values().any(|job| {
                    job.retry_after
                        .is_none_or(|deadline| std::time::Instant::now() >= deadline)
                }),
                jobs.len(),
            )
        };
        let scanning = count < MAX_GROUP_FILE_JOBS
            && self
                .group_file_delivery
                .scan
                .lock()
                .expect("group file scan lock")
                .cursor
                .is_some();
        due || scanning
            || self
                .group_file_delivery
                .authorization
                .lock()
                .expect("group file authorization lock")
                .values()
                .any(|view| !view.ready)
    }

    /// The existing producer owns the timer; cooled work retains its wake deadline.
    pub(super) fn group_file_retry_delay(&self) -> Option<std::time::Duration> {
        let now = std::time::Instant::now();
        self.group_file_delivery
            .jobs
            .lock()
            .expect("group file jobs lock")
            .values()
            .filter_map(|job| job.retry_after)
            .filter(|deadline| *deadline > now)
            .map(|deadline| deadline.saturating_duration_since(now))
            .min()
    }

    fn group_file_custody_allowed(&self, job: &GroupFileUpload) -> Result<bool, LogError> {
        self.group_file_custody_with_attempt(job, None)
    }

    fn group_file_custody_with_attempt(
        &self,
        job: &GroupFileUpload,
        attempt: Option<&AttemptOffer>,
    ) -> Result<bool, LogError> {
        self.group_file_custody_probe(job, attempt)?.ready()
    }

    fn group_file_custody_probe(
        &self,
        job: &GroupFileUpload,
        attempt: Option<&AttemptOffer>,
    ) -> Result<AuthorizationProbe, LogError> {
        let Some(offer) =
            self.group_file_offer_with_attempt(job.file, job.manifest, &job.recipient, attempt)?
        else {
            return Ok(AuthorizationProbe::Denied);
        };
        let original = &offer.event;
        if original.author.ed25519_pub() != &self.identity.public().ed25519_pub
            || self
                .file_progress(job.file)
                .map_err(|error| match error {
                    super::NodeError::Log(error) => error,
                    other => LogError::Serialization(other.to_string()),
                })?
                .is_none_or(|p| p.done != p.total)
        {
            return Ok(AuthorizationProbe::Denied);
        }
        // Custody of ciphertext may precede approval. An explicit refusal or
        // verified recipient completion stops further deposits for this device.
        if !self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_decision_for(job.manifest, &job.recipient)?
            .is_none_or(|decision| {
                decision.file == job.file
                    && decision.sender == self.identity.public()
                    && decision.recipient == job.recipient
                    && decision.decision != ReceiveDecision::Declined
                    && decision.completion.is_none()
            })
        {
            return Ok(AuthorizationProbe::Denied);
        }
        // A new member's grant cannot release a pre-invitation file. Reuse the
        // existing recipient projection for its exact signed manifest.
        let before = self
            .sync_store(&job.recipient)
            .lock()
            .expect("projection lock")
            .history_sync_stamp(&original.conversation_id)?;
        let qualified = before.as_ref().is_some_and(|stamp| {
            job.authorization_stamp
                .lock()
                .expect("group file qualification lock")
                .as_ref()
                == Some(stamp)
        });
        if !qualified {
            // Immutable job fields bind this slot to one exact manifest/file
            // and full recipient. A missing or changed version is never a hit.
            *job.authorization_stamp
                .lock()
                .expect("group file qualification lock") = None;
            match self.group_file_outgoing_probe(
                original.conversation_id,
                job.manifest,
                &job.recipient,
            )? {
                AuthorizationProbe::Allowed => {}
                other => return Ok(other),
            }
            let after = self
                .sync_store(&job.recipient)
                .lock()
                .expect("projection lock")
                .history_sync_stamp(&original.conversation_id)?;
            if before.is_some() && before == after {
                *job.authorization_stamp
                    .lock()
                    .expect("group file qualification lock") = before;
            }
        }
        Ok(AuthorizationProbe::Allowed)
    }

    #[cfg(test)]
    fn group_file_outgoing_contains(
        &self,
        channel: ConversationId,
        event: EventId,
        peer: &PublicIdentity,
    ) -> Result<bool, LogError> {
        self.group_file_outgoing_probe(channel, event, peer)?
            .ready()
    }

    fn group_file_outgoing_probe(
        &self,
        channel: ConversationId,
        event: EventId,
        peer: &PublicIdentity,
    ) -> Result<AuthorizationProbe, LogError> {
        let key = (*channel.as_bytes(), peer.ed25519_pub, peer.x25519_pub);
        // This owner holds only four leased SQL selections, never historical
        // event IDs or a cached positive grant. No guard crosses network work.
        let mut views = self
            .group_file_delivery
            .authorization
            .lock()
            .expect("group file authorization lock");
        // Every path takes the cache before constructing any scope's store.
        // A missing fifth scope must not discard another scope's unfinished
        // finite preparation. Spend one turn on that owner, then yield.
        if !views.contains_key(&key) && views.len() >= MAX_AUTHORIZATION_VIEWS {
            let oldest_ready = views
                .iter()
                .filter(|(_, view)| view.ready)
                .min_by_key(|(_, view)| view.used)
                .map(|(key, _)| *key);
            if let Some(oldest) = oldest_ready {
                views.remove(&oldest);
            } else {
                let advanced = self.advance_group_file_authorization_on(&mut views)?;
                drop(views);
                if let Some(key) = advanced.completed {
                    self.wake_group_file_dependency(
                        ConversationId::new(key.0),
                        Some((key.1, key.2)),
                    );
                }
                return if advanced.progressed {
                    Ok(AuthorizationProbe::Preparing)
                } else {
                    Err(LogError::StaleCursor)
                };
            }
        }
        let owner = self.sync_store(peer);
        let store = owner.lock().expect("projection lock");
        let stamp = store.history_sync_stamp(&channel)?;
        if let Some(view) = views.get(&key) {
            match store.validate_history_upload(&view.upload) {
                Ok(()) => {}
                Err(LogError::StaleCursor | LogError::ConversationRetiring) => {
                    views.remove(&key);
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(view) = views.get_mut(&key)
            && view.ready
            && (stamp.is_none() || view.stamp != stamp)
        {
            // Ordinary appends extend the existing finite prepared prefix;
            // changed authority must reopen through the same selection owner.
            if store.resume_history_upload(&mut view.upload)? {
                view.stamp = stamp.clone();
                view.ready = false;
            } else {
                views.remove(&key);
            }
        }
        if let std::collections::btree_map::Entry::Vacant(entry) = views.entry(key) {
            entry.insert(AuthorizationView {
                upload: store.open_history_upload(&channel)?,
                stamp,
                ready: false,
                used: std::time::Instant::now(),
            });
            // Once per admitted cold view, not once per Pending turn: this
            // wakes an idle worker without creating a self-notification loop.
            self.group_file_notify.notify_one();
        }
        let view = views.get_mut(&key).expect("installed authorization view");
        view.used = std::time::Instant::now();
        let before = view.upload.preparation_progress();
        let was_ready = view.ready;
        let step = store.prepare_history_upload_step(
            &view.upload,
            crate::eventlog::sync::network::PreparationBudget::default(),
        );
        let step = match step {
            Ok(step) => step,
            Err(error @ (LogError::StaleCursor | LogError::ConversationRetiring)) => {
                views.remove(&key);
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        view.ready = step.ready;
        if !step.ready {
            let progressed = step.rows != 0
                || step.edges != 0
                || step.bytes != 0
                || before != view.upload.preparation_progress();
            return if progressed {
                Ok(AuthorizationProbe::Preparing)
            } else {
                Err(LogError::StaleCursor)
            };
        }
        let allowed = store.history_upload_contains(&view.upload, event)?;
        // A concurrent append may lie beyond this finite prefix. Positive
        // answers retain the owner's current control fences; an absent event
        // cannot become a final denial until the source observation catches up.
        if !allowed {
            let current = store.history_sync_stamp(&channel)?;
            if current.is_none() || view.stamp != current {
                return Err(LogError::StaleCursor);
            }
        }
        drop(store);
        drop(owner);
        drop(views);
        if !was_ready {
            self.wake_group_file_dependency(channel, Some((peer.ed25519_pub, peer.x25519_pub)));
        }
        Ok(if allowed {
            AuthorizationProbe::Allowed
        } else {
            AuthorizationProbe::Denied
        })
    }

    /// Advance even orphaned preparation through its existing exact authority
    /// owner. The caller holds the cache first; no store or guard spans awaits.
    fn advance_group_file_authorization_on(
        &self,
        views: &mut BTreeMap<AuthorizationKey, AuthorizationView>,
    ) -> Result<AuthorizationAdvance, LogError> {
        let Some(key) = views
            .iter()
            .filter(|(_, view)| !view.ready)
            .min_by_key(|(_, view)| view.used)
            .map(|(key, _)| *key)
        else {
            return Ok(AuthorizationAdvance::default());
        };
        let peer = PublicIdentity {
            ed25519_pub: key.1,
            x25519_pub: key.2,
        };
        let owner = self.sync_store(&peer);
        let store = owner.lock().expect("projection lock");
        let view = views
            .get_mut(&key)
            .expect("selected pending authorization view");
        let before = view.upload.preparation_progress();
        let result = store.validate_history_upload(&view.upload).and_then(|()| {
            store.prepare_history_upload_step(
                &view.upload,
                crate::eventlog::sync::network::PreparationBudget::default(),
            )
        });
        match result {
            Ok(step) => {
                let progressed = step.rows != 0
                    || step.edges != 0
                    || step.bytes != 0
                    || before != view.upload.preparation_progress();
                view.ready = step.ready;
                view.used = std::time::Instant::now();
                Ok(AuthorizationAdvance {
                    progressed: progressed || step.ready,
                    completed: step.ready.then_some(key),
                })
            }
            Err(LogError::StaleCursor | LogError::ConversationRetiring) => {
                views.remove(&key);
                Ok(AuthorizationAdvance {
                    progressed: true,
                    completed: None,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Scheduling hint only. Publication owners call after releasing their
    /// source guards; the next attempt independently checks current authority.
    pub(in crate::node) fn wake_group_file_checkpoint(&self, channel: ConversationId) {
        self.wake_group_file_dependency(channel, None);
    }
    fn wake_group_file_dependency(
        &self,
        channel: ConversationId,
        peer: Option<([u8; 32], [u8; 32])>,
    ) {
        let changed = {
            let mut jobs = self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock");
            let mut changed = false;
            for job in jobs.values_mut() {
                if job.channel == channel
                    && job.waiting_dependency
                    && peer.is_none_or(|(ed, x)| {
                        job.recipient.ed25519_pub == ed && job.recipient.x25519_pub == x
                    })
                {
                    job.waiting_dependency = false;
                    job.retry_after = None;
                    changed = true;
                }
            }
            changed
        };
        if changed {
            self.group_file_notify.notify_one();
        }
    }

    fn group_file_custody_current(&self, job: &GroupFileUpload, offer: &AttemptOffer) -> bool {
        let current = match self.group_file_custody_with_attempt(job, Some(offer)) {
            Ok(current) => current,
            Err(LogError::StaleCursor) => false,
            Err(error) => {
                log::warn!("Group file custody deferred after storage failure: {error}");
                false
            }
        };
        #[cfg(test)]
        self.trace_account_client(&job.recipient, || {
            format!("group_file_custody_checked {current}")
        });
        current
    }

    /// One bounded pass; mailbox reconciliation resumes only missing signed chunks.
    pub(super) async fn retry_group_file_delivery(&self) {
        use futures_util::stream::{self, StreamExt};
        use std::ops::Bound::{Excluded, Unbounded};
        let Ok(_work) = self.group_file_delivery.gate.try_lock() else {
            return;
        };
        let preparation = {
            let mut views = self
                .group_file_delivery
                .authorization
                .lock()
                .expect("group file authorization lock");
            self.advance_group_file_authorization_on(&mut views)
        };
        match preparation {
            Ok(advanced) => {
                if let Some(key) = advanced.completed {
                    self.wake_group_file_dependency(
                        ConversationId::new(key.0),
                        Some((key.1, key.2)),
                    );
                }
            }
            Err(error) => {
                log::warn!("Group file authorization deferred after storage failure: {error}")
            }
        }
        // Pending refill must not prevent queued jobs from advancing the
        // same finite authorization owners and releasing admission slots.
        if let Err(error) = self.refill_group_file_delivery()
            && !matches!(error, LogError::StaleCursor)
        {
            log::warn!("Group file scan deferred after storage failure: {error}");
        }
        let after = *self
            .group_file_delivery
            .cursor
            .lock()
            .expect("group file cursor lock");
        let selected = {
            let jobs = self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock");
            let start = after.map_or(Unbounded, Excluded);
            jobs.range((start, Unbounded))
                .chain(
                    jobs.iter()
                        .take_while(|(key, _)| after.is_some_and(|after| **key <= after)),
                )
                .filter(|(_, job)| {
                    job.retry_after
                        .is_none_or(|after| std::time::Instant::now() >= after)
                })
                .take(2)
                .map(|(key, job)| (*key, job.clone()))
                .collect::<Vec<_>>()
        };
        // Advancing at launch gives canceled/stalled work a fair next turn.
        let _ = tokio::time::timeout(super::delivery_runtime::FILE_EXCHANGE_TIMEOUT, async {
            for (key, job) in selected {
                *self
                    .group_file_delivery
                    .cursor
                    .lock()
                    .expect("group file cursor lock") = Some(key);
                // Publish before any authorization/network await: cancellation,
                // unavailable relays and failures must retain bounded backoff.
                let retry_delay = {
                    let mut jobs = self.group_file_delivery.jobs.lock().expect("group file jobs lock");
                    let Some(pending) = jobs.get_mut(&key) else { continue; };
                    if pending.recipient != job.recipient || pending.file != job.file
                        || pending.demand != job.demand { continue; }
                    let delay = pending.retry_delay;
                    // Register before probing: a concurrent dependency publication
                    // can clear this deadline without losing its wake afterwards.
                    pending.waiting_dependency = true;
                    pending.retry_after = Some(std::time::Instant::now() + delay);
                    pending.retry_delay = (delay * 2).min(std::time::Duration::from_secs(60));
                    delay
                };
                let demand = match self.group_file_dispatch_demand(job.file, job.manifest, &job.recipient) {
                    Ok(demand) => demand,
                    Err(error) => { log::warn!("Group file demand observation deferred: {error}"); continue; }
                };
                {
                    let mut jobs = self.group_file_delivery.jobs.lock().expect("group file jobs lock");
                    let Some(pending) = jobs.get_mut(&key) else { continue; };
                    if pending.recipient != job.recipient || pending.file != job.file
                        || pending.demand != job.demand { continue; }
                    pending.demand = demand;
                }
                let job = GroupFileUpload { demand, ..job };
                let settled = self.files.lock().expect("files lock")
                    .group_custody_settled(job.file, job.manifest, &job.recipient, demand);
                match settled {
                    Ok(true) => { self.retire_group_file_dispatch(&key, &job); continue; }
                    Ok(false) => {}
                    Err(error) => { log::warn!("Group file dispatch observation deferred: {error}"); continue; }
                }
                #[cfg(test)]
                self.trace_account_client(&job.recipient, || "group_file_dependency_registered".into());
                let offer = AttemptOffer::new(None);
                let allowed = match self.group_file_custody_probe(&job, Some(&offer)) {
                    Ok(AuthorizationProbe::Allowed) => {
                        self.observe_mailbox_work(super::work_observation::reason::FILE_ALLOWED, None);
                        true
                    },
                    Ok(AuthorizationProbe::Denied) => false,
                    Ok(AuthorizationProbe::Preparing) => {
                        self.observe_mailbox_work(super::work_observation::reason::FILE_PREPARING, None);
                        if let Some(pending) = self.group_file_delivery.jobs.lock().expect("group file jobs lock").get_mut(&key)
                            .filter(|pending| pending.recipient == job.recipient && pending.file == job.file && pending.demand == job.demand) {
                            pending.waiting_dependency = false;
                            pending.retry_after = None;
                            pending.retry_delay = std::time::Duration::from_secs(3);
                        }
                        continue;
                    }
                    Err(LogError::StaleCursor) => {
                        self.observe_mailbox_work(super::work_observation::reason::FILE_WAITING, None);
                        continue;
                    }
                    Err(error) => { log::warn!("Group file retry deferred after storage failure: {error}"); continue; }
                };
                if let Some(pending) = self.group_file_delivery.jobs.lock().expect("group file jobs lock").get_mut(&key)
                    .filter(|pending| pending.recipient == job.recipient && pending.file == job.file && pending.demand == job.demand) {
                    pending.waiting_dependency = false;
                    // Readiness may have consumed the waiting deadline during
                    // the probe. Network work owns a fresh cancellation deadline.
                    if allowed {
                        pending.retry_after = Some(std::time::Instant::now() + retry_delay);
                    }
                }
                if !allowed {
                    self.retire_group_file_dispatch(&key, &job);
                    continue;
                }
                let candidates = {
                    let roster = self.roster.lock().expect("roster lock");
                    self.post_office_candidates_for(&roster, &job.recipient)
                };
                let candidates = self.relay_candidate_batch("group-file:relays", candidates);
                let held = std::sync::atomic::AtomicBool::new(false);
                stream::iter(candidates)
                    .for_each_concurrent(2, |relay| {
                        let job = &job;
                        let held = &held;
                        let offer = &offer;
                        async move {
                            let _ = tokio::time::timeout(
                                super::delivery_runtime::RELAY_EXCHANGE_TIMEOUT,
                                async {
                                    if !self.relay_send_allowed(&relay.public)
                                        || !self.group_file_custody_current(job, offer)
                                    {
                                        return;
                                    }
                                    let Ok(mut channel) =
                                        self.privacy_sync_dial(relay.addr, &relay.public).await
                                    else {
                                        return;
                                    };
                                    let mut mailboxes = vec![RelayMailbox::new(
                                        &self.identity,
                                        &job.recipient,
                                        &relay.public,
                                    )];
                                    if self.bridge_directory_allowed() {
                                        mailboxes.push(RelayMailbox::federated(
                                            &self.identity,
                                            &job.recipient,
                                        ));
                                    }
                                    for mailbox in mailboxes.into_iter().flatten() {
                                        let mailbox_id = mailbox.id();
                                        let federated = RelayMailbox::is_federated_id(&mailbox_id);
                                        let bridge = self.bridge_generation.load(Ordering::SeqCst);
                                        let mut progress =
                                            super::opaque_relay::OpaqueMailboxProgress::new_file_dispatch(
                                                &relay.public,
                                                &job.recipient,
                                                job.file,
                                                mailbox,
                                            );
                                        let current = || {
                                            self.group_file_custody_current(job, offer)
                                                && self.relay_send_allowed(&relay.public)
                                                && (!federated
                                                    || (self.bridge_directory_allowed()
                                                        && self.bridge_generation.load(Ordering::SeqCst) == bridge))
                                                && self
                                                    .roster
                                                    .lock()
                                                    .expect("roster lock")
                                                    .get(&relay.public.user_id())
                                                    .is_some_and(|peer| {
                                                        peer.post_office
                                                            && peer.public == relay.public
                                                    })
                                        };
                                        loop {
                                            match self
                                                .advance_opaque_mailbox_if(
                                                    &mut channel,
                                                    &mut progress,
                                                    &current,
                                                )
                                                .await
                                            {
                                                Ok(true) => break,
                                                Ok(false) => {}
                                                Err(_) => return,
                                            }
                                            if !current() {
                                                return;
                                            }
                                        }
                                        // Chunk sequences are one-based and this job is
                                        // restricted to our original signed file. Resolve
                                        // its final header without hydrating every chunk.
                                        let final_chunk = (|| -> Result<Option<EventId>, LogError> {
                                            let Some(retention) = self.files.lock().expect("files lock")
                                                .retention_for(job.file)? else { return Ok(None); };
                                            Ok(self.log.lock().expect("log lock").repository()?
                                                .authored_header(
                                                    &job.file,
                                                    crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub),
                                                    u64::from(retention.chunks),
                                                )?
                                                .filter(|header| header.kind == EventKind::Message)
                                                .map(|header| header.id))
                                        })();
                                        let final_chunk = match final_chunk {
                                            Ok(chunk) => chunk,
                                            Err(error) => { log::warn!("Group file completion deferred after storage failure: {error}"); return; }
                                        };
                                        let outer = if let Some(inner) = final_chunk {
                                            let resolved = self.resolve_opaque_confirmation(&mut progress, inner, &|| {
                                                current() && channel.io_admitted()
                                            }).await;
                                            if !current() || !channel.io_admitted() {
                                                return;
                                            }
                                            match resolved {
                                                Ok(outer) => outer,
                                                Err(_) => return,
                                            }
                                        } else {
                                            None
                                        };
                                        if let Some(outer) = outer
                                            && matches!(
                                                super::session::request_durable_have(
                                                    &mut channel,
                                                    mailbox_id,
                                                    outer
                                                )
                                                .await,
                                                Ok(true)
                                            )
                                            && current()
                                        {
                                            held.store(true, Ordering::Relaxed);
                                        }
                                    }
                                    self.recycle_sync_channel(channel);
                                },
                            )
                            .await;
                        }
                    })
                    .await;
                if held.load(Ordering::Relaxed) {
                    #[cfg(test)]
                    self.trace_account_client(&job.recipient, || "group_file_dispatch_before_settle".into());
                    let settled = self.files.lock().expect("files lock")
                        .settle_group_custody(job.file, job.manifest, &job.recipient, demand);
                    match settled {
                        Ok(true) => { self.retire_group_file_dispatch(&key, &job); continue; }
                        Ok(false) => {}
                        Err(error) => log::warn!("Group file dispatch settlement deferred: {error}"),
                    }
                }
                if let Some(pending) = self.group_file_delivery.jobs.lock()
                    .expect("group file jobs lock").get_mut(&key)
                    .filter(|pending| pending.file == job.file && pending.recipient == job.recipient
                        && pending.demand == job.demand)
                {
                    pending.retry_after = Some(std::time::Instant::now() + retry_delay);
                }
            }
        })
        .await;
    }
}

#[cfg(test)]
#[path = "group_file_authorization_tests.rs"]
mod authorization_tests;

//! Consent-gated group attachments over the existing recipient-encrypted mailboxes.
use super::{Node, ReceiveDecision, delivery_store::FileCard, file_consent::Decision};
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

#[derive(Clone)]
struct GroupFileUpload {
    file: ConversationId,
    manifest: EventId,
    recipient: PublicIdentity,
    retry_after: Option<std::time::Instant>,
    // Bounded by the existing job count. Cloned retry selections share only
    // exact local outgoing-membership evidence, never remote custody proof.
    authorization_stamp: Arc<Mutex<Option<super::privacy_sync::HistorySyncStamp>>>,
}

#[derive(Default)]
pub(super) struct GroupFileDelivery {
    gate: tokio::sync::Mutex<()>,
    scan_cursor: Mutex<Option<i64>>,
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
        if self.delivery_suspended.load(Ordering::Acquire) {
            return Ok(None);
        }
        let Some(row) = self
            .received_files
            .lock()
            .expect("file rows lock")
            .entry(manifest)?
        else {
            return Ok(None);
        };
        let Some(decoded) = super::files::validated_manifest(&row.plaintext) else {
            return Ok(None);
        };
        if decoded.file_conv() != file
            || self.recipient_expired(&row.plaintext, manifest, row.wall_clock)
        {
            return Ok(None);
        }
        let Some(event) = self.log.lock().expect("log lock").get(&manifest)? else {
            return Ok(None);
        };
        if event.kind != EventKind::FileManifest
            || event.conversation_id != row.conversation
            || event.author.user_id() != row.from
            || (event.author.ed25519_pub() != &self.identity.public().ed25519_pub
                && event.author.ed25519_pub() != &peer.ed25519_pub)
            || !event.verify_integrity()
            || !event.verify_signature()
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
        Ok(Some((event, row)))
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
        let after = *self
            .group_file_delivery
            .scan_cursor
            .lock()
            .expect("group file scan lock");
        let page = self
            .received_files
            .lock()
            .expect("file rows lock")
            .scan_page(
                None,
                after,
                super::transcript_repository::TranscriptPageLimit::default(),
            )?;
        for row in page.rows {
            if row.from == self.user_id()
                && let Some(manifest) = super::files::validated_manifest(&row.plaintext)
            {
                self.queue_group_file_delivery(manifest.file_conv(), row.event_id)?;
            }
        }
        *self
            .group_file_delivery
            .scan_cursor
            .lock()
            .expect("group file scan lock") = page.next;
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
            let job = GroupFileUpload {
                file,
                manifest,
                recipient,
                retry_after: None,
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
                self.file_pull_notify.notify_one();
            }
        }
        Ok(())
    }

    pub(super) fn group_file_work_pending(&self) -> bool {
        let jobs = !self
            .group_file_delivery
            .jobs
            .lock()
            .expect("group file jobs lock")
            .is_empty();
        jobs || self
            .group_file_delivery
            .authorization
            .lock()
            .expect("group file authorization lock")
            .values()
            .any(|view| !view.ready)
    }

    fn group_file_custody_allowed(&self, job: &GroupFileUpload) -> Result<bool, LogError> {
        let Some((original, _)) = self.group_file_offer(job.file, job.manifest, &job.recipient)?
        else {
            return Ok(false);
        };
        if original.author.ed25519_pub() != &self.identity.public().ed25519_pub
            || self
                .file_progress(job.file)
                .map_err(|error| match error {
                    super::NodeError::Log(error) => error,
                    other => LogError::Serialization(other.to_string()),
                })?
                .is_none_or(|p| p.done != p.total)
        {
            return Ok(false);
        }
        // Custody of ciphertext may precede approval. An explicit refusal or
        // verified recipient completion stops further deposits for this device.
        if !self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_control_for(job.manifest, &job.recipient)?
            .as_ref()
            .and_then(Decision::open)
            .is_none_or(|decision| {
                decision.file == job.file
                    && decision.sender == self.identity.public()
                    && decision.recipient == job.recipient
                    && decision.decision != ReceiveDecision::Declined
                    && decision.completion.is_none()
            })
        {
            return Ok(false);
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
            if !self.group_file_outgoing_contains(
                original.conversation_id,
                job.manifest,
                &job.recipient,
            )? {
                return Ok(false);
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
        Ok(true)
    }

    fn group_file_outgoing_contains(
        &self,
        channel: ConversationId,
        event: EventId,
        peer: &PublicIdentity,
    ) -> Result<bool, LogError> {
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
                self.advance_group_file_authorization_on(&mut views)?;
                return Err(LogError::StaleCursor);
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
            self.file_pull_notify.notify_one();
        }
        let view = views.get_mut(&key).expect("installed authorization view");
        view.used = std::time::Instant::now();
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
            return Err(LogError::StaleCursor);
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
        Ok(allowed)
    }

    /// Advance even orphaned preparation through its existing exact authority
    /// owner. The caller holds the cache first; no store or guard spans awaits.
    fn advance_group_file_authorization_on(
        &self,
        views: &mut BTreeMap<AuthorizationKey, AuthorizationView>,
    ) -> Result<(), LogError> {
        let Some(key) = views
            .iter()
            .filter(|(_, view)| !view.ready)
            .min_by_key(|(_, view)| view.used)
            .map(|(key, _)| *key)
        else {
            return Ok(());
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
        let result = store.validate_history_upload(&view.upload).and_then(|()| {
            store.prepare_history_upload_step(
                &view.upload,
                crate::eventlog::sync::network::PreparationBudget::default(),
            )
        });
        match result {
            Ok(step) => {
                view.ready = step.ready;
                view.used = std::time::Instant::now();
            }
            Err(LogError::StaleCursor | LogError::ConversationRetiring) => {
                views.remove(&key);
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn group_file_custody_current(&self, job: &GroupFileUpload) -> bool {
        match self.group_file_custody_allowed(job) {
            Ok(current) => current,
            Err(LogError::StaleCursor) => false,
            Err(error) => {
                log::warn!("Group file custody deferred after storage failure: {error}");
                false
            }
        }
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
        if let Err(error) = preparation {
            log::warn!("Group file authorization deferred after storage failure: {error}");
            return;
        }
        // Pending refill must not prevent queued jobs from advancing the
        // same finite authorization owners and releasing admission slots.
        if let Err(error) = self.refill_group_file_delivery()
            && !matches!(error, LogError::StaleCursor)
        {
            log::warn!("Group file scan deferred after storage failure: {error}");
            return;
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
                let allowed = match self.group_file_custody_allowed(&job) { Ok(allowed) => allowed, Err(LogError::StaleCursor) => continue, Err(error) => { log::warn!("Group file retry deferred after storage failure: {error}"); continue; } };
                if !allowed {
                    self.group_file_delivery
                        .jobs
                        .lock()
                        .expect("group file jobs lock")
                        .remove(&key);
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
                        async move {
                            let _ = tokio::time::timeout(
                                super::delivery_runtime::RELAY_EXCHANGE_TIMEOUT,
                                async {
                                    if !self.relay_send_allowed(&relay.public)
                                        || !self.group_file_custody_current(job)
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
                                            super::opaque_relay::OpaqueMailboxProgress::new(
                                                &relay.public,
                                                &job.recipient,
                                                Some(job.file),
                                                mailbox,
                                            );
                                        let current = || {
                                            self.group_file_custody_current(job)
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
                if held.load(Ordering::Relaxed)
                    && let Some(pending) = self
                        .group_file_delivery
                        .jobs
                        .lock()
                        .expect("group file jobs lock")
                        .get_mut(&key)
                {
                    pending.retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
                }
            }
        })
        .await;
    }
}

#[cfg(test)]
#[path = "group_file_authorization_tests.rs"]
mod authorization_tests;

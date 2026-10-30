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
    sync::{Mutex, atomic::Ordering},
};

type UploadKey = (EventId, [u8; 32]);
const MAX_GROUP_FILE_JOBS: usize = 10_000;

#[derive(Clone)]
struct GroupFileUpload {
    file: ConversationId,
    manifest: EventId,
    recipient: PublicIdentity,
    retry_after: Option<std::time::Instant>,
}

#[derive(Default)]
pub(super) struct GroupFileDelivery {
    gate: tokio::sync::Mutex<()>,
    scan_cursor: Mutex<Option<i64>>,
    cursor: Mutex<Option<UploadKey>>,
    jobs: Mutex<BTreeMap<UploadKey, GroupFileUpload>>,
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
            let book = self.channels.lock().expect("channels lock");
            let Some(state) = book.state(&row.conversation) else {
                return Ok(None);
            };
            if !self.channel_identity_is_current(state)?
                || !state.is_accepted_member(&peer.user_id())
                || !state.members().contains(peer)
                || (state.owner_account_id().is_some()
                    && state.member_account_id(&peer.user_id()) != proof.account_id().as_deref())
            {
                return Ok(None);
            }
        }
        // Check signed controls too: a removal can precede the live-book refresh.
        let log = self.log.lock().expect("log lock");
        let Some(members) = super::channel::ChannelBook::projected_members(
            row.conversation,
            &log.events(&row.conversation)?,
        ) else {
            return Ok(None);
        };
        Ok(
            (members.contains(peer) && members.contains(&self.identity.public()))
                .then_some((event, row)),
        )
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
                if peer != self.identity.public() && self.group_member(&peer)? {
                    if let Some(proof) = self
                        .historical_author(&peer.ed25519_pub)
                        .filter(|proof| proof.public() == peer)
                    {
                        peers.push(proof);
                    }
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
        let mut added = false;
        for recipient in members
            .into_iter()
            .filter(|peer| *peer != self.identity.public())
        {
            let job = GroupFileUpload {
                file,
                manifest,
                recipient,
                retry_after: None,
            };
            if !self.group_file_custody_allowed(&job)? {
                continue;
            }
            let mut jobs = self
                .group_file_delivery
                .jobs
                .lock()
                .expect("group file jobs lock");
            let key = (manifest, job.recipient.ed25519_pub);
            if jobs.len() < MAX_GROUP_FILE_JOBS && !jobs.contains_key(&key) {
                jobs.insert(key, job);
                added = true;
            }
        }
        if added {
            self.file_pull_notify.notify_one();
        }
        Ok(())
    }

    pub(super) fn group_file_work_pending(&self) -> bool {
        !self
            .group_file_delivery
            .jobs
            .lock()
            .expect("group file jobs lock")
            .is_empty()
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
        // A new member's grant cannot release a pre-invitation file. Reuse the
        // existing recipient projection for its exact signed manifest.
        if !self
            .sync_store(&job.recipient)
            .lock()
            .expect("projection lock")
            .authorized_event_ids(&original.conversation_id)?
            .contains(&job.manifest)
        {
            return Ok(false);
        }
        // Custody of ciphertext may precede approval. An explicit refusal or
        // verified recipient completion stops further deposits for this device.
        Ok(self
            .file_consent
            .lock()
            .expect("file consent lock")
            .received_control_for(job.manifest, &job.recipient)
            .and_then(Decision::open)
            .is_none_or(|decision| {
                decision.file == job.file
                    && decision.sender == self.identity.public()
                    && decision.recipient == job.recipient
                    && decision.decision != ReceiveDecision::Declined
                    && decision.completion.is_none()
            }))
    }

    fn group_file_custody_current(&self, job: &GroupFileUpload) -> bool {
        match self.group_file_custody_allowed(job) {
            Ok(current) => current,
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
        if let Err(error) = self.refill_group_file_delivery() {
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
                let allowed = match self.group_file_custody_allowed(&job) { Ok(allowed) => allowed, Err(error) => { log::warn!("Group file retry deferred after storage failure: {error}"); continue; } };
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
                                        let chunks = match self
                                            .log
                                            .lock()
                                            .expect("log lock")
                                            .events(&job.file) { Ok(chunks) => chunks, Err(error) => { log::warn!("Group file completion deferred after storage failure: {error}"); return; } };
                                        let final_chunk = chunks.last().map(|event| event.id);
                                        if let Some((_, outer)) = progress
                                            .uploaded()
                                            .iter()
                                            .find(|(inner, _)| Some(*inner) == final_chunk)
                                            && matches!(
                                                super::session::request_durable_have(
                                                    &mut channel,
                                                    mailbox_id,
                                                    *outer
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

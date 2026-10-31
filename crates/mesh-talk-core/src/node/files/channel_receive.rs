//! Channel file reception uses the existing receiver and FILES transaction owners.
use super::*;
use crate::node::{
    channel::checkpoint::{
        ChannelCheckpointStore, ChannelReceiveBudget, ChannelReceptionRole, FileReceivePreview,
        PreparedChannelReceive,
    },
    received_store::ReceivedStore,
    reception_effects::{ArrivalKind, ReceptionMetadata},
};

/// One producer continuation, independent of history length. Rejected speculative
/// rows remain in the existing disk pending owner until the first card commits.
pub(in crate::node) struct ChannelFileTurn {
    channel: ConversationId,
    catch_up: bool,
    reset: bool,
    preview: Option<(FileReceivePreview, crate::eventlog::Event)>,
    // Only the explicit legacy SDK retains one admitted manifest for its final
    // callback. Production hosts retain no payload here and pull durable facts.
    legacy_last: Option<(ReceivedEntry, ReceptionMetadata)>,
    retry_at: Option<std::time::Instant>,
    retry_delay_ms: u64,
}
impl ChannelFileTurn {
    fn new(channel: ConversationId, context: &str) -> Self {
        Self {
            channel,
            catch_up: context != "live",
            reset: true,
            preview: None,
            legacy_last: None,
            retry_at: None,
            retry_delay_ms: 0,
        }
    }
}

impl Node {
    pub(in crate::node) fn pending_channel_file(&self) -> Option<ConversationId> {
        self.pending_channel_files
            .lock()
            .expect("channel file turn lock")
            .as_ref()
            .map(|turn| turn.channel)
    }
    pub(in crate::node) fn process_channel_file_reception(
        &self,
        requested: ConversationId,
        context: &'static str,
    ) -> Result<(), NodeError> {
        let mut pending = self
            .pending_channel_files
            .lock()
            .expect("channel file turn lock");
        let turn = pending.get_or_insert_with(|| ChannelFileTurn::new(requested, context));
        if turn
            .retry_at
            .is_some_and(|deadline| std::time::Instant::now() < deadline)
        {
            return Ok(());
        }
        // Finish the sole retained continuation before starting another channel.
        // Its remaining ciphertext is durable work even if the caller changed.
        let result = self.channel_file_turn(turn);
        match result {
            Ok(true) => {
                *pending = None;
            }
            Ok(false) => {
                turn.retry_at = None;
                turn.retry_delay_ms = 0;
                self.file_pull_notify.notify_one();
            }
            Err(error) => {
                turn.preview = None;
                turn.reset = true;
                turn.catch_up = true;
                let delay = turn.retry_delay_ms.max(1000);
                turn.retry_at =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(delay));
                turn.retry_delay_ms = (delay * 2).min(30_000);
                self.file_pull_notify.notify_one();
                return Err(error);
            }
        }
        Ok(())
    }

    fn channel_file_turn(&self, turn: &mut ChannelFileTurn) -> Result<bool, NodeError> {
        let channel = turn.channel;
        {
            let received = self.received_files.lock().expect("file rows lock");
            received.check_publication()?;
            self.storage
                .database()
                .read(|c| received.check_reception_owner_on(c))?;
        }
        if !self.channel_checkpoints.project_step(
            self.log.lock().expect("log lock").repository()?,
            &self.identity,
            channel,
        )? {
            return Ok(false);
        }
        let mut budget = ChannelReceiveBudget::default();
        while budget.page() {
            if turn.reset {
                if self.channel_checkpoints.reset_file_preview(channel)? {
                    continue;
                }
                turn.reset = false;
            }
            if let Some((preview, event)) = turn.preview.as_ref()
                && !self.channel_file_retain(event, preview.first().plaintext(), None)?
            {
                let (preview, event) = turn.preview.take().expect("present preview");
                self.commit_channel_file(&preview.into_first(), &event, false, turn)?;
                turn.reset = true;
                continue;
            }
            let page = self
                .channel_checkpoints
                .pending_file_page(channel, turn.preview.as_ref().map(|p| &p.0))?;
            for id in page.ids {
                let header = self
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .header(&id)?
                    .ok_or(LogError::StaleCursor)?;
                if !budget.admit(header.stored_bytes)? {
                    return Ok(false);
                }
                let event = self
                    .log
                    .lock()
                    .expect("log lock")
                    .get(&id)?
                    .ok_or(LogError::StaleCursor)?;
                if let Some((preview, _)) = turn.preview.as_mut()
                    && preview.same_chain(&event)
                {
                    let Some(plaintext) = self
                        .channel_checkpoints
                        .preview_file_following(&event, preview)?
                    else {
                        continue;
                    };
                    let retain = self.channel_file_retain(
                        &event,
                        &plaintext,
                        Some(preview.first().plaintext()),
                    )?;
                    if retain
                        && !self
                            .received_files
                            .lock()
                            .expect("file rows lock")
                            .contains(id)?
                    {
                        turn.catch_up = true;
                        let (preview, event) = turn.preview.take().expect("present preview");
                        self.commit_channel_file(&preview.into_first(), &event, true, turn)?;
                        turn.reset = true;
                        break;
                    }
                    self.channel_checkpoints
                        .defer_file_preview(&event, preview)?;
                    continue;
                }
                // A different chain can be consumed independently, but a
                // second surfaced card establishes catch-up before either
                // visible card is published.
                let Some(candidate) = self.channel_checkpoints.prepare_receive(&event)? else {
                    continue;
                };
                let retain = self.channel_file_retain(
                    &event,
                    candidate.plaintext(),
                    turn.preview.as_ref().map(|p| p.0.first().plaintext()),
                )?;
                let new_card = retain
                    && !self
                        .received_files
                        .lock()
                        .expect("file rows lock")
                        .contains(id)?;
                if !new_card || turn.catch_up {
                    self.commit_channel_file(&candidate, &event, retain, turn)?;
                } else if turn.preview.is_some() {
                    turn.catch_up = true;
                    let (preview, first) = turn.preview.take().expect("present preview");
                    self.commit_channel_file(&preview.into_first(), &first, true, turn)?;
                    turn.reset = true;
                    break;
                } else {
                    turn.preview = Some((FileReceivePreview::new(candidate)?, event));
                }
            }
            if turn.reset {
                continue;
            }
            if !page.more_work {
                if let Some((preview, event)) = turn.preview.take() {
                    self.commit_channel_file(&preview.into_first(), &event, true, turn)?;
                    turn.reset = true;
                    continue;
                }
                if let Some((_, metadata)) = self.channel_checkpoints.finish_file_batch(channel)? {
                    self.note_reception_change();
                    if let Some((entry, _)) = turn.legacy_last.take()
                        && let Some(last) = self.received_file_for_reception(&entry, &metadata)?
                    {
                        let _ = self.file_incoming.send(last);
                    }
                }
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn channel_file_retain(
        &self,
        event: &crate::eventlog::Event,
        plaintext: &[u8],
        first: Option<&[u8]>,
    ) -> Result<bool, NodeError> {
        if self.recipient_expired(plaintext, event.id, event.wall_clock)
            || !self.channel_content_visible(event.conversation_id, event.id)?
            || self
                .account_suppression
                .lock()
                .expect("suppression lock")
                .contains(event.conversation_id, event.id)?
            || self
                .delivery
                .lock()
                .expect("delivery lock")
                .manifest_event_erased(event.id)
            || self.message_recalled_selected(event.conversation_id, event.id, None, None)?
        {
            return Ok(false);
        }
        let Some(manifest) = validated_manifest(plaintext) else {
            return Ok(false);
        };
        if let Some(first) = first.and_then(validated_manifest)
            && first.file_conv() == manifest.file_conv()
            && !same_file_transfer(&first, &manifest)
        {
            return Ok(false);
        }
        let files = self.files.lock().expect("file book lock");
        if files.processed(event.id)? {
            return Ok(false);
        }
        Ok(files.accepts(&manifest)?)
    }

    fn commit_channel_file(
        &self,
        candidate: &PreparedChannelReceive,
        event: &crate::eventlog::Event,
        retain: bool,
        turn: &mut ChannelFileTurn,
    ) -> Result<(), NodeError> {
        if event.kind != EventKind::FileManifest
            || event.id != candidate.event_id()
            || event.conversation_id != candidate.conversation()
            || event.wall_clock != candidate.wall_clock()
            || event.author.user_id() != candidate.author()
        {
            return Err(LogError::AdmissionRejected(
                "file source does not match prepared receiver".into(),
            )
            .into());
        }
        // Expiry, local erase and transfer eligibility can change while a
        // bounded preview yields. Recheck before reserving or retaining bytes.
        let retain = retain && self.channel_file_retain(event, candidate.plaintext(), None)?;
        let entry = ReceivedEntry {
            event_id: event.id,
            conversation: event.conversation_id,
            from: event.author.user_id(),
            wall_clock: event.wall_clock,
            plaintext: candidate.plaintext().to_vec(),
        };
        if retain {
            // Reserve the existing completion journal outside transcript/SQL
            // locks. A failure leaves the consuming receiver untouched.
            self.retain_group_file_completion(event, &entry)?;
        }
        let mut metadata = ReceptionMetadata::for_incoming(
            event,
            &entry,
            &self.identity.public(),
            &self.account_id(),
            super::super::node::now_millis(),
        );
        metadata.own_device = self
            .linked_devices()
            .iter()
            .any(|d| d.public.user_id() == entry.from);
        metadata.arrival_kind = if turn.catch_up {
            ArrivalKind::CatchUp
        } else {
            ArrivalKind::Live
        };
        let committed = {
            let mut received = self.received_files.lock().expect("file rows lock");
            received.check_write()?;
            let existing = received.contains(event.id)?;
            let installed = self.storage.database().transaction(|c| {
                received.check_reception_owner_on(c)?;
                if !ChannelCheckpointStore::install_receive_on(c, candidate)? {
                    return Ok(None);
                }
                if !retain {
                    ReceivedStore::install_evidence_on(c, entry.conversation, entry.event_id)?;
                    crate::node::filebook::indexed::mark_processed_on(c, entry.event_id)?;
                    return Ok(None);
                }
                if existing {
                    ReceivedStore::install_file_on(c, &entry, event, None)?;
                    return Ok(None);
                }
                let metadata = ChannelCheckpointStore::prepare_batch_on(
                    c,
                    candidate,
                    ChannelReceptionRole::File,
                    metadata,
                )?;
                if !ReceivedStore::install_file_on(c, &entry, event, Some(&metadata))? {
                    return Ok(None);
                }
                ChannelCheckpointStore::install_batch_on(
                    c,
                    candidate,
                    ChannelReceptionRole::File,
                    &metadata,
                )?;
                Ok(Some(metadata))
            })?;
            received.note_channel_commit(usize::from(installed.is_some()));
            installed
        };
        if retain {
            // Historical cards can become eligible after a late signed proof.
            // Refresh current file state even when no arrival fact is created.
            self.note_file_change();
        }
        let Some(metadata) = committed else {
            return Ok(());
        };
        self.note_reception_change();
        if !metadata.own_device {
            self.note_account_local_change();
        }
        let manifest = validated_manifest(&entry.plaintext).expect("validated retained manifest");
        self.remember_manifest_scope(
            manifest.file_conv(),
            entry.conversation,
            event.author,
            event.id,
        );
        self.file_pull_notify.notify_one();
        if self.file_incoming.is_legacy() {
            if metadata.batch_complete {
                if let Some(file) = self.received_file_for_reception(&entry, &metadata)? {
                    let _ = self.file_incoming.send(file);
                }
            } else if let Some((previous, metadata)) = turn.legacy_last.replace((entry, metadata))
                && let Some(file) = self.received_file_for_reception(&previous, &metadata)?
            {
                let _ = self.file_incoming.send(file);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        channel::{ChannelMeta, ChannelState},
        discovery::Roster,
        eventlog::{ConversationId, Event, EventId, EventKind},
        identity::device::DeviceIdentity,
        node::{Node, ReceivedFile},
    };
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct Fixture {
        _directory: tempfile::TempDir,
        receiver: Arc<Node>,
        incoming: mpsc::UnboundedReceiver<ReceivedFile>,
        channel: ConversationId,
        events: Vec<Event>,
    }
    impl Fixture {
        fn new(payloads: Vec<Vec<u8>>) -> Self {
            Self::with_delivery(payloads, false)
        }
        fn with_delivery(payloads: Vec<Vec<u8>>, bounded: bool) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let sender = DeviceIdentity::generate();
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, incoming) = mpsc::unbounded_channel();
            let identity = DeviceIdentity::generate();
            let receiver = if bounded {
                Node::open_bounded_with_profile_lease(
                    crate::storage::profile::ProfileStorageLease::acquire(directory.path())
                        .unwrap(),
                    crate::node::BoundedNodeOpenRequest {
                        account: crate::identity::account::Account::from_secret_bytes(
                            identity.secret_bytes().0,
                        ),
                        identity,
                        roster: Arc::new(Mutex::new(Roster::default())),
                        log_path: &directory.path().join("messages.log"),
                        sent_path: &directory.path().join("sent.log"),
                        password: "pw",
                    },
                )
            } else {
                Node::open(
                    identity,
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &directory.path().join("messages.log"),
                    &directory.path().join("sent.log"),
                    "pw",
                )
            }
            .unwrap();
            let channel = ConversationId::new([201; 32]);
            let meta = ChannelMeta {
                name: "Files".into(),
                members: vec![sender.public(), receiver.identity.public()],
                epoch: 0,
                owner: sender.user_id(),
            };
            let genesis = Event::new(
                &sender,
                channel,
                1,
                vec![],
                1,
                1,
                EventKind::MembershipChange,
                meta.encode(),
            );
            let mut chain = ChannelState::from_meta(channel, meta.clone());
            chain.set_identity(sender.user_id());
            let keys = crate::node::channel::seal_keys_for(
                &sender,
                &meta.members,
                &chain.my_sender_distribution(),
                0,
            )
            .unwrap();
            let rotation = Event::new(
                &sender,
                channel,
                2,
                vec![genesis.id],
                2,
                2,
                EventKind::KeyRotation,
                keys.encode(),
            );
            let mut parent = rotation.id;
            let mut events = Vec::new();
            let mut log = receiver.log.lock().unwrap();
            log.append(genesis).unwrap();
            log.append(rotation).unwrap();
            for (index, plaintext) in payloads.into_iter().enumerate() {
                let sequence = index as u64 + 3;
                let event = Event::new(
                    &sender,
                    channel,
                    sequence,
                    vec![parent],
                    sequence,
                    sequence,
                    EventKind::FileManifest,
                    chain.seal_sender_message(&plaintext).unwrap(),
                );
                parent = event.id;
                log.append(event.clone()).unwrap();
                events.push(event);
            }
            drop(log);
            // This fixture installs signed history directly and has no runtime
            // worker. Prepare that setup before exercising file receive/CAS:
            // one reception call intentionally advances only one bounded phase.
            let budget = crate::eventlog::sync::network::PreparationBudget::default();
            let mut ready = false;
            for _ in 0..16 * (events.len() + 2) {
                let log = receiver.log.lock().unwrap();
                let step = receiver
                    .channel_checkpoints
                    .project_step_with_budget(
                        log.repository().unwrap(),
                        &receiver.identity,
                        channel,
                        budget,
                    )
                    .unwrap();
                assert!(step.rows <= budget.rows);
                assert!(step.edges <= budget.edges);
                assert!(step.bytes <= budget.bytes);
                if step.ready {
                    ready = true;
                    break;
                }
            }
            assert!(ready, "signed file fixture authority must finish preparing");
            assert!(events.iter().all(|event| {
                !receiver
                    .received_files
                    .lock()
                    .unwrap()
                    .contains(event.id)
                    .unwrap()
            }));
            receiver
                .process_channel_reception_inner(channel, "live", false)
                .unwrap();
            Self {
                _directory: directory,
                receiver,
                incoming,
                channel,
                events,
            }
        }
        fn process(&self) {
            self.receiver
                .process_channel_file_reception(self.channel, "live")
                .unwrap();
        }
        fn reopen(self) -> Self {
            let Self {
                _directory,
                receiver,
                incoming: _,
                channel,
                events,
            } = self;
            let secret = receiver.identity.secret_bytes();
            receiver.close_profile().unwrap();
            let (dm, _) = mpsc::unbounded_channel();
            let (channel_tx, _) = mpsc::unbounded_channel();
            let (file, incoming) = mpsc::unbounded_channel();
            let receiver = Node::open(
                DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel_tx,
                file,
                &_directory.path().join("messages.log"),
                &_directory.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            Self {
                _directory,
                receiver,
                incoming,
                channel,
                events,
            }
        }
        fn drain(&mut self) -> Vec<ReceivedFile> {
            for _ in 0..20 {
                let before = self
                    .receiver
                    .log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts()
                    .0;
                self.process();
                let after = self
                    .receiver
                    .log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts()
                    .0;
                assert!(
                    after - before <= 64,
                    "one file turn loaded {} bodies",
                    after - before
                );
                if self.receiver.pending_channel_file().is_none() {
                    break;
                }
            }
            assert!(
                self.receiver.pending_channel_file().is_none(),
                "file continuation did not finish"
            );
            let mut files = Vec::new();
            while let Ok(file) = self.incoming.try_recv() {
                files.push(file);
            }
            files
        }
    }
    fn manifest(n: u8) -> crate::file::FileManifest {
        crate::file::FileManifest {
            name: format!("file-{n}.txt"),
            size: 3,
            mime: "text/plain".into(),
            checksum: [n; 32],
            file_key: [n; 32],
            file_conv: ConversationId::new([n; 32]),
            chunk_count: 1,
        }
    }
    #[test]
    fn channel_file_receive_installs_body_book_and_exact_reception() {
        let file = manifest(1);
        let mut f = Fixture::new(vec![file.encode()]);
        let before = f.receiver.reception_effects.head().unwrap();
        f.process();
        let event: EventId = f.events[0].id;
        assert!(
            f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(event)
                .unwrap()
        );
        assert!(
            f.receiver
                .files
                .lock()
                .unwrap()
                .contains_file(file.file_conv)
                .unwrap()
        );
        let received = f.incoming.try_recv().unwrap();
        assert_eq!(received.event_id, event);
        assert_eq!(received.arrival_kind, "live");
        assert!(received.batch_complete);
        assert_eq!(received.batch_size, 1);
        let after = f.receiver.reception_effects.head().unwrap();
        assert!(after.sequence > before.sequence);
        f.process();
        assert_eq!(f.receiver.reception_effects.head().unwrap(), after);
        assert!(f.incoming.try_recv().is_err());
    }

    #[test]
    fn channel_file_preview_counts_only_visible_valid_cards() {
        for rejected in [
            b"invalid manifest".to_vec(),
            crate::expiry::wrap(&manifest(2).encode(), Some(1)),
        ] {
            let mut f = Fixture::new(vec![rejected, manifest(1).encode()]);
            let files = f.drain();
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].arrival_kind, "live");
            assert!(files[0].batch_complete);
            assert!(
                f.receiver
                    .files
                    .lock()
                    .unwrap()
                    .processed(f.events[0].id)
                    .unwrap()
            );
        }
        let mut f = Fixture::new(vec![manifest(2).encode(), manifest(1).encode()]);
        f.receiver
            .account_suppression
            .lock()
            .unwrap()
            .suppress(f.channel, f.events[0].id)
            .unwrap();
        let files = f.drain();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].arrival_kind, "live");
    }

    #[test]
    fn channel_file_preview_retains_progress_across_long_rejected_prefix() {
        for second in [false, true] {
            let mut payloads = vec![manifest(1).encode()];
            payloads.extend((0..128).map(|_| b"invalid manifest".to_vec()));
            if second {
                payloads.push(manifest(2).encode());
            }
            let mut f = Fixture::new(payloads);
            f.process();
            assert!(f.incoming.try_recv().is_err());
            assert!(
                !f.receiver
                    .received_files
                    .lock()
                    .unwrap()
                    .contains(f.events[0].id)
                    .unwrap()
            );
            let files = f.drain();
            assert_eq!(files.len(), if second { 2 } else { 1 });
            assert!(
                files
                    .iter()
                    .all(|f| f.arrival_kind == if second { "catch_up" } else { "live" })
            );
            assert!(files.last().unwrap().batch_complete);
            assert_eq!(files.last().unwrap().batch_size, files.len());
        }
    }

    #[test]
    fn channel_file_receiver_body_book_and_effect_roll_back_together() {
        let mut f = Fixture::new(vec![manifest(1).encode()]);
        let before = f.receiver.reception_effects.head().unwrap();
        f.receiver.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER reject_file_effect BEFORE INSERT ON reception_pending BEGIN SELECT RAISE(ABORT,'injected file effect failure'); END;")?;
            Ok(())
        }).unwrap();
        assert!(
            f.receiver
                .process_channel_file_reception(f.channel, "live")
                .is_err()
        );
        assert!(
            !f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(f.events[0].id)
                .unwrap()
        );
        assert!(
            !f.receiver
                .files
                .lock()
                .unwrap()
                .contains_file(manifest(1).file_conv)
                .unwrap()
        );
        assert_eq!(f.receiver.reception_effects.head().unwrap(), before);
        assert!(f.incoming.try_recv().is_err());
        let reads = f
            .receiver
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        f.receiver
            .process_channel_file_reception(f.channel, "live")
            .unwrap();
        assert_eq!(
            f.receiver
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            reads,
            "storage failure must back off without rereading ciphertext"
        );
        f.receiver
            .storage
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_file_effect;")?;
                Ok(())
            })
            .unwrap();
        f.receiver
            .pending_channel_files
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .retry_at = None;
        let files = f.drain();
        assert_eq!(files.len(), 1);
        assert!(files[0].batch_complete);
        let after = f.receiver.reception_effects.head().unwrap();
        assert!(f.drain().is_empty());
        assert_eq!(f.receiver.reception_effects.head().unwrap(), after);
    }

    #[test]
    fn channel_file_preview_restart_discards_private_keys_and_resumes_exactly_once() {
        let mut payloads = vec![manifest(1).encode()];
        payloads.extend((0..128).map(|_| b"invalid manifest".to_vec()));
        payloads.push(manifest(2).encode());
        let mut f = Fixture::new(payloads);
        let before = f.receiver.reception_effects.head().unwrap();
        f.process();
        assert!(f.incoming.try_recv().is_err());
        f = f.reopen();
        assert_eq!(
            f.receiver
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            (0, 0)
        );
        f.receiver
            .process_channel_file_reception(f.channel, "catch_up")
            .unwrap();
        let files = f.drain();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.arrival_kind == "catch_up"));
        assert!(files[1].batch_complete);
        assert_eq!(
            f.receiver.reception_effects.head().unwrap().sequence,
            before.sequence + 3
        );
        f = f.reopen();
        assert!(f.drain().is_empty());
        assert_eq!(
            f.receiver.reception_effects.head().unwrap().sequence,
            before.sequence + 3
        );
    }

    #[test]
    fn channel_file_preview_preserves_first_transfer_binding_before_commit() {
        let first = manifest(1);
        let mut conflicting = manifest(2);
        conflicting.file_conv = first.file_conv;
        let mut f = Fixture::new(vec![first.encode(), conflicting.encode()]);
        let files = f.drain();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].event_id, f.events[0].id);
        assert_eq!(files[0].arrival_kind, "live");
        assert!(
            !f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(f.events[1].id)
                .unwrap()
        );
    }

    #[test]
    fn channel_file_preview_rejects_a_changed_source_before_consuming_keys() {
        let mut payloads = vec![manifest(1).encode()];
        payloads.extend((0..128).map(|_| b"invalid manifest".to_vec()));
        let mut f = Fixture::new(payloads);
        f.process();
        let tail = Event::new(
            &f.receiver.identity,
            f.channel,
            1,
            vec![f.events.last().unwrap().id],
            1000,
            1000,
            EventKind::Message,
            b"unopened own row".to_vec(),
        );
        f.receiver.log.lock().unwrap().append(tail).unwrap();
        let mut rejected = false;
        for _ in 0..16 {
            let result = f.receiver.process_channel_file_reception(f.channel, "live");
            assert!(
                !f.receiver
                    .received_files
                    .lock()
                    .unwrap()
                    .contains(f.events[0].id)
                    .unwrap()
            );
            if result.is_err() {
                rejected = true;
                break;
            }
        }
        assert!(
            rejected,
            "changed authority must reject the private preview after bounded projection work"
        );
        assert!(
            !f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(f.events[0].id)
                .unwrap()
        );
        f.receiver
            .pending_channel_files
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .retry_at = None;
        let files = f.drain();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].arrival_kind, "catch_up");
    }

    #[test]
    fn channel_file_bounded_host_retains_autosave_work_while_desktop_advances() {
        use crate::node::{ReceptionConsumer, ReceptionLimit};
        let mut f = Fixture::with_delivery((1..=65).map(|n| manifest(n).encode()).collect(), true);
        let mut cursor = f
            .receiver
            .reception_resume(ReceptionConsumer::Desktop)
            .unwrap();
        let autosave = f
            .receiver
            .reception_resume(ReceptionConsumer::CliAutosave)
            .unwrap();
        f.process();
        assert!(!f.receiver.file_incoming.is_legacy());
        assert!(
            f.receiver
                .pending_channel_files
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|turn| turn.legacy_last.is_none())
        );
        assert!(f.drain().is_empty());
        let head = f.receiver.reception_head().unwrap();
        assert_eq!(head.sequence, autosave.sequence + 66);
        let mut count = 0;
        loop {
            let page = f
                .receiver
                .reception_page(
                    cursor,
                    Some(head),
                    ReceptionLimit {
                        rows: 4,
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(page.receipts.len() <= 4);
            count += page.receipts.len();
            f.receiver
                .acknowledge_reception(ReceptionConsumer::Desktop, cursor, page.next)
                .unwrap();
            cursor = page.next;
            if page.complete {
                break;
            }
        }
        assert_eq!(count, 66);
        let pending = f
            .receiver
            .reception_page(
                autosave,
                Some(head),
                ReceptionLimit {
                    rows: 4,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(pending.receipts.len(), 4);
        assert_eq!(
            f.receiver
                .registered_reception(ReceptionConsumer::CliAutosave)
                .unwrap(),
            Some(autosave)
        );
    }

    #[test]
    fn channel_file_terminal_survives_clear_without_republishing_erased_card() {
        let mut f = Fixture::new(vec![manifest(1).encode(), manifest(2).encode()]);
        let before = f.receiver.reception_head().unwrap();
        f.receiver
            .received_files
            .lock()
            .unwrap()
            .fail_once_after_for_test(1);
        assert!(
            f.receiver
                .process_channel_file_reception(f.channel, "catch_up")
                .is_err()
        );
        assert!(f.incoming.try_recv().is_err());
        assert_eq!(
            f.receiver.reception_head().unwrap().sequence,
            before.sequence + 1
        );
        f.receiver.clear_conversation(f.channel).unwrap();
        f.receiver
            .pending_channel_files
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .retry_at = None;
        assert!(
            f.drain().is_empty(),
            "erased final legacy callback must stay hidden"
        );
        let page = f
            .receiver
            .reception_page(before, None, Default::default())
            .unwrap();
        assert_eq!(page.receipts.len(), 1);
        let crate::node::HostReception::ChannelMessage(terminal) = &page.receipts[0] else {
            panic!("expected bodyless file terminal")
        };
        assert!(terminal.reception_only && terminal.batch_complete);
        assert!(terminal.batch_id.as_deref().unwrap().starts_with("file:"));
        assert!(terminal.wrapped.is_empty() && terminal.text.is_empty());
    }

    #[test]
    fn channel_file_existing_history_gains_eligibility_without_replaying_arrival() {
        let file = manifest(1);
        let mut f = Fixture::new(vec![file.encode()]);
        let event = &f.events[0];
        f.receiver
            .received_files
            .lock()
            .unwrap()
            .record(
                f.channel,
                event.author.user_id(),
                event.wall_clock,
                &file.encode(),
                event.id,
            )
            .unwrap();
        let before = f.receiver.reception_head().unwrap();
        let changes = f.receiver.subscribe_file_changes();
        assert!(
            !f.receiver
                .files
                .lock()
                .unwrap()
                .contains_file(file.file_conv)
                .unwrap()
        );
        assert!(f.drain().is_empty());
        assert!(
            f.receiver
                .files
                .lock()
                .unwrap()
                .contains_file(file.file_conv)
                .unwrap()
        );
        assert!(
            changes.has_changed().unwrap(),
            "late eligibility must invalidate current file state"
        );
        assert_eq!(f.receiver.reception_head().unwrap(), before);
    }

    #[test]
    fn channel_file_transaction_rejects_a_different_signed_source() {
        let mut f = Fixture::new(vec![manifest(1).encode(), manifest(2).encode()]);
        let before = f.receiver.reception_head().unwrap();
        let candidate = f
            .receiver
            .channel_checkpoints
            .prepare_receive(&f.events[0])
            .unwrap()
            .unwrap();
        let mut turn = super::ChannelFileTurn::new(f.channel, "catch_up");
        assert!(
            f.receiver
                .commit_channel_file(&candidate, &f.events[1], true, &mut turn)
                .is_err()
        );
        assert_eq!(f.receiver.reception_head().unwrap(), before);
        assert!(
            !f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(f.events[0].id)
                .unwrap()
        );
        assert!(
            !f.receiver
                .received_files
                .lock()
                .unwrap()
                .contains(f.events[1].id)
                .unwrap()
        );
        assert_eq!(f.drain().len(), 2);
    }
}

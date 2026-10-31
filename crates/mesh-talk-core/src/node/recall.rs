//! Message recall (unsend): retract a message I sent so it disappears for everyone,
//! within a short window. Recall is ADDITIVE — it appends a `Delete` event that
//! references the target message and propagates exactly like a reaction (DM/account:
//! sealed-box + account fan-out; channel: per-member sealed). It never removes the
//! target from the synced log. At read time `history`/`account_history` collect the
//! recalled ids and render a placeholder. Mirrors `reactions.rs`.
//!
//! Authorization: only the message's own author can recall it. Account recalls bind the
//! claimed sender account to the authenticating peer (like account reactions) and are
//! only honoured when that account is the target message's author; channel recalls are
//! honoured only when the `Delete` event's author equals the target event's author.

mod indexed;
use super::node::{Node, now_millis};
#[cfg(test)]
use crate::eventlog::LogError;
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::node::NodeError;
use crate::node::conversation::{account_conversation_id, dm_conversation_id};
use crate::node::dm_envelope::RecallEnvelope;
#[cfg(test)]
use crate::storage::record_log::EncryptedRecordLog;
use bincode::Options;
pub(crate) use indexed::IndexedRecallLog;
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::path::Path;

/// How long after sending a message it may still be recalled (WeChat's 2 minutes).
pub const RECALL_WINDOW_MS: u64 = 2 * 60 * 1000;

const RECALL_LOG_MAGIC: &[u8; 6] = b"MTRECL";

/// Sticker artwork preserves message recall while ordinary attachments keep
/// their existing contract. Validation prevents a filename alone granting it.
fn recallable_manifest(bytes: &[u8]) -> bool {
    super::files::validated_manifest(bytes).is_some_and(|manifest| {
        manifest.protection().is_some() || crate::file::sticker::is_sticker_name(manifest.name())
    })
}

/// The plaintext payload of a channel/DM `Delete` event: the message being recalled.
/// Sealed like a reaction before it ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletePayload {
    pub target: EventId,
}

impl DeletePayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("delete payload serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// One of OUR OWN recalls, recorded durably. Our recall payloads are sealed to peers, so
/// we can't recover them from our own log after restart — this is how `history` keeps
/// showing our recalled messages as placeholders across restarts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecallMark {
    pub conversation: ConversationId,
    pub target: EventId,
    pub wall_clock: u64,
}

/// A local-only, password-encrypted log of our own recalls, indexed by conversation.
#[cfg(test)]
pub struct RecallLog {
    file: EncryptedRecordLog<RecallMark>,
    by_conversation: HashMap<ConversationId, Vec<RecallMark>>,
    revision: u64,
}

#[cfg(test)]
impl RecallLog {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, marks) =
            EncryptedRecordLog::<RecallMark>::open(path, password, RECALL_LOG_MAGIC)?;
        let mut by_conversation: HashMap<ConversationId, Vec<RecallMark>> = HashMap::new();
        for mark in marks {
            by_conversation
                .entry(mark.conversation)
                .or_default()
                .push(mark);
        }
        Ok(Self {
            file,
            revision: by_conversation.values().map(Vec::len).sum::<usize>() as u64,
            by_conversation,
        })
    }

    /// Record one of our own recalls (idempotent per `(conversation, target)` — recalling
    /// the same message twice doesn't grow the log).
    pub fn record(
        &mut self,
        conversation: ConversationId,
        target: EventId,
        wall_clock: u64,
    ) -> Result<(), LogError> {
        let already = self
            .by_conversation
            .get(&conversation)
            .is_some_and(|v| v.iter().any(|m| m.target == target));
        if already {
            return Ok(());
        }
        let mark = RecallMark {
            conversation,
            target,
            wall_clock,
        };
        self.file.append_durable(&mark)?;
        self.revision = self.revision.wrapping_add(1);
        self.by_conversation
            .entry(conversation)
            .or_default()
            .push(mark);
        Ok(())
    }

    /// The targets we ourselves recalled in `conversation`.
    pub fn targets(&self, conversation: &ConversationId) -> Vec<EventId> {
        self.by_conversation
            .get(conversation)
            .map(|v| v.iter().map(|m| m.target).collect())
            .unwrap_or_default()
    }

    /// Own recall tombstones retained for a portable snapshot.
    pub(crate) fn snapshot_marks(&self) -> Vec<RecallMark> {
        let mut marks: Vec<_> = self.by_conversation.values().flatten().cloned().collect();
        marks.sort_by(|a, b| {
            a.conversation
                .as_bytes()
                .cmp(b.conversation.as_bytes())
                .then_with(|| a.target.cmp(&b.target))
        });
        marks
    }
}

#[cfg(test)]
mod recovery_snapshot_tests {
    use super::*;

    #[test]
    fn own_recall_marks_can_be_enumerated_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recalls.log");
        let conversation = ConversationId::new([1; 32]);
        let target = EventId::new([2; 32]);
        RecallLog::open(&path, "pw")
            .unwrap()
            .record(conversation, target, 42)
            .unwrap();
        let reopened = RecallLog::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.snapshot_marks(),
            vec![RecallMark {
                conversation,
                target,
                wall_clock: 42
            }]
        );
    }
}

pub(in crate::node) enum ChannelRecallPreparation {
    Retry(Option<EventId>),
    Ignored,
    Mark(RecallMark),
}

impl Node {
    pub(in crate::node) fn prepare_channel_recall(
        &self,
        event: &Event,
        author_x: Option<[u8; 32]>,
        content_admitted: bool,
    ) -> Result<ChannelRecallPreparation, NodeError> {
        if !content_admitted
            || event.kind != EventKind::Delete
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return Ok(ChannelRecallPreparation::Ignored);
        }
        let Some(author_x) = author_x.or_else(|| {
            self.historical_author(event.author.ed25519_pub())
                .map(|p| p.x25519_pub)
        }) else {
            return Ok(ChannelRecallPreparation::Retry(None));
        };
        let Some(payload) = super::channel::SealedPayload::decode(&event.ciphertext)
            .and_then(|sealed| sealed.open(&self.identity, &author_x))
            .and_then(|plain| DeletePayload::decode(&plain))
        else {
            return Ok(ChannelRecallPreparation::Ignored);
        };
        let Some(target) = self
            .log
            .lock()
            .expect("log mutex")
            .repository()?
            .header(&payload.target)?
        else {
            return Ok(ChannelRecallPreparation::Retry(Some(payload.target)));
        };
        if target.author != event.author || target.conversation != event.conversation_id {
            return Ok(ChannelRecallPreparation::Ignored);
        }
        Ok(ChannelRecallPreparation::Mark(RecallMark {
            conversation: event.conversation_id,
            target: payload.target,
            wall_clock: target.wall_clock,
        }))
    }

    /// Recall (unsend) one of OUR OWN account messages: `target` is the logical msg id from
    /// account history. Fails if the message isn't ours or the 2-minute window has passed.
    /// Fans a `RecallEnvelope` (a `Delete` event) out to every device of `target_account_id`
    /// and our own, then records our own recall durably.
    pub async fn recall_account(
        &self,
        target_account_id: &str,
        target: EventId,
    ) -> Result<(), NodeError> {
        self.recall_account_if(target_account_id, target, |commit| commit())
            .await
    }

    /// Enclose signed events and local recall projection in host commit authority.
    pub async fn recall_account_if(
        &self,
        target_account_id: &str,
        target: EventId,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        authorize(&mut || Ok(()))?;
        let my_account = self.account.account_id();
        let acct_conv = account_conversation_id(&my_account, target_account_id);
        // Sibling copies preserve original account authorship and send time.
        let wall = self
            .private_dm_target_route(target_account_id, target)?
            .filter(|(sender, recipient, _)| {
                sender == &my_account && recipient == target_account_id
            })
            .map(|(_, _, wall)| wall);
        let wall = match wall {
            Some(wall) => Some(wall),
            None => self
                .received_files
                .lock()
                .expect("files mutex")
                .entry_in(&acct_conv, target)?
                .filter(|row| row.from == self.user_id() && recallable_manifest(&row.plaintext))
                .map(|row| row.wall_clock),
        };
        let Some(wall) = wall else {
            return Err(NodeError::Channel(
                "can only recall your own message".into(),
            ));
        };
        let already_accepted = self
            .recalls
            .lock()
            .expect("recalls mutex")
            .contains(acct_conv, target)?;
        if !already_accepted && now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
            return Err(NodeError::Channel(
                "the 2-minute recall window has passed".into(),
            ));
        }

        let envelope = RecallEnvelope::new(
            my_account.clone(),
            target_account_id.to_string(),
            *target.as_bytes(),
        )
        .encode();
        let mut deliveries = Vec::new();
        authorize(&mut || {
            for peer in self.account_fanout_targets(target_account_id) {
                let Ok(sealed) =
                    crate::dm::seal(&self.identity, &peer.public.x25519_pub, &envelope)
                else {
                    continue;
                };
                let conv = dm_conversation_id(&self.identity.public(), &peer.public);
                if self.append_event(conv, EventKind::Delete, sealed).is_ok() {
                    deliveries.push((peer, conv));
                }
            }
            // Account recalls retain their best-effort fanout contract, including
            // a durable local recall when every destination is currently unavailable.
            self.record_my_recall(acct_conv, target, wall)?;
            self.publish_own_account_recall(target_account_id, target, wall)
        })?;
        for (peer, conv) in deliveries {
            self.deliver_direct(&peer, conv).await.ok();
            self.replicate_to_post_office(conv).await.ok();
        }
        Ok(())
    }

    /// Recall (unsend) one of OUR OWN channel messages by its event id. Fails if the message
    /// isn't ours / isn't a message / the window has passed. Seals a `DeletePayload`
    /// per-member (like a channel reaction) as a `Delete` event and distributes it.
    pub async fn recall_channel(
        &self,
        channel: ConversationId,
        target: EventId,
    ) -> Result<(), NodeError> {
        self.recall_channel_if(channel, target, |commit| commit())
            .await
    }

    /// Enclose signed events and local recall projection in host commit authority.
    pub async fn recall_channel_if(
        &self,
        channel: ConversationId,
        target: EventId,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        authorize(&mut || Ok(()))?;
        let me = Author::from_ed25519(self.identity.public().ed25519_pub);
        let recallable_file_target = self
            .received_files
            .lock()
            .expect("received files lock")
            .entry_in(&channel, target)?
            .is_some_and(|row| recallable_manifest(&row.plaintext));
        let wall = {
            let log = self.log.lock().expect("log mutex not poisoned");
            let event = log
                .get(&target)?
                .ok_or_else(|| NodeError::Channel("unknown message".into()))?;
            if event.author != me {
                return Err(NodeError::Channel(
                    "can only recall your own message".into(),
                ));
            }
            if event.kind != EventKind::Message
                && !(event.kind == EventKind::FileManifest && recallable_file_target)
            {
                return Err(NodeError::Channel("not a message".into()));
            }
            event.wall_clock
        };
        if now_millis().saturating_sub(wall) > RECALL_WINDOW_MS {
            return Err(NodeError::Channel(
                "the 2-minute recall window has passed".into(),
            ));
        }

        let (members, epoch) = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel(format!("unknown channel {channel:?}")))?;
            if !self.channel_identity_is_current(state)? {
                return Err(NodeError::Channel("not a channel member".into()));
            }
            (state.recipient_members(), state.epoch())
        };
        let payload = DeletePayload { target };
        let sealed =
            crate::node::channel::SealedPayload::seal(&self.identity, &members, &payload.encode())
                .map_err(|e| NodeError::Channel(format!("recall seal failed: {e}")))?;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            {
                let book = self.channels.lock().expect("channels lock");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
                if state.epoch() != epoch || !self.channel_identity_is_current(state)? {
                    return Err(NodeError::Channel(
                        "channel membership changed; retry the operation".into(),
                    ));
                }
            }
            self.append_event(channel, EventKind::Delete, sealed.encode())?;
            self.record_my_recall(channel, target, wall)
        })?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }

    fn record_my_recall(
        &self,
        conv: ConversationId,
        target: EventId,
        wall_clock: u64,
    ) -> Result<(), NodeError> {
        self.recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .record(conv, target, wall_clock)
            .map_err(NodeError::Log)
    }

    /// Selected-row visibility. The caller supplies the author from its already
    /// authenticated transcript/manifest. Producers persist verified peer recall
    /// marks; account-private claims are read by their exact certified key.
    pub(crate) fn message_recalled_selected(
        &self,
        conversation: ConversationId,
        id: EventId,
        account: Option<&str>,
        author_account: Option<&str>,
    ) -> Result<bool, NodeError> {
        if account
            .is_some_and(|peer| account_conversation_id(&self.account_id(), peer) != conversation)
        {
            return Ok(false);
        }
        let (logical, mut aliases) = {
            let identities = self
                .file_message_identities
                .lock()
                .expect("file identity mutex");
            (
                identities.logical(conversation, id)?,
                identities.aliases(conversation, id)?,
            )
        };
        if !aliases.contains(&logical) {
            aliases.push(logical);
        }
        if !aliases.contains(&id) {
            aliases.push(id);
        }
        for target in aliases {
            if self
                .recalls
                .lock()
                .expect("recalls mutex")
                .contains(conversation, target)?
            {
                return Ok(true);
            }
            if let (Some(peer), Some(author)) = (account, author_account)
                && self.private_dm_recall_selected(peer, target, author)?
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Compatibility enumeration for legacy recall fixtures.
    #[cfg(test)]
    pub(in crate::node) fn recalled_targets_channel(
        &self,
        conv: ConversationId,
    ) -> Result<HashSet<EventId>, NodeError> {
        let mut out = HashSet::new();
        let mut after = None;
        loop {
            let page = self
                .recalls
                .lock()
                .expect("recalls mutex")
                .page(Some(conv), after, 128)?;
            if page.is_empty() {
                break;
            }
            for mark in page {
                after = Some((mark.conversation, mark.target));
                out.insert(mark.target);
            }
        }
        Ok(out)
    }
    #[cfg(test)]
    pub(in crate::node) fn recalled_targets_account(
        &self,
        peer: &str,
    ) -> Result<HashSet<EventId>, NodeError> {
        for author in self.historical_dm_peers() {
            if matches!(author.account_id().as_deref(),Some(a) if a==peer||a==self.account_id()) {
                self.project_private_dm_controls(dm_conversation_id(
                    &self.identity.public(),
                    &author.public(),
                ))?;
            }
        }
        let conv = account_conversation_id(&self.account_id(), peer);
        let mut out = self.recalled_targets_channel(conv)?;
        for (target, actor) in self.private_dm_recall_claims(peer) {
            if let Some((author, _, wall)) = self.private_dm_target_route(peer, target)?
                && actor == author
            {
                self.recalls
                    .lock()
                    .expect("recalls mutex")
                    .record(conv, target, wall)?;
                out.insert(target);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_payload_round_trips_and_rejects_trailing() {
        let p = DeletePayload {
            target: EventId::new([9u8; 32]),
        };
        assert_eq!(DeletePayload::decode(&p.encode()), Some(p.clone()));
        let mut junk = p.encode();
        junk.push(0x01);
        assert_eq!(DeletePayload::decode(&junk), None);
    }

    #[test]
    fn recall_log_records_dedups_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recalls.log");
        let conv = ConversationId::new([1u8; 32]);
        let t1 = EventId::new([1u8; 32]);
        let t2 = EventId::new([2u8; 32]);
        {
            let mut r = RecallLog::open(&path, "pw").unwrap();
            r.record(conv, t1, 1000).unwrap();
            r.record(conv, t1, 1000).unwrap(); // duplicate target → no growth
            r.record(conv, t2, 2000).unwrap();
            let mut targets = r.targets(&conv);
            targets.sort_by_key(|e| *e.as_bytes());
            assert_eq!(targets, vec![t1, t2]);
        }
        // Survives reopen (durable, unlike own reactions).
        let r = RecallLog::open(&path, "pw").unwrap();
        assert_eq!(r.targets(&conv).len(), 2);
        assert!(r.targets(&ConversationId::new([9u8; 32])).is_empty());
    }
}

#[cfg(test)]
mod acceptance_tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[tokio::test]
    async fn sibling_recalls_imported_account_message_without_original_device_sentlog() {
        use std::sync::Arc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let account = Account::generate();
        let source_identity = DeviceIdentity::generate();
        let sibling_identity = DeviceIdentity::generate();
        let recipient_identity = DeviceIdentity::generate();
        let recipient_account = Account::generate();
        let source_proof = Announce::new_with_account(&source_identity, &account, "source", 9);
        let sibling_proof = Announce::new_with_account(&sibling_identity, &account, "sibling", 9);
        let recipient_proof = Announce::new_with_account(
            &recipient_identity,
            &recipient_account,
            "recipient",
            listener.local_addr().unwrap().port(),
        );
        let (source, _, _) = super::super::test_support::node_with_files(
            &dir.path().join("source"),
            source_identity,
            Account::from_secret_bytes(account.secret_bytes()),
            &recipient_proof,
        );
        let (sibling, _, _) = super::super::test_support::node_with_files(
            &dir.path().join("sibling"),
            sibling_identity,
            account,
            &recipient_proof,
        );
        let (recipient, _, _) = super::super::test_support::node_with_files(
            &dir.path().join("recipient"),
            recipient_identity,
            recipient_account,
            &source_proof,
        );
        for (node, folder, proof) in [
            (&source, "source", &source_proof),
            (&sibling, "sibling", &sibling_proof),
            (&recipient, "recipient", &recipient_proof),
        ] {
            node.configure_privacy(
                &dir.path().join(folder),
                "pw",
                proof,
                Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
            )
            .unwrap();
        }
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(recipient.clone().run_accept_loop(listener));
        let id = source
            .enqueue_to_account(&recipient.account_id(), b"account-owned original", None)
            .await
            .unwrap();
        source.flush_delivery(id).await;
        assert_eq!(
            recipient.account_history(&source.account_id(), 10).unwrap()[0].id,
            id
        );
        sibling
            .import_account_backfill(&source.export_account_backfill().unwrap())
            .unwrap();
        let conversation = account_conversation_id(&sibling.account_id(), &recipient.account_id());
        assert!(
            sibling
                .sentlog
                .lock()
                .unwrap()
                .entries(&conversation)
                .is_empty()
        );
        let imported = sibling
            .account_history(&recipient.account_id(), 10)
            .unwrap();
        assert_eq!(imported[0].id, id);
        assert!(imported[0].from_me);
        sibling
            .recall_account(&recipient.account_id(), id)
            .await
            .expect("a certified sibling can recall the account's imported own message");
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if recipient.account_history(&source.account_id(), 10).unwrap()[0].recalled {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("external recipient accepts the sibling's account-authorized recall");
        let recalled = sibling
            .account_history(&recipient.account_id(), 10)
            .unwrap();
        assert!(recalled[0].recalled);
        assert_eq!(
            recalled[0].recalled_text.as_deref(),
            Some(b"account-owned original".as_slice())
        );
        tasks.shutdown().await;
    }

    #[tokio::test]
    async fn imported_recall_keeps_original_window_and_rejects_foreign_or_wrong_peer() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let foreign_account = Account::generate();
        let foreign =
            Announce::new_with_account(&DeviceIdentity::generate(), &foreign_account, "foreign", 9);
        let (source, _, _) = super::super::test_support::node_with_files(
            &dir.path().join("source"),
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account.secret_bytes()),
            &foreign,
        );
        let (sibling, _, _) = super::super::test_support::node_with_files(
            &dir.path().join("sibling"),
            DeviceIdentity::generate(),
            account,
            &foreign,
        );
        let own = source.account_id();
        let peer = foreign_account.account_id();
        let personal = account_conversation_id(&own, &own);
        let external = account_conversation_id(&own, &peer);
        let old = EventId::new([51; 32]);
        let incoming = EventId::new([52; 32]);
        let old_wall = now_millis() - RECALL_WINDOW_MS - 10_000;
        let body = super::super::MessageBody::new(b"original".to_vec(), None).encode();
        source
            .sentlog
            .lock()
            .unwrap()
            .record(
                personal,
                1,
                old_wall,
                &super::super::DmEnvelope::new(
                    own.clone(),
                    own.clone(),
                    *old.as_bytes(),
                    body.clone(),
                )
                .encode(),
            )
            .unwrap();
        source
            .received
            .lock()
            .unwrap()
            .record(
                external,
                peer.clone(),
                now_millis(),
                &super::super::DmEnvelope::new(
                    peer.clone(),
                    own.clone(),
                    *incoming.as_bytes(),
                    body,
                )
                .encode(),
                incoming,
            )
            .unwrap();
        sibling
            .import_account_backfill(&source.export_account_backfill().unwrap())
            .unwrap();
        assert!(
            sibling
                .sentlog
                .lock()
                .unwrap()
                .entries(&personal)
                .is_empty()
        );
        assert_eq!(
            sibling.account_history(&own, 10).unwrap()[0].wall_clock,
            old_wall
        );
        assert!(sibling.account_history(&own, 10).unwrap()[0].from_me);
        assert!(!sibling.account_history(&peer, 10).unwrap()[0].from_me);
        assert!(matches!(
            sibling.recall_account(&own, old).await,
            Err(NodeError::Channel(message)) if message.contains("2-minute"),
        ));
        for target in [old, incoming] {
            assert!(matches!(
                sibling.recall_account(&peer, target).await,
                Err(NodeError::Channel(message)) if message.contains("own message"),
            ));
        }
        for conversation in [personal, external] {
            assert!(
                sibling
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&conversation)
                    .is_empty()
            );
        }
        assert!(
            sibling
                .account_documents(super::super::AccountDocumentDomain::DmRecall, None, 64,)
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[tokio::test]
    async fn rejected_channel_recall_append_does_not_create_tombstone() {
        let dir = tempfile::tempdir().unwrap();
        let own = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 1);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            own,
            Account::generate(),
            &proof,
        );
        let channel = node.create_channel("local", vec![]).await.unwrap();
        let target = {
            let mut log = node.log.lock().unwrap();
            let (parents, clock) = log.prepare(&channel).unwrap();
            let target = Event::new(
                &node.identity,
                channel,
                100,
                parents,
                clock,
                now_millis(),
                EventKind::Message,
                vec![],
            );
            log.append(target.clone()).unwrap();
            log.append(Event::new(
                &peer,
                channel,
                1,
                vec![target.id],
                u64::MAX,
                now_millis(),
                EventKind::Message,
                vec![],
            ))
            .unwrap();
            target
        };
        assert!(matches!(
            node.recall_channel(channel, target.id).await,
            Err(NodeError::Log(LogError::ClockExhausted))
        ));
        assert!(node.recalls.lock().unwrap().targets(&channel).is_empty());
    }
}

#[cfg(test)]
mod selected_lookup_tests {
    use super::*;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };
    #[test]
    fn selected_recall_lookup_binds_scope_and_authenticated_author() {
        let dir = tempfile::tempdir().unwrap();
        let own = Account::generate();
        let peer = Account::generate();
        let proof = Announce::new_with_account(&DeviceIdentity::generate(), &peer, "peer", 9);
        let (node, _, _) = crate::node::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            Account::from_secret_bytes(own.secret_bytes()),
            &proof,
        );
        let host = account_conversation_id(&own.account_id(), &peer.account_id());
        let selected = EventId::new([19; 32]);
        assert!(
            !node
                .message_recalled_selected(
                    host,
                    selected,
                    Some(&peer.account_id()),
                    Some(&own.account_id())
                )
                .unwrap()
        );
        node.publish_own_account_recall(&peer.account_id(), selected, 100)
            .unwrap();
        assert!(
            node.message_recalled_selected(
                host,
                selected,
                Some(&peer.account_id()),
                Some(&own.account_id())
            )
            .unwrap()
        );
        assert!(
            !node
                .message_recalled_selected(
                    host,
                    selected,
                    Some(&peer.account_id()),
                    Some(&peer.account_id())
                )
                .unwrap()
        );
        assert!(
            !node
                .message_recalled_selected(host, selected, Some(&peer.account_id()), None)
                .unwrap()
        );
        assert!(
            !node
                .message_recalled_selected(
                    ConversationId::new([42; 32]),
                    selected,
                    Some(&peer.account_id()),
                    Some(&own.account_id())
                )
                .unwrap()
        );
        node.record_my_recall(host, selected, 100).unwrap();
        assert!(
            node.message_recalled_selected(host, selected, None, None)
                .unwrap()
        );
        assert!(
            !node
                .message_recalled_selected(ConversationId::new([42; 32]), selected, None, None)
                .unwrap()
        );
    }
}

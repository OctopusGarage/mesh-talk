//! Source archive selectors. The cache retains metadata, never a second copy of plaintext.
use super::*;
use std::collections::BTreeMap;
use std::ops::Bound::{Excluded, Unbounded};

pub(super) fn empty_history_digest() -> [u8; 32] {
    ArchiveIndex::default().finish().digest
}

type AccountExportPage = (
    Vec<HistoryRecord>,
    Option<HistoryCursor>,
    [u8; 32],
    (u64, [u8; 32]),
);
type RecalledAccounts = HashMap<ConversationId, HashSet<EventId>>;

fn recalled_in(recalled: &RecalledAccounts, conversation: ConversationId, target: EventId) -> bool {
    recalled
        .get(&conversation)
        .is_some_and(|ids| ids.contains(&target))
}

#[derive(Clone)]
enum ArchiveRow {
    SentDirect {
        conversation: ConversationId,
        index: usize,
    },
    ReceivedDirect {
        conversation: ConversationId,
        index: usize,
    },
    SentChannel {
        conversation: ConversationId,
        index: usize,
        event_id: EventId,
    },
    ReceivedChannel {
        conversation: ConversationId,
        index: usize,
    },
    File {
        conversation: ConversationId,
        index: usize,
        peer_account: Option<String>,
    },
}

#[derive(Default)]
struct ArchiveIndex {
    rows: BTreeMap<HistoryCursor, ArchiveRow>,
    digest: [u8; 32],
    digest_xor: [u8; 32],
    scanned: HashMap<(u8, ConversationId), usize>,
}
impl ArchiveIndex {
    fn offer(&mut self, key: HistoryCursor, row: ArchiveRow) {
        if self.rows.contains_key(&key) {
            return;
        }
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-history-record-v2");
        hash.update([key.tag]);
        hash.update(key.id);
        for (byte, part) in self.digest_xor.iter_mut().zip(hash.finalize()) {
            *byte ^= part;
        }
        self.rows.insert(key, row);
    }
    fn finish(mut self) -> Self {
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-history-digest-v2");
        hash.update(self.digest_xor);
        hash.update((self.rows.len() as u64).to_be_bytes());
        self.digest = hash.finalize().into();
        self
    }
}
#[derive(PartialEq, Eq)]
struct ArchiveKey {
    history: bool,
    files: bool,
    version: AccountSyncVersion,
    revisions: [u64; 8],
    channels: Vec<ConversationId>,
    accounts: Vec<String>,
    controls: (u64, [u8; 32]),
    recalls: [u8; 32],
}
impl ArchiveKey {
    // Appends leave captured selectors valid. Erasure, policy and authorization
    // transitions do not; fail closed on those without restarting for ordinary writes.
    fn accepts_append_only_change(&self, current: &Self) -> bool {
        self.history == current.history
            && self.files == current.files
            && self.version == current.version
            && self.channels == current.channels
            && self.accounts == current.accounts
            && self.controls == current.controls
            && self.recalls == current.recalls
            && self.revisions[3..] == current.revisions[3..]
    }
}

/// A single shared selector index avoids multiplying retained archive memory by sibling count.
#[derive(Default)]
pub(in crate::node) struct ArchiveCache {
    key: Option<ArchiveKey>,
    index: Option<ArchiveIndex>,
    #[cfg(test)]
    pub(super) builds: usize,
    #[cfg(test)]
    pub(super) scanned_rows: usize,
}

impl Node {
    /// Resolve each conversation's recalls once, before taking any row-store
    /// locks. The same projection filters selectors and materialized payloads.
    pub(super) fn account_archive_recalls(&self) -> (RecalledAccounts, [u8; 32]) {
        let own = self.account_id();
        let mut peers = self.known_account_ids();
        peers.push(own.clone());
        peers.sort();
        peers.dedup();
        let mut recalled = HashMap::new();
        let mut digest = [0u8; 32];
        let mut count = 0u64;
        for peer in peers {
            let conversation = account_conversation_id(&own, &peer);
            let targets = self.recalled_targets_account(&peer);
            for target in &targets {
                let mut hash = Sha256::new();
                hash.update(b"mesh-talk-account-archive-recall-v1");
                hash.update(conversation.as_bytes());
                hash.update(target.as_bytes());
                for (byte, part) in digest.iter_mut().zip(hash.finalize()) {
                    *byte ^= part;
                }
                count = count.saturating_add(1);
            }
            if !targets.is_empty() {
                recalled.insert(conversation, targets);
            }
        }
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(count.to_be_bytes());
        (recalled, hash.finalize().into())
    }

    fn build_archive_index(
        &self,
        requester: &PublicIdentity,
        include_history: bool,
        include_files: bool,
        version: AccountSyncVersion,
        previous: Option<ArchiveIndex>,
        recalled: &RecalledAccounts,
    ) -> ArchiveIndex {
        let own = self.account_id();
        let mut page = previous.unwrap_or_default();
        let requester_id = requester.user_id();
        let me = self.user_id();
        let channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id).is_some_and(|state| {
                        !state.is_dissolved()
                            && state.is_accepted_member(&me)
                            && state.is_accepted_member(&requester_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        if include_history {
            {
                let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
                for conversation in sent.conversations() {
                    for (index, entry) in sent
                        .entries_ref(&conversation)
                        .iter()
                        .enumerate()
                        .skip(page.scanned.get(&(0, conversation)).copied().unwrap_or(0))
                    {
                        page.scanned.insert((0, conversation), index + 1);
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let Some(env) = DmEnvelope::decode(&entry.plaintext) else {
                            continue;
                        };
                        if recalled_in(recalled, conversation, EventId::new(env.msg_id)) {
                            continue;
                        }
                        if self.recipient_expired(
                            &env.body,
                            EventId::new(env.msg_id),
                            entry.wall_clock,
                        ) {
                            continue;
                        }
                        if env.route.sender_account != own
                            || *account_conversation_id(&own, &env.route.recipient_account)
                                .as_bytes()
                                != *conversation.as_bytes()
                        {
                            continue;
                        }
                        page.offer(
                            HistoryCursor {
                                tag: 0,
                                id: env.msg_id,
                            },
                            ArchiveRow::SentDirect {
                                conversation,
                                index,
                            },
                        );
                    }
                }
            }
            {
                let received = self.received.lock().expect("received mutex not poisoned");
                for conversation in received.conversations() {
                    for (index, entry) in received
                        .entries_ref(&conversation)
                        .iter()
                        .enumerate()
                        .skip(page.scanned.get(&(1, conversation)).copied().unwrap_or(0))
                    {
                        page.scanned.insert((1, conversation), index + 1);
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let Some(env) = DmEnvelope::decode(&entry.plaintext) else {
                            continue;
                        };
                        if recalled_in(recalled, conversation, EventId::new(env.msg_id)) {
                            continue;
                        }
                        let other = if env.route.sender_account == own {
                            &env.route.recipient_account
                        } else if env.route.recipient_account == own {
                            &env.route.sender_account
                        } else {
                            continue;
                        };
                        if env.route.sender_account != entry.from
                            || *account_conversation_id(&own, other).as_bytes()
                                != *conversation.as_bytes()
                        {
                            continue;
                        }
                        if self.recipient_expired(&env.body, entry.event_id, entry.wall_clock) {
                            continue;
                        }
                        page.offer(
                            HistoryCursor {
                                tag: 0,
                                id: env.msg_id,
                            },
                            ArchiveRow::ReceivedDirect {
                                conversation,
                                index,
                            },
                        );
                    }
                }
            }
            for &channel in &channels {
                {
                    let log = self.log.lock().expect("log mutex not poisoned");
                    let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
                    let recovered = self
                        .recovered_sent_ids
                        .lock()
                        .expect("recovered sent ids mutex not poisoned");
                    let author =
                        crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
                    for (index, entry) in sent
                        .entries_ref(&channel)
                        .iter()
                        .enumerate()
                        .skip(page.scanned.get(&(2, channel)).copied().unwrap_or(0))
                    {
                        page.scanned.insert((2, channel), index + 1);
                        if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                            continue;
                        }
                        let id = recovered
                            .as_ref()
                            .and_then(|saved| saved.id_for(entry))
                            .or_else(|| {
                                log.authored_event(channel, author, entry.seq)
                                    .filter(|event| {
                                        event.kind == crate::eventlog::EventKind::Message
                                    })
                                    .map(|event| event.id)
                            });
                        let Some(id) = id else { continue };
                        if self.recipient_expired(&entry.plaintext, id, entry.wall_clock) {
                            continue;
                        }
                        page.offer(
                            HistoryCursor {
                                tag: 1,
                                id: *id.as_bytes(),
                            },
                            ArchiveRow::SentChannel {
                                conversation: channel,
                                index,
                                event_id: id,
                            },
                        );
                    }
                }
                let received = self.received.lock().expect("received mutex not poisoned");
                for (index, entry) in received
                    .entries_ref(&channel)
                    .iter()
                    .enumerate()
                    .skip(page.scanned.get(&(3, channel)).copied().unwrap_or(0))
                {
                    page.scanned.insert((3, channel), index + 1);
                    if entry.plaintext.is_empty() || entry.plaintext.len() > MAX_RECORD {
                        continue;
                    }
                    if self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock) {
                        continue;
                    }
                    page.offer(
                        HistoryCursor {
                            tag: 1,
                            id: *entry.event_id.as_bytes(),
                        },
                        ArchiveRow::ReceivedChannel {
                            conversation: channel,
                            index,
                        },
                    );
                }
            }
        }
        if include_files {
            let mut allowed_hosts: HashMap<ConversationId, Option<String>> = HashMap::new();
            for &channel in &channels {
                allowed_hosts.insert(channel, None);
            }
            if version == AccountSyncVersion::V2 {
                allowed_hosts.insert(account_conversation_id(&own, &own), Some(own.clone()));
            }
            for other in self.known_account_ids() {
                allowed_hosts.insert(account_conversation_id(&own, &other), Some(other));
            }
            let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
            let rows = self
                .received_files
                .lock()
                .expect("received files mutex not poisoned");
            let log = self.log.lock().expect("log mutex not poisoned");
            let proofs = self
                .account_file_proofs
                .lock()
                .expect("file proofs mutex not poisoned");
            let suppression = self
                .account_suppression
                .lock()
                .expect("account suppression mutex not poisoned");
            for host in rows.conversations() {
                let Some(peer_account) = allowed_hosts.get(&host) else {
                    continue;
                };
                for (index, entry) in rows
                    .entries_ref(&host)
                    .iter()
                    .enumerate()
                    .skip(page.scanned.get(&(4, host)).copied().unwrap_or(0))
                {
                    page.scanned.insert((4, host), index + 1);
                    if entry.plaintext.is_empty()
                        || recalled_in(recalled, host, entry.event_id)
                        || self.recipient_expired(
                            &entry.plaintext,
                            entry.event_id,
                            entry.wall_clock,
                        )
                        || delivery.file_erased(host, entry.event_id)
                        || delivery.manifest_event_erased(entry.event_id)
                        || suppression.contains(host, entry.event_id)
                    {
                        continue;
                    }
                    let Some(proof) = log
                        .get(&entry.event_id)
                        .or_else(|| proofs.get(&entry.event_id))
                    else {
                        continue;
                    };
                    if super::super::files::eligible_manifest_row(entry, Some(proof), None)
                        .is_none()
                    {
                        continue;
                    }
                    let payload_size = bincode::DefaultOptions::new()
                        .with_fixint_encoding()
                        .serialized_size(&FileCardPayloadRef {
                            manifest: &entry.plaintext,
                            proof,
                        })
                        .unwrap_or(u64::MAX);
                    if payload_size > MAX_RECORD as u64 {
                        continue;
                    }
                    page.offer(
                        HistoryCursor {
                            tag: 2,
                            id: *entry.event_id.as_bytes(),
                        },
                        ArchiveRow::File {
                            conversation: host,
                            index,
                            peer_account: peer_account.clone(),
                        },
                    );
                }
            }
        }
        page.finish()
    }

    fn archive_key(
        &self,
        requester: &PublicIdentity,
        history: bool,
        files: bool,
        version: AccountSyncVersion,
        recalls: [u8; 32],
    ) -> ArchiveKey {
        let me = self.user_id();
        let requester_id = requester.user_id();
        let mut channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id).is_some_and(|state| {
                        !state.is_dissolved()
                            && state.is_accepted_member(&me)
                            && state.is_accepted_member(&requester_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        channels.sort_by_key(|id| *id.as_bytes());
        let (sent, sent_rewrite) = {
            let log = self.sentlog.lock().expect("sentlog mutex not poisoned");
            (log.revision(), log.rewrite_revision())
        };
        let (received, received_rewrite) = {
            let log = self.received.lock().expect("received mutex not poisoned");
            (log.revision(), log.rewrite_revision())
        };
        let (files_revision, files_rewrite) = {
            let log = self
                .received_files
                .lock()
                .expect("received files mutex not poisoned");
            (log.revision(), log.rewrite_revision())
        };
        ArchiveKey {
            history,
            files,
            version,
            controls: self.private_dm_control_revision(),
            recalls,
            channels,
            accounts: if files {
                let mut accounts = self.known_account_ids();
                accounts.sort();
                accounts.dedup();
                accounts
            } else {
                Vec::new()
            },
            revisions: [
                sent,
                received,
                files_revision,
                self.device_sync_policy_snapshot().1,
                0, // Attachment readiness alone does not invalidate archive metadata.
                sent_rewrite,
                received_rewrite,
                files_rewrite,
            ],
        }
    }

    pub(super) fn prepare_account_export(
        &self,
        requester: &PublicIdentity,
        after: Option<HistoryCursor>,
        history: bool,
        files: bool,
        version: AccountSyncVersion,
    ) -> Result<AccountExportPage, NodeError> {
        let (recalled, recalls) = self.account_archive_recalls();
        // The lock prevents duplicate concurrent index builds. Producers never acquire it.
        let mut cache = self
            .account_archive_cache
            .lock()
            .expect("archive cache mutex not poisoned");
        let key = self.archive_key(requester, history, files, version, recalls);
        if cache.key.as_ref() != Some(&key) || cache.index.is_none() {
            let incremental = cache.key.as_ref().is_some_and(|old| {
                old.history == key.history
                    && old.files == key.files
                    && old.version == key.version
                    && old.channels == key.channels
                    && old.accounts == key.accounts
                    && old.controls == key.controls
                    && old.recalls == key.recalls
                    && old.revisions[3] == key.revisions[3]
                    && old.revisions[5..] == key.revisions[5..]
                    && old.revisions[..3] != key.revisions[..3]
            });
            let previous = if incremental {
                cache.index.take()
            } else {
                None
            };
            #[cfg(test)]
            let scanned_before: usize = previous
                .as_ref()
                .map_or(0, |index| index.scanned.values().sum());
            let index =
                self.build_archive_index(requester, history, files, version, previous, &recalled);
            #[cfg(test)]
            {
                cache.scanned_rows += index.scanned.values().sum::<usize>() - scanned_before;
            }
            if !key.accepts_append_only_change(
                &self.archive_key(requester, history, files, version, recalls),
            ) {
                cache.index = None;
                return Err(NodeError::Channel(
                    "account archive changed while indexing".into(),
                ));
            }
            cache.key = Some(key);
            cache.index = Some(index);
            #[cfg(test)]
            {
                cache.builds += 1;
            }
        }
        let index = cache.index.as_ref().expect("archive index installed");
        let selected = match after {
            Some(after) => index
                .rows
                .range((Excluded(after), Unbounded))
                .take(RECORDS_PER_PAGE + 1)
                .collect::<Vec<_>>(),
            None => index
                .rows
                .iter()
                .take(RECORDS_PER_PAGE + 1)
                .collect::<Vec<_>>(),
        };
        let next = (selected.len() > RECORDS_PER_PAGE).then(|| *selected[RECORDS_PER_PAGE - 1].0);
        let mut records = Vec::with_capacity(RECORDS_PER_PAGE);
        for (key, row) in selected.into_iter().take(RECORDS_PER_PAGE) {
            if let Some(record) = self.materialize_archive_row(*key, row, &recalled)? {
                records.push(record);
            }
        }
        if !cache
            .key
            .as_ref()
            .expect("archive key installed")
            .accepts_append_only_change(
                &self.archive_key(requester, history, files, version, recalls),
            )
        {
            return Err(NodeError::Channel(
                "account archive changed while selecting page".into(),
            ));
        }
        Ok((
            records,
            next,
            index.digest,
            cache.key.as_ref().expect("archive key installed").controls,
        ))
    }

    fn materialize_archive_row(
        &self,
        key: HistoryCursor,
        row: &ArchiveRow,
        recalled: &RecalledAccounts,
    ) -> Result<Option<HistoryRecord>, NodeError> {
        let stale = || NodeError::Channel("account archive selector invalidated".into());
        let conversation = match row {
            ArchiveRow::SentDirect { conversation, .. }
            | ArchiveRow::ReceivedDirect { conversation, .. }
            | ArchiveRow::SentChannel { conversation, .. }
            | ArchiveRow::ReceivedChannel { conversation, .. }
            | ArchiveRow::File { conversation, .. } => *conversation,
        };
        if key.tag != 1 && recalled_in(recalled, conversation, EventId::new(key.id)) {
            return Ok(None);
        }
        if self
            .account_suppression
            .lock()
            .expect("suppression mutex not poisoned")
            .contains(conversation, EventId::new(key.id))
        {
            return Ok(None);
        }
        let record = match row {
            ArchiveRow::SentDirect {
                conversation,
                index,
            } => {
                let log = self.sentlog.lock().expect("sentlog mutex not poisoned");
                let entry = log
                    .entries_ref(conversation)
                    .get(*index)
                    .ok_or_else(stale)?;
                let env = DmEnvelope::decode(&entry.plaintext).ok_or_else(stale)?;
                if env.msg_id != key.id {
                    return Err(stale());
                }
                if self.recipient_expired(&env.body, EventId::new(env.msg_id), entry.wall_clock) {
                    return Ok(None);
                }
                HistoryRecord::Direct(BackfillRecord {
                    conv: *conversation.as_bytes(),
                    from: env.route.sender_account,
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext.clone(),
                    event_id: env.msg_id,
                })
            }
            ArchiveRow::ReceivedDirect {
                conversation,
                index,
            }
            | ArchiveRow::ReceivedChannel {
                conversation,
                index,
            } => {
                let log = self.received.lock().expect("received mutex not poisoned");
                let entry = log
                    .entries_ref(conversation)
                    .get(*index)
                    .ok_or_else(stale)?;
                if key.tag == 0 {
                    let env = DmEnvelope::decode(&entry.plaintext).ok_or_else(stale)?;
                    if env.msg_id != key.id {
                        return Err(stale());
                    }
                    if self.recipient_expired(&env.body, EventId::new(env.msg_id), entry.wall_clock)
                    {
                        return Ok(None);
                    }
                    HistoryRecord::Direct(BackfillRecord {
                        conv: *conversation.as_bytes(),
                        from: entry.from.clone(),
                        wall_clock: entry.wall_clock,
                        plaintext: entry.plaintext.clone(),
                        event_id: env.msg_id,
                    })
                } else {
                    if *entry.event_id.as_bytes() != key.id {
                        return Err(stale());
                    }
                    if self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock) {
                        return Ok(None);
                    }
                    HistoryRecord::Channel(ChannelRecord {
                        conversation: *conversation.as_bytes(),
                        from: entry.from.clone(),
                        wall_clock: entry.wall_clock,
                        plaintext: entry.plaintext.clone(),
                        event_id: key.id,
                    })
                }
            }
            ArchiveRow::SentChannel {
                conversation,
                index,
                event_id,
            } => {
                let log = self.sentlog.lock().expect("sentlog mutex not poisoned");
                let entry = log
                    .entries_ref(conversation)
                    .get(*index)
                    .ok_or_else(stale)?;
                if self.recipient_expired(&entry.plaintext, *event_id, entry.wall_clock) {
                    return Ok(None);
                }
                HistoryRecord::Channel(ChannelRecord {
                    conversation: *conversation.as_bytes(),
                    from: self.user_id(),
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext.clone(),
                    event_id: key.id,
                })
            }
            ArchiveRow::File {
                conversation,
                index,
                peer_account,
            } => {
                let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
                let rows = self
                    .received_files
                    .lock()
                    .expect("received files mutex not poisoned");
                let entry = rows
                    .entries_ref(conversation)
                    .get(*index)
                    .ok_or_else(stale)?;
                if *entry.event_id.as_bytes() != key.id {
                    return Err(stale());
                }
                if self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
                    || delivery.file_erased(*conversation, entry.event_id)
                    || delivery.manifest_event_erased(entry.event_id)
                    || self
                        .account_suppression
                        .lock()
                        .expect("suppression mutex not poisoned")
                        .contains(*conversation, entry.event_id)
                {
                    return Ok(None);
                }
                let log = self.log.lock().expect("log mutex not poisoned");
                let proofs = self
                    .account_file_proofs
                    .lock()
                    .expect("file proofs mutex not poisoned");
                let proof = log
                    .get(&entry.event_id)
                    .or_else(|| proofs.get(&entry.event_id))
                    .ok_or_else(stale)?;
                if super::super::files::eligible_manifest_row(entry, Some(proof), None).is_none() {
                    return Ok(None);
                }
                let payload = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .serialize(&FileCardPayloadRef {
                        manifest: &entry.plaintext,
                        proof,
                    })
                    .map_err(|_| stale())?;
                HistoryRecord::FileCard(FileCardRecord {
                    host: *conversation.as_bytes(),
                    peer_account: peer_account.clone(),
                    from: entry.from.clone(),
                    wall_clock: entry.wall_clock,
                    event_id: key.id,
                    payload,
                })
            }
        };
        Ok(Some(record))
    }
}

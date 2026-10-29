//! Persistent archive selectors. Only a bounded selected page hydrates message bodies.
use super::*;
use crate::eventlog::LogError;
use crate::node::transcript_repository::{FILES, RECEIVED, SENT, TranscriptPageLimit};
use rusqlite::{OptionalExtension, params};

const SCHEMA: &str = "
CREATE TABLE account_archive_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1),token BLOB NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1)),digest_xor BLOB NOT NULL CHECK(length(digest_xor)=32),records INTEGER NOT NULL CHECK(records>=0),sent_cursor INTEGER NOT NULL,received_cursor INTEGER NOT NULL,file_cursor INTEGER NOT NULL,deadline BLOB CHECK(deadline IS NULL OR length(deadline)=8));
CREATE TABLE account_archive_rows(tag INTEGER NOT NULL CHECK(tag BETWEEN 0 AND 2),id BLOB NOT NULL CHECK(length(id)=32),kind INTEGER NOT NULL CHECK(kind BETWEEN 1 AND 3),ordinal INTEGER NOT NULL REFERENCES transcript_rows(ordinal) ON DELETE CASCADE,conversation BLOB NOT NULL CHECK(length(conversation)=32),peer TEXT,charge INTEGER NOT NULL CHECK(charge>0),PRIMARY KEY(tag,id)) WITHOUT ROWID;
";
fn digest(xor: [u8; 32], count: u64) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"mesh-talk-history-digest-v2");
    h.update(xor);
    h.update(count.to_be_bytes());
    h.finalize().into()
}
pub(super) fn empty_history_digest() -> [u8; 32] {
    digest([0; 32], 0)
}
type AccountExportPage = (
    Vec<HistoryRecord>,
    Option<HistoryCursor>,
    [u8; 32],
    (u64, [u8; 32]),
);
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ArchiveKey {
    owner: String,
    history: bool,
    files: bool,
    version: u16,
    revisions: [u64; 11],
    channels: Vec<ConversationId>,
    accounts: Vec<String>,
    controls: (u64, [u8; 32]),
    recalls: u64,
}
impl ArchiveKey {
    fn accepts_append_only_change(&self, current: &Self) -> bool {
        self.owner == current.owner
            && self.history == current.history
            && self.files == current.files
            && self.version == current.version
            && self.channels == current.channels
            && self.accounts == current.accounts
            && self.controls == current.controls
            && self.recalls == current.recalls
            && self.revisions[3..] == current.revisions[3..]
    }
    fn token(&self) -> Result<Vec<u8>, LogError> {
        bincode::serialize(self).map_err(|_| LogError::CorruptFile("invalid archive key".into()))
    }
}
#[derive(Default)]
pub(in crate::node) struct ArchiveCache {
    initialized: bool,
    #[cfg(test)]
    pub(super) builds: usize,
    #[cfg(test)]
    pub(super) scanned_rows: usize,
}
#[derive(Clone)]
struct ArchiveRow {
    key: HistoryCursor,
    kind: i64,
    ordinal: i64,
    conversation: ConversationId,
    peer: Option<String>,
    charge: usize,
}
impl Node {
    fn archive_key(
        &self,
        requester: &PublicIdentity,
        history: bool,
        files: bool,
        version: AccountSyncVersion,
    ) -> Result<ArchiveKey, NodeError> {
        let me = self.user_id();
        let requester_id = requester.user_id();
        let mut channels = {
            let book = self.channels.lock().expect("channels mutex");
            book.channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id).is_some_and(|s| {
                        !s.is_dissolved()
                            && s.is_accepted_member(&me)
                            && s.is_accepted_member(&requester_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        channels.sort_by_key(|id| *id.as_bytes());
        let sent = self.sentlog.lock().expect("sent mutex");
        let received = self.received.lock().expect("received mutex");
        let file = self.received_files.lock().expect("files mutex");
        let revisions = [
            sent.revision(),
            received.revision(),
            file.revision(),
            self.device_sync_policy_snapshot().1,
            self.account_suppression
                .lock()
                .expect("suppression mutex")
                .len()? as u64,
            sent.rewrite_revision(),
            received.rewrite_revision(),
            file.rewrite_revision(),
            self.recovered_sent_ids
                .lock()
                .expect("recovered mutex")
                .as_ref()
                .map(|r| r.revision())
                .transpose()?
                .unwrap_or(0),
            self.file_message_identities
                .lock()
                .expect("identities mutex")
                .revision()?,
            self.account_file_proofs
                .lock()
                .expect("proofs mutex")
                .revision()?,
        ];
        drop(file);
        drop(received);
        drop(sent);
        Ok(ArchiveKey {
            owner: self.account_id(),
            history,
            files,
            version: version.number(),
            revisions,
            channels,
            accounts: if files {
                self.known_account_ids()?
            } else {
                Vec::new()
            },
            controls: self.private_dm_control_revision(),
            recalls: self.recalls.lock().expect("recalls mutex").revision(),
        })
    }
    fn initialize_archive(&self) -> Result<(), LogError> {
        self.storage.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='account_archive_state')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
        })
    }
    fn reset_archive(&self, token: &[u8]) -> Result<(), LogError> {
        // Retiring an interrupted derived build never holds the full selector set in memory.
        loop {
            let n=self.storage.database().transaction(|c|Ok(c.execute("DELETE FROM account_archive_rows WHERE (tag,id) IN(SELECT tag,id FROM account_archive_rows ORDER BY tag,id LIMIT 128)",[])?))?;
            if n == 0 {
                break;
            }
        }
        self.storage.database().transaction(|c|{c.execute("INSERT OR REPLACE INTO account_archive_state VALUES(1,?1,0,zeroblob(32),0,0,0,0,NULL)",[token])?;Ok(())})
    }
    fn offer_archive_row(&self, row: &ArchiveRow, deadline: Option<u64>) -> Result<(), LogError> {
        self.storage.database().transaction(|c|{
        let inserted=c.execute("INSERT OR IGNORE INTO account_archive_rows VALUES(?1,?2,?3,?4,?5,?6,?7)",params![row.key.tag,row.key.id.as_slice(),row.kind,row.ordinal,row.conversation.as_bytes().as_slice(),row.peer,row.charge as i64])?;
        if inserted>0 {let mut xor:[u8;32]=c.query_row("SELECT digest_xor FROM account_archive_state WHERE singleton=1",[],|r|r.get::<_,Vec<u8>>(0))?.try_into().map_err(|_|LogError::CorruptFile("invalid archive digest".into()))?;let mut hash=Sha256::new();hash.update(b"mesh-talk-history-record-v2");hash.update([row.key.tag]);hash.update(row.key.id);for(a,b)in xor.iter_mut().zip(hash.finalize()){*a^=b;}let deadline=deadline.map(u64::to_be_bytes);c.execute("UPDATE account_archive_state SET digest_xor=?1,records=records+1,deadline=CASE WHEN ?2 IS NOT NULL AND (deadline IS NULL OR ?2<deadline) THEN ?2 ELSE deadline END WHERE singleton=1",params![xor.as_slice(),deadline.as_ref().map(|d|d.as_slice())])?;}Ok(())
    })
    }
    fn build_archive_index(
        &self,
        key: &ArchiveKey,
        cache: &mut ArchiveCache,
    ) -> Result<(), NodeError> {
        let own = self.account_id();
        let mut hosts: HashMap<_, Option<String>> =
            key.channels.iter().map(|id| (*id, None)).collect();
        if key.version == 2 {
            hosts.insert(account_conversation_id(&own, &own), Some(own.clone()));
        }
        for account in &key.accounts {
            hosts.insert(
                account_conversation_id(&own, account),
                Some(account.clone()),
            );
        }
        for (kind, column) in [
            (SENT, "sent_cursor"),
            (RECEIVED, "received_cursor"),
            (FILES, "file_cursor"),
        ] {
            if (kind == FILES && !key.files) || (kind != FILES && !key.history) {
                continue;
            }
            let mut after = self.storage.database().read(|c| {
                Ok(c.query_row(
                    &format!("SELECT {column} FROM account_archive_state WHERE singleton=1"),
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })?;
            loop {
                let page = match kind {
                    SENT => self.sentlog.lock().expect("sent mutex").header_scan_page(
                        None,
                        Some(after),
                        TranscriptPageLimit::default(),
                    )?,
                    RECEIVED => self
                        .received
                        .lock()
                        .expect("received mutex")
                        .header_scan_page(None, Some(after), TranscriptPageLimit::default())?,
                    _ => self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .header_scan_page(None, Some(after), TranscriptPageLimit::default())?,
                };
                if page.rows.is_empty() {
                    break;
                }
                for header in page.rows {
                    after = header.ordinal;
                    #[cfg(test)]
                    {
                        cache.scanned_rows += 1;
                    }
                    if header.body_bytes == 0 || header.body_bytes > MAX_RECORD {
                        continue;
                    }
                    let mut row = ArchiveRow {
                        key: HistoryCursor {
                            tag: 0,
                            id: [0; 32],
                        },
                        kind,
                        ordinal: header.ordinal,
                        conversation: header.conversation,
                        peer: None,
                        charge: header
                            .body_bytes
                            .saturating_add(header.sender_bytes)
                            .saturating_mul(3)
                            .saturating_add(1024),
                    };
                    if kind == FILES {
                        row.charge = MAX_RECORD * 3 + 32 * 1024;
                        let Some(peer) = hosts.get(&header.conversation) else {
                            continue;
                        };
                        row.peer = peer.clone();
                        row.key.tag = 2;
                        row.key.id =
                            header.key.as_slice().try_into().map_err(|_| {
                                LogError::CorruptFile("invalid file selector".into())
                            })?;
                    } else if let Some(id) = header.logical_id {
                        row.key.id = *id.as_bytes();
                    } else if key.channels.contains(&header.conversation) {
                        row.key.tag = 1;
                        row.key.id = if kind == SENT {
                            *self.sent_header_identity(&header)?.as_bytes()
                        } else {
                            header.key.as_slice().try_into().map_err(|_| {
                                LogError::CorruptFile("invalid received selector".into())
                            })?
                        };
                        if row.key.id == [0; 32] {
                            continue;
                        }
                    } else {
                        continue;
                    }
                    if self.materialize_archive_row(&row)?.is_some() {
                        self.offer_archive_row(&row, header.deadline)?;
                    }
                }
                self.storage.database().transaction(|c| {
                    c.execute(
                        &format!("UPDATE account_archive_state SET {column}=?1 WHERE singleton=1"),
                        [after],
                    )?;
                    Ok(())
                })?;
            }
        }
        #[cfg(not(test))]
        let _ = cache;
        Ok(())
    }
    pub(super) fn prepare_account_export(
        &self,
        requester: &PublicIdentity,
        after: Option<HistoryCursor>,
        history: bool,
        files: bool,
        version: AccountSyncVersion,
    ) -> Result<AccountExportPage, NodeError> {
        let mut cache = self
            .account_archive_cache
            .lock()
            .expect("archive cache mutex");
        if !cache.initialized {
            self.initialize_archive()?;
            cache.initialized = true;
        }
        let key = self.archive_key(requester, history, files, version)?;
        let token = key.token()?;
        let previous = self.storage.database().read(|c| {
            Ok(c.query_row(
                "SELECT token,ready,deadline FROM account_archive_state WHERE singleton=1",
                [],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, bool>(1)?,
                        r.get::<_, Option<Vec<u8>>>(2)?,
                    ))
                },
            )
            .optional()?)
        })?;
        let fresh = previous.as_ref().is_some_and(|(old, ready, deadline)| {
            *ready
                && old == &token
                && deadline
                    .as_ref()
                    .is_none_or(|d| d.as_slice() > self.expiry_now().to_be_bytes().as_slice())
        });
        if !fresh {
            let incremental = previous.as_ref().is_some_and(|(old, ready, deadline)| {
                *ready
                    && deadline
                        .as_ref()
                        .is_none_or(|d| d.as_slice() > self.expiry_now().to_be_bytes().as_slice())
                    && bincode::deserialize::<ArchiveKey>(old)
                        .is_ok_and(|old| old.accepts_append_only_change(&key))
            });
            if !incremental {
                self.reset_archive(&token)?;
            } else {
                self.storage.database().transaction(|c| {
                    c.execute(
                        "UPDATE account_archive_state SET token=?1,ready=0 WHERE singleton=1",
                        [token.as_slice()],
                    )?;
                    Ok(())
                })?;
            }
            self.build_archive_index(&key, &mut cache)?;
            if !key
                .accepts_append_only_change(&self.archive_key(requester, history, files, version)?)
            {
                return Err(NodeError::Channel(
                    "account archive changed while indexing".into(),
                ));
            }
            self.storage.database().transaction(|c| {
                c.execute(
                    "UPDATE account_archive_state SET ready=1 WHERE singleton=1",
                    [],
                )?;
                Ok(())
            })?;
            #[cfg(test)]
            {
                cache.builds += 1;
            }
        }
        let (selected,more)=self.storage.database().read(|c|{
            let mut statement=c.prepare("SELECT tag,id,kind,ordinal,conversation,peer,charge FROM account_archive_rows WHERE (?1 IS NULL OR (tag,id)>(?1,?2)) ORDER BY tag,id LIMIT ?3")?;
            let mut rows=statement.query(params![after.map(|a|a.tag),after.as_ref().map(|a|a.id.as_slice()),RECORDS_PER_PAGE as i64+1])?;let mut selected=Vec::new();let mut bytes=0usize;let mut more=false;
            while let Some(r)=rows.next()?{let charge=usize::try_from(r.get::<_,i64>(6)?).map_err(|_|LogError::InvalidPageBudget)?;if selected.len()==RECORDS_PER_PAGE||bytes.checked_add(charge).is_none_or(|n|n>8*1024*1024){if selected.is_empty(){return Err(LogError::PageBudgetTooSmall{required:charge});}more=true;break;}bytes+=charge;let id:Vec<u8>=r.get(1)?;let conv:Vec<u8>=r.get(4)?;selected.push(ArchiveRow{key:HistoryCursor{tag:r.get(0)?,id:id.try_into().map_err(|_|LogError::CorruptFile("invalid archive id".into()))?},kind:r.get(2)?,ordinal:r.get(3)?,conversation:ConversationId::new(conv.try_into().map_err(|_|LogError::CorruptFile("invalid archive host".into()))?),peer:r.get(5)?,charge});}Ok((selected,more))
        })?;
        let next = more.then(|| selected.last().expect("nonempty bounded archive page").key);
        let mut records = Vec::new();
        for row in selected.iter().take(RECORDS_PER_PAGE) {
            if let Some(record) = self.materialize_archive_row(row)? {
                records.push(record);
            }
        }
        if !key.accepts_append_only_change(&self.archive_key(requester, history, files, version)?) {
            return Err(NodeError::Channel(
                "account archive changed while selecting page".into(),
            ));
        }
        let archive_digest = self.storage.database().read(|c| {
            let (xor, count): (Vec<u8>, i64) = c.query_row(
                "SELECT digest_xor,records FROM account_archive_state WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(digest(
                xor.try_into()
                    .map_err(|_| LogError::CorruptFile("invalid archive digest".into()))?,
                u64::try_from(count)
                    .map_err(|_| LogError::CorruptFile("invalid archive count".into()))?,
            ))
        })?;
        Ok((records, next, archive_digest, key.controls))
    }
    fn materialize_archive_row(
        &self,
        row: &ArchiveRow,
    ) -> Result<Option<HistoryRecord>, NodeError> {
        let stale = || NodeError::Channel("account archive selector invalidated".into());
        let target = EventId::new(row.key.id);
        if self
            .account_suppression
            .lock()
            .expect("suppression mutex")
            .contains(row.conversation, target)?
        {
            return Ok(None);
        }
        if row.kind == FILES {
            let entry = self
                .received_files
                .lock()
                .expect("files mutex")
                .row_by_ordinal(row.ordinal)?
                .ok_or_else(stale)?;
            if entry.conversation != row.conversation || entry.event_id != target {
                return Err(stale());
            }
            {
                let delivery = self.delivery.lock().expect("delivery mutex");
                if delivery.file_erased(entry.conversation, entry.event_id)
                    || delivery.manifest_event_erased(entry.event_id)
                {
                    return Ok(None);
                }
            }
            if self.file_message_row_suppressed(&entry)?
                || self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
            {
                return Ok(None);
            }
            let selected = self.log.lock().expect("log mutex").get(&entry.event_id)?;
            let proof = match selected {
                Some(event) => Some(event),
                None => self
                    .account_file_proofs
                    .lock()
                    .expect("proofs mutex")
                    .get(&entry.event_id)?,
            };
            let Some(proof) = proof.as_ref() else {
                return Ok(None);
            };
            if super::super::files::eligible_manifest_row(&entry, Some(proof), None).is_none() {
                return Ok(None);
            }
            let author = self
                .file_message_identities
                .lock()
                .expect("file identities mutex")
                .row_author_account(&entry)?;
            if self.message_recalled_selected(
                entry.conversation,
                entry.event_id,
                row.peer.as_deref(),
                author.as_deref(),
            )? {
                return Ok(None);
            }
            let payload = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(&FileCardPayloadRef {
                    manifest: &entry.plaintext,
                    proof,
                })
                .map_err(|_| stale())?;
            if payload.len() > MAX_RECORD {
                return Ok(None);
            }
            return Ok(Some(HistoryRecord::FileCard(FileCardRecord {
                host: *entry.conversation.as_bytes(),
                peer_account: row.peer.clone(),
                from: entry.from,
                wall_clock: entry.wall_clock,
                event_id: row.key.id,
                payload,
            })));
        }
        let (conversation, from, wall_clock, plaintext) = if row.kind == SENT {
            let entry = self
                .sentlog
                .lock()
                .expect("sent mutex")
                .row_by_ordinal(row.ordinal)?
                .ok_or_else(stale)?;
            (
                entry.conversation,
                self.user_id(),
                entry.wall_clock,
                entry.plaintext,
            )
        } else {
            let entry = self
                .received
                .lock()
                .expect("received mutex")
                .row_by_ordinal(row.ordinal)?
                .ok_or_else(stale)?;
            if row.key.tag == 1 && entry.event_id != target {
                return Err(stale());
            }
            (
                entry.conversation,
                entry.from,
                entry.wall_clock,
                entry.plaintext,
            )
        };
        if conversation != row.conversation || plaintext.is_empty() || plaintext.len() > MAX_RECORD
        {
            return Ok(None);
        }
        if row.key.tag == 1 {
            if self.recipient_expired(&plaintext, target, wall_clock) {
                return Ok(None);
            }
            return Ok(Some(HistoryRecord::Channel(ChannelRecord {
                conversation: *conversation.as_bytes(),
                from,
                wall_clock,
                plaintext,
                event_id: row.key.id,
            })));
        }
        let Some(env) = DmEnvelope::decode_ref(&plaintext) else {
            return Ok(None);
        };
        let own = self.account_id();
        let peer = if env.route.sender_account == own {
            env.route.recipient_account
        } else if env.route.recipient_account == own {
            env.route.sender_account
        } else {
            return Ok(None);
        };
        if env.msg_id != row.key.id
            || account_conversation_id(&own, peer) != conversation
            || (row.kind == SENT && env.route.sender_account != own)
            || (row.kind == RECEIVED && env.route.sender_account != from)
            || self.recipient_expired(env.body, target, wall_clock)
        {
            return Ok(None);
        }
        if self.message_recalled_selected(
            conversation,
            target,
            Some(peer),
            Some(env.route.sender_account),
        )? {
            return Ok(None);
        }
        Ok(Some(HistoryRecord::Direct(BackfillRecord {
            conv: *conversation.as_bytes(),
            from: env.route.sender_account.to_owned(),
            wall_clock,
            plaintext,
            event_id: row.key.id,
        })))
    }
}

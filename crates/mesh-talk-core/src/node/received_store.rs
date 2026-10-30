//! On-demand received history and file aliases with exact durable identity checks.
use super::{
    received_log::ReceivedEntry,
    transcript_repository::{self as repository, *},
};
use crate::{
    eventlog::{
        LogError,
        event::{ConversationId, EventId},
    },
    storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog},
};
use std::{path::Path, sync::Arc};

pub(crate) struct ReceivedStore {
    repository: TranscriptRepository,
    #[cfg(test)]
    fail_once_after: Option<usize>,
}
fn decode(row: repository::StoredRow) -> Result<ReceivedEntry, LogError> {
    let id = row
        .header
        .key
        .as_slice()
        .try_into()
        .map_err(|_| repository::invalid("invalid received event id"))?;
    if row.header.sequence != 0 {
        return Err(repository::invalid("invalid received sequence"));
    }
    Ok(ReceivedEntry {
        event_id: EventId::new(id),
        conversation: row.header.conversation,
        from: row.sender,
        wall_clock: row.header.wall_clock,
        plaintext: row.plaintext,
    })
}
impl ReceivedStore {
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        Self::open(profile, path, password, false)
    }
    pub(crate) fn file_cards_from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        Self::open(profile, path, password, true)
    }
    fn open(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
        aliases: bool,
    ) -> Result<Self, LogError> {
        let kind = if aliases { FILES } else { RECEIVED };
        let repository =
            TranscriptRepository::open(profile, path, password, kind, |c, path, password| {
                let writer = EncryptedRecordLog::<ReceivedEntry>::open_with_visitor(
                    path,
                    password,
                    b"MTRECV",
                    |entry| {
                        TranscriptRepository::put_on(
                            c,
                            kind,
                            entry.conversation,
                            entry.event_id.as_bytes(),
                            entry.wall_clock,
                            0,
                            &entry.from,
                            &entry.plaintext,
                            aliases,
                            false,
                        )?;
                        Ok(())
                    },
                )?;
                writer.sync()
            })?;
        Ok(Self {
            repository,
            #[cfg(test)]
            fail_once_after: None,
        })
    }
    pub(crate) fn record_durable(&mut self, entry: &ReceivedEntry) -> Result<(), LogError> {
        if self.repository.kind == RECEIVED {
            self.check_write()?;
            let inserted = self.repository.profile.database().transaction(|c| {
                Self::install_received_on(
                    c,
                    entry,
                    None,
                    super::reception_effects::ReceptionKind::DirectMessage,
                )
            })?;
            self.note_channel_commit(usize::from(inserted));
            Ok(())
        } else {
            self.repository.put(
                entry.conversation,
                entry.event_id.as_bytes(),
                entry.wall_clock,
                0,
                &entry.from,
                &entry.plaintext,
                false,
                false,
            )
        }
    }
    pub(crate) fn check_write(&self) -> Result<(), LogError> {
        self.repository.check_write()
    }
    /// Body-free processing evidence for a consumed channel message. The caller
    /// checks this store's publication barrier and commits its receiver state in
    /// the same transaction. This does not change the transcript revision.
    pub(crate) fn install_evidence_on(
        c: &rusqlite::Connection,
        conversation: ConversationId,
        event_id: EventId,
    ) -> Result<(), LogError> {
        c.execute(
            "INSERT OR IGNORE INTO transcript_received_evidence VALUES(?1,?2)",
            rusqlite::params![
                event_id.as_bytes().as_slice(),
                conversation.as_bytes().as_slice()
            ],
        )?;
        Ok(())
    }
    /// The channel owner holds this store and the profile transaction. Check the
    /// source barrier before entering it; update the cache only after COMMIT.
    pub(crate) fn install_channel_on(
        c: &rusqlite::Connection,
        entry: &ReceivedEntry,
        reception: Option<&super::reception_effects::ReceptionMetadata>,
    ) -> Result<bool, LogError> {
        Self::install_received_on(
            c,
            entry,
            reception,
            super::reception_effects::ReceptionKind::ChannelMessage,
        )
    }
    fn install_received_on(
        c: &rusqlite::Connection,
        entry: &ReceivedEntry,
        reception: Option<&super::reception_effects::ReceptionMetadata>,
        reception_kind: super::reception_effects::ReceptionKind,
    ) -> Result<bool, LogError> {
        let logical = super::dm_envelope::DmEnvelope::decode_ref(&entry.plaintext)
            .filter(|env| {
                super::conversation::account_conversation_id(
                    env.route.sender_account,
                    env.route.recipient_account,
                ) == entry.conversation
            })
            .map(|env| env.msg_id);
        let suppressed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_suppression WHERE conversation=?1 AND (id=?2 OR id=?3))",rusqlite::params![entry.conversation.as_bytes().as_slice(),entry.event_id.as_bytes().as_slice(),logical.as_ref().map(|id|id.as_slice())],|r|r.get(0))?;
        if suppressed {
            Self::install_evidence_on(c, entry.conversation, entry.event_id)?;
            return Ok(false);
        }
        let inserted = TranscriptRepository::put_on(
            c,
            RECEIVED,
            entry.conversation,
            entry.event_id.as_bytes(),
            entry.wall_clock,
            0,
            &entry.from,
            &entry.plaintext,
            false,
            false,
        )?;
        if let Some(metadata) = reception {
            super::reception_effects::install_on(
                c,
                super::reception_effects::ReceptionSource {
                    kind: reception_kind,
                    conversation: entry.conversation,
                    event_id: entry.event_id,
                },
                metadata,
            )?;
        }
        Ok(inserted)
    }
    pub(crate) fn note_channel_commit(&mut self, inserted: usize) {
        self.repository.note_committed_inserts(inserted);
    }
    pub(crate) fn record_durable_with_reception(
        &mut self,
        entry: &ReceivedEntry,
        metadata: &super::reception_effects::ReceptionMetadata,
    ) -> Result<(), LogError> {
        if self.repository.kind != RECEIVED {
            return Err(repository::invalid(
                "DM reception requires message transcript owner",
            ));
        }
        self.check_write()?;
        let inserted = self.repository.profile.database().transaction(|c| {
            self.repository.profile.check_participant_account_on(c)?;
            Self::install_received_on(
                c,
                entry,
                Some(metadata),
                super::reception_effects::ReceptionKind::DirectMessage,
            )
        })?;
        self.note_channel_commit(usize::from(inserted));
        Ok(())
    }
    pub(crate) fn record_file_alias_durable(
        &mut self,
        entry: &ReceivedEntry,
    ) -> Result<(), LogError> {
        if self.repository.kind != FILES {
            return Err(repository::invalid("file alias in message store"));
        }
        self.repository.put(
            entry.conversation,
            entry.event_id.as_bytes(),
            entry.wall_clock,
            0,
            &entry.from,
            &entry.plaintext,
            true,
            false,
        )
    }
    /// FILES plaintext, validated eligibility, exact processed evidence and host
    /// reception facts share the existing profile transaction. The journal may
    /// retire its intent only after this returns successfully.
    pub(crate) fn record_file_with_reception(
        &mut self,
        entry: &ReceivedEntry,
        original: Option<&crate::eventlog::Event>,
        metadata: Option<&super::reception_effects::ReceptionMetadata>,
    ) -> Result<(), LogError> {
        if self.repository.kind != FILES {
            return Err(repository::invalid("file reception requires FILES owner"));
        }
        self.check_write()?;
        let inserted = self.repository.profile.database().transaction(|c| {
            self.repository.profile.check_participant_account_on(c)?;
            let stored;
            let original = match original {
                Some(original) => original,
                None => {
                    stored = super::account_file_repository::IndexedAccountFileProofs::get_on(
                        c,
                        &entry.event_id,
                    )?
                    .ok_or_else(|| repository::invalid("file reception has no signed proof"))?;
                    &stored
                }
            };
            Self::install_file_on(c, entry, original, metadata)
        })?;
        self.note_channel_commit(usize::from(inserted));
        Ok(())
    }
    /// The channel owner calls this after its receiver CAS and before batch
    /// progress, all in one transaction. Return value counts newly retained rows.
    pub(crate) fn install_file_on(
        c: &rusqlite::Connection,
        entry: &ReceivedEntry,
        original: &crate::eventlog::Event,
        metadata: Option<&super::reception_effects::ReceptionMetadata>,
    ) -> Result<bool, LogError> {
        if super::files::eligible_manifest_row(entry, Some(original), None).is_none() {
            return Err(LogError::AdmissionRejected(
                "invalid signed file reception".into(),
            ));
        }
        if super::file_message_identity::IndexedFileMessageIdentities::row_suppressed_on(c, entry)?
        {
            super::filebook::indexed::mark_processed_on(c, entry.event_id)?;
            return Ok(false);
        }
        let inserted = TranscriptRepository::put_on(
            c,
            FILES,
            entry.conversation,
            entry.event_id.as_bytes(),
            entry.wall_clock,
            0,
            &entry.from,
            &entry.plaintext,
            true,
            false,
        )?;
        super::filebook::indexed::install_on(c, entry, original)?;
        if let Some(metadata) = metadata {
            super::reception_effects::install_on(
                c,
                super::reception_effects::ReceptionSource {
                    kind: super::reception_effects::ReceptionKind::File,
                    conversation: entry.conversation,
                    event_id: entry.event_id,
                },
                metadata,
            )?;
        }
        Ok(inserted)
    }
    pub(crate) fn record(
        &mut self,
        conversation: ConversationId,
        from: String,
        wall_clock: u64,
        plaintext: &[u8],
        event_id: EventId,
    ) -> Result<(), LogError> {
        #[cfg(test)]
        if let Some(remaining) = self.fail_once_after.as_mut() {
            if *remaining == 0 {
                self.fail_once_after = None;
                return Err(std::io::Error::other("injected receive projection failure").into());
            }
            *remaining -= 1;
        }
        self.repository.put(
            conversation,
            event_id.as_bytes(),
            wall_clock,
            0,
            &from,
            plaintext,
            false,
            true,
        )
    }
    pub(crate) fn contains(&self, id: EventId) -> Result<bool, LogError> {
        self.repository.contains(None, id.as_bytes())
    }
    pub(crate) fn processed(&self, id: EventId) -> Result<bool, LogError> {
        self.repository.profile.database().read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM transcript_received_evidence WHERE id=?1)",
                [id.as_bytes().as_slice()],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn contains_in(
        &self,
        conversation: &ConversationId,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.repository.contains(Some(conversation), id.as_bytes())
    }
    pub(crate) fn account_entry(
        &self,
        conversation: &ConversationId,
        id: EventId,
    ) -> Result<Option<ReceivedEntry>, LogError> {
        self.repository
            .lookup_logical(conversation, id)?
            .map(decode)
            .transpose()
    }
    pub(crate) fn entry(&self, id: EventId) -> Result<Option<ReceivedEntry>, LogError> {
        self.repository
            .lookup(None, id.as_bytes())?
            .map(decode)
            .transpose()
    }
    pub(crate) fn entry_in(
        &self,
        conversation: &ConversationId,
        id: EventId,
    ) -> Result<Option<ReceivedEntry>, LogError> {
        self.repository
            .lookup(Some(conversation), id.as_bytes())?
            .map(decode)
            .transpose()
    }
    pub(crate) fn file_headers(
        &self,
        file: &ConversationId,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        self.repository.file_headers(file, after, limit)
    }
    pub(crate) fn header_by_ordinal(
        &self,
        ordinal: i64,
    ) -> Result<Option<TranscriptHeader>, LogError> {
        self.repository.header_by_ordinal(ordinal)
    }
    pub(crate) fn row_by_ordinal(&self, ordinal: i64) -> Result<Option<ReceivedEntry>, LogError> {
        self.repository
            .row_by_ordinal(ordinal)?
            .map(decode)
            .transpose()
    }
    pub(crate) fn header_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
        descending: bool,
    ) -> Result<TranscriptPage<TranscriptHeader>, LogError> {
        self.repository
            .header_page(conversation, after, limit, descending)
    }
    pub(crate) fn header_reverse_scan_page(
        &self,
        conversation: &ConversationId,
        before: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        self.repository
            .header_reverse_scan_page(conversation, before, limit)
    }
    pub(crate) fn header_scan_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        self.repository.header_scan_page(conversation, after, limit)
    }
    pub(crate) fn account_routes_page(
        &self,
        after: Option<ConversationId>,
        limit: usize,
    ) -> Result<Vec<(ConversationId, Option<super::DmRoute>)>, LogError> {
        self.repository.account_routes_page(after, limit)
    }
    pub(crate) fn conversation_revision(
        &self,
        conversation: &ConversationId,
    ) -> Result<u64, LogError> {
        self.repository.conversation_revision(conversation)
    }
    pub(crate) fn page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptPage<ReceivedEntry>, LogError> {
        let page = self.repository.page(conversation, after, limit)?;
        Ok(TranscriptPage {
            rows: page
                .rows
                .into_iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
            next: page.next,
        })
    }
    /// Newest first; the returned cursor continues strictly into older rows.
    pub(crate) fn recent_page(
        &self,
        conversation: Option<&ConversationId>,
        before: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptPage<ReceivedEntry>, LogError> {
        let page = self.repository.recent_page(conversation, before, limit)?;
        Ok(TranscriptPage {
            rows: page
                .rows
                .into_iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
            next: page.next,
        })
    }
    pub(crate) fn scan_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<ReceivedEntry>, LogError> {
        let page = self.repository.scan_page(conversation, after, limit)?;
        Ok(TranscriptScanPage {
            rows: page
                .rows
                .into_iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
            next: page.next,
        })
    }
    pub(crate) fn visit(
        &self,
        conversation: Option<&ConversationId>,
        mut visit: impl FnMut(&ReceivedEntry) -> Result<bool, LogError>,
    ) -> Result<(), LogError> {
        let mut cursor = None;
        loop {
            let page = self.scan_page(conversation, cursor, TranscriptPageLimit::default())?;
            for row in &page.rows {
                if !visit(row)? {
                    return Ok(());
                }
            }
            let Some(next) = page.next else { return Ok(()) };
            cursor = Some(next);
        }
    }
    pub(crate) fn find(
        &self,
        conversation: Option<&ConversationId>,
        mut predicate: impl FnMut(&ReceivedEntry) -> bool,
    ) -> Result<Option<ReceivedEntry>, LogError> {
        let mut found = None;
        self.visit(conversation, |row| {
            if predicate(row) {
                found = Some(row.clone());
                Ok(false)
            } else {
                Ok(true)
            }
        })?;
        Ok(found)
    }
    pub(crate) fn any(
        &self,
        conversation: Option<&ConversationId>,
        mut predicate: impl FnMut(&ReceivedEntry) -> bool,
    ) -> Result<bool, LogError> {
        let mut found = false;
        self.visit(conversation, |row| {
            found = predicate(row);
            Ok(!found)
        })?;
        Ok(found)
    }
    pub(crate) fn remove_ordinals(&mut self, ordinals: &[i64]) -> Result<usize, LogError> {
        self.repository.remove_ordinals(ordinals)
    }
    pub(crate) fn remove_where(
        &mut self,
        mut predicate: impl FnMut(&ReceivedEntry) -> bool,
    ) -> Result<usize, LogError> {
        self.repository
            .remove_where(|row| Ok(predicate(&decode(row)?)))
    }
    pub(crate) fn count(&self, conversation: Option<&ConversationId>) -> Result<usize, LogError> {
        self.repository.count(conversation)
    }
    pub(crate) fn usage(&self) -> Result<TranscriptUsage, LogError> {
        self.repository.usage()
    }
    pub(crate) fn conversation_page(
        &self,
        after: Option<ConversationId>,
        limit: usize,
    ) -> Result<Vec<ConversationId>, LogError> {
        self.repository.conversation_page(after, limit)
    }
    pub(crate) fn deadline_page(
        &self,
        before: Option<u64>,
        after: Option<TranscriptDeadlineCursor>,
        limit: usize,
    ) -> Result<Vec<TranscriptDeadline>, LogError> {
        self.repository.deadline_page(before, after, limit)
    }
    pub(crate) fn revision(&self) -> u64 {
        self.repository.revision()
    }
    pub(crate) fn rewrite_revision(&self) -> u64 {
        self.repository.rewrite_revision()
    }
    #[cfg(test)]
    pub(crate) fn body_reads(&self) -> usize {
        self.repository.body_reads()
    }
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.repository.fail_appends_for_test(enabled)
    }
    #[cfg(test)]
    pub(crate) fn fail_next_append_rollback_for_test(&mut self, bytes: usize) {
        self.repository.fail_next_append_rollback_for_test(bytes);
    }
    #[cfg(test)]
    pub(crate) fn fail_once_after_for_test(&mut self, successful: usize) {
        self.fail_once_after = Some(successful);
    }
}

// Explicit fixture materialization only. Production callers use owned lookups
// and bounded pages; these compatibility names do not exist in release builds.
#[cfg(test)]
impl ReceivedStore {
    pub(crate) fn entries_ref(&self, conversation: &ConversationId) -> Vec<ReceivedEntry> {
        let mut rows = Vec::new();
        self.visit(Some(conversation), |row| {
            rows.push(row.clone());
            Ok(true)
        })
        .expect("fixture transcript read");
        rows
    }
    pub(crate) fn entries(&self, conversation: &ConversationId) -> Vec<ReceivedEntry> {
        let mut rows = self.entries_ref(conversation);
        rows.sort_by_key(|entry| entry.wall_clock);
        rows
    }
}

#[cfg(test)]
impl ReceivedStore {
    pub(crate) fn conversations(&self) -> Vec<ConversationId> {
        let mut cursor = None;
        let mut result = Vec::new();
        loop {
            let page = self
                .conversation_page(cursor, 128)
                .expect("fixture conversations");
            if page.is_empty() {
                return result;
            }
            cursor = page.last().copied();
            result.extend(page);
        }
    }
}

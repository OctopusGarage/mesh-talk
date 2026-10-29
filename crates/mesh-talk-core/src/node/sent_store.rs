//! On-demand local sent history, owned by the encrypted participant profile.
use super::{
    dm_envelope::DmEnvelope,
    sentlog::SentEntry,
    transcript_repository::{self as repository, *},
};
use crate::{
    eventlog::{LogError, event::ConversationId},
    storage::{profile::context::ProfileStorage, record_log::EncryptedRecordLog},
};
use std::{path::Path, sync::Arc};

pub(crate) struct SentStore {
    repository: TranscriptRepository,
}
fn key(entry: &SentEntry) -> Vec<u8> {
    if let Some(envelope) = DmEnvelope::decode_ref(&entry.plaintext) {
        account_key(envelope.msg_id)
    } else {
        legacy_key(entry.seq)
    }
}
fn legacy_key(seq: u64) -> Vec<u8> {
    let mut key = vec![0];
    key.extend(seq.to_be_bytes());
    key
}
fn account_key(id: [u8; 32]) -> Vec<u8> {
    let mut key = vec![1];
    key.extend(id);
    key
}
fn decode(row: repository::StoredRow) -> Result<SentEntry, LogError> {
    let entry = SentEntry {
        conversation: row.header.conversation,
        seq: row.header.sequence,
        wall_clock: row.header.wall_clock,
        plaintext: row.plaintext,
    };
    if !row.sender.is_empty() || key(&entry) != row.header.key {
        return Err(repository::invalid(
            "sent transcript key disagrees with body",
        ));
    }
    Ok(entry)
}
impl SentStore {
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        let repository =
            TranscriptRepository::open(profile, path, password, SENT, |c, path, password| {
                let writer = EncryptedRecordLog::<SentEntry>::open_with_visitor(
                    path,
                    password,
                    b"MTSENT",
                    |entry| {
                        TranscriptRepository::put_on(
                            c,
                            SENT,
                            entry.conversation,
                            &key(&entry),
                            entry.wall_clock,
                            entry.seq,
                            "",
                            &entry.plaintext,
                            false,
                            false,
                        )?;
                        Ok(())
                    },
                )?;
                writer.sync()
            })?;
        Ok(Self { repository })
    }
    pub(crate) fn record_durable(&mut self, entry: &SentEntry) -> Result<(), LogError> {
        self.repository.put(
            entry.conversation,
            &key(entry),
            entry.wall_clock,
            entry.seq,
            "",
            &entry.plaintext,
            false,
            false,
        )
    }
    pub(crate) fn record(
        &mut self,
        conversation: ConversationId,
        seq: u64,
        wall_clock: u64,
        plaintext: &[u8],
    ) -> Result<(), LogError> {
        self.record_durable(&SentEntry {
            conversation,
            seq,
            wall_clock,
            plaintext: plaintext.to_vec(),
        })
    }
    pub(crate) fn maximum_sequence(&self, conversation: &ConversationId) -> Result<u64, LogError> {
        self.repository.maximum_sequence(conversation)
    }
    pub(crate) fn entry(
        &self,
        conversation: &ConversationId,
        seq: u64,
    ) -> Result<Option<SentEntry>, LogError> {
        self.repository
            .lookup(Some(conversation), &legacy_key(seq))?
            .map(decode)
            .transpose()
    }
    pub(crate) fn account_entry(
        &self,
        conversation: &ConversationId,
        id: [u8; 32],
    ) -> Result<Option<SentEntry>, LogError> {
        self.repository
            .lookup(Some(conversation), &account_key(id))?
            .map(decode)
            .transpose()
    }
    pub(crate) fn header_by_ordinal(
        &self,
        ordinal: i64,
    ) -> Result<Option<TranscriptHeader>, LogError> {
        self.repository.header_by_ordinal(ordinal)
    }
    pub(crate) fn row_by_ordinal(&self, ordinal: i64) -> Result<Option<SentEntry>, LogError> {
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
    ) -> Result<TranscriptPage<SentEntry>, LogError> {
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
    ) -> Result<TranscriptPage<SentEntry>, LogError> {
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
    ) -> Result<TranscriptScanPage<SentEntry>, LogError> {
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
        mut visit: impl FnMut(&SentEntry) -> Result<bool, LogError>,
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
        mut predicate: impl FnMut(&SentEntry) -> bool,
    ) -> Result<Option<SentEntry>, LogError> {
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
        mut predicate: impl FnMut(&SentEntry) -> bool,
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
        mut predicate: impl FnMut(&SentEntry) -> bool,
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
}

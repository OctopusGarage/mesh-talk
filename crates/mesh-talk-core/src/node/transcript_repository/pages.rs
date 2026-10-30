use super::*;

#[cfg(test)]
thread_local! {
    static HEADER_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(super) fn take_header_reads() -> usize {
    HEADER_READS.with(|reads| reads.replace(0))
}

impl Header {
    fn cursor(&self) -> TranscriptCursor {
        TranscriptCursor {
            wall_clock: self.wall_clock,
            sequence: self.sequence,
            ordinal: self.ordinal,
        }
    }
}
fn admit(
    rows: &mut rusqlite::Rows<'_>,
    limit: TranscriptPageLimit,
    bodies: bool,
) -> Result<(Vec<Header>, bool), LogError> {
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        #[cfg(test)]
        HEADER_READS.with(|reads| reads.set(reads.get() + 1));
        let h = header(row)?;
        let charge = if bodies {
            h.body_bytes
                .checked_add(h.sender_bytes)
                .ok_or_else(|| invalid("transcript page byte overflow"))?
        } else {
            256
        };
        if selected.len() == limit.rows
            || bytes
                .checked_add(charge)
                .is_none_or(|total| total > limit.bytes)
        {
            if selected.is_empty() {
                return Err(invalid("first transcript row exceeds page byte budget"));
            }
            return Ok((selected, true));
        }
        bytes += charge;
        selected.push(h);
    }
    Ok((selected, false))
}
impl TranscriptRepository {
    pub(in crate::node) fn page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptPage<StoredRow>, LogError> {
        self.history_page(conversation, after, limit, false)
    }
    pub(in crate::node) fn recent_page(
        &self,
        conversation: Option<&ConversationId>,
        before: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptPage<StoredRow>, LogError> {
        self.history_page(conversation, before, limit, true)
    }
    fn history_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
        descending: bool,
    ) -> Result<TranscriptPage<StoredRow>, LogError> {
        limit.sql_limit()?;
        self.profile.database().read(|c| {
            let page = self.select_history_on(c, conversation, after, limit, descending, true)?;
            Ok(TranscriptPage {
                rows: page
                    .rows
                    .into_iter()
                    .map(|h| self.load_on(c, h))
                    .collect::<Result<_, _>>()?,
                next: page.next,
            })
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn select_history_on(
        &self,
        c: &Connection,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
        descending: bool,
        bodies: bool,
    ) -> Result<TranscriptPage<Header>, LogError> {
        let sql_limit = limit.sql_limit()?;
        let cursor = after.map(|v| {
            (
                v.wall_clock.to_be_bytes(),
                v.sequence.to_be_bytes(),
                v.ordinal,
            )
        });
        let host = if conversation.is_some() {
            " AND conversation=?2"
        } else {
            " AND ?2 IS NULL"
        };
        let seek = match (after.is_some(), descending) {
            (false, _) => "",
            (true, false) => " AND (clock,sequence,ordinal)>(?3,?4,?5)",
            (true, true) => " AND (clock,sequence,ordinal)<(?3,?4,?5)",
        };
        let order = if descending {
            "clock DESC,sequence DESC,ordinal DESC"
        } else {
            "clock,sequence,ordinal"
        };
        let query = format!(
            "SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1{host}{seek} ORDER BY {order} LIMIT ?6"
        );
        let mut statement = c.prepare(&query)?;
        let mut rows = statement.query(params![
            self.kind,
            conversation.map(|id| id.as_bytes().as_slice()),
            cursor.as_ref().map(|v| v.0.as_slice()),
            cursor.as_ref().map(|v| v.1.as_slice()),
            cursor.as_ref().map(|v| v.2),
            sql_limit
        ])?;
        let (headers, more) = admit(&mut rows, limit, bodies)?;
        let next = if more {
            headers.last().map(Header::cursor)
        } else {
            None
        };
        Ok(TranscriptPage {
            rows: headers,
            next,
        })
    }
    pub(super) fn select_scan_on(
        &self,
        c: &Connection,
        conversation: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<(Vec<Header>, bool), LogError> {
        let sql_limit = limit.sql_limit()?;
        let host = if conversation.is_some() {
            " AND conversation=?2"
        } else {
            " AND ?2 IS NULL"
        };
        let query = format!(
            "SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1{host} AND ordinal>?3 ORDER BY ordinal LIMIT ?4"
        );
        let mut statement = c.prepare(&query)?;
        let mut rows = statement.query(params![
            self.kind,
            conversation.map(|id| id.as_bytes().as_slice()),
            after.unwrap_or(0),
            sql_limit
        ])?;
        admit(&mut rows, limit, true)
    }
    /// Metadata-only page; each fixed-width header is charged 256 bytes.
    pub(crate) fn header_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<TranscriptCursor>,
        limit: TranscriptPageLimit,
        descending: bool,
    ) -> Result<TranscriptPage<TranscriptHeader>, LogError> {
        self.profile
            .database()
            .read(|c| self.select_history_on(c, conversation, after, limit, descending, false))
    }
    pub(crate) fn header_reverse_scan_page(
        &self,
        conversation: &ConversationId,
        before: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        let sql_limit = limit.sql_limit()?;
        self.profile.database().read(|c|{let mut statement=c.prepare(&format!("SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND conversation=?2 AND ordinal<?3 ORDER BY ordinal DESC LIMIT ?4"))?;let mut rows=statement.query(params![self.kind,conversation.as_bytes().as_slice(),before.unwrap_or(i64::MAX),sql_limit])?;let(rows,more)=admit(&mut rows,limit,false)?;let next=if more{rows.last().map(|h|h.ordinal)}else{None};Ok(TranscriptScanPage{rows,next})})
    }
    pub(crate) fn header_scan_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        self.select_metadata_page(conversation, None, after, limit)
    }
    pub(crate) fn file_headers(
        &self,
        file: &ConversationId,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        if self.kind != FILES {
            return Err(invalid("file selector requires file transcript owner"));
        }
        self.select_metadata_page(None, Some(file), after, limit)
    }
    fn select_metadata_page(
        &self,
        conversation: Option<&ConversationId>,
        file: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<TranscriptHeader>, LogError> {
        let sql_limit = limit.sql_limit()?;
        self.profile.database().read(|c| {
            let host=if conversation.is_some(){" AND conversation=?2"}else{" AND ?2 IS NULL"};
            let file_filter=if file.is_some(){" AND file_conversation=?5"}else{" AND ?5 IS NULL"};
            let mut statement=c.prepare(&format!("SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1{host}{file_filter} AND ordinal>?3 ORDER BY ordinal LIMIT ?4"))?;
            let mut rows=statement.query(params![self.kind,conversation.map(|id|id.as_bytes().as_slice()),after.unwrap_or(0),sql_limit,file.map(|id|id.as_bytes().as_slice())])?;
            let (rows,more)=admit(&mut rows,limit,false)?;
            let next=if more{rows.last().map(|h|h.ordinal)}else{None};
            Ok(TranscriptScanPage{rows,next})
        })
    }
    pub(in crate::node) fn scan_page(
        &self,
        conversation: Option<&ConversationId>,
        after: Option<i64>,
        limit: TranscriptPageLimit,
    ) -> Result<TranscriptScanPage<StoredRow>, LogError> {
        self.profile.database().read(|c| {
            let (headers, more) = self.select_scan_on(c, conversation, after, limit)?;
            let next = if more {
                headers.last().map(|h| h.ordinal)
            } else {
                None
            };
            let entries = headers
                .into_iter()
                .map(|h| self.load_on(c, h))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TranscriptScanPage {
                rows: entries,
                next,
            })
        })
    }
}

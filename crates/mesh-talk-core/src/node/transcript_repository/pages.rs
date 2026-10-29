use super::*;

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
) -> Result<(Vec<Header>, bool), LogError> {
    let mut selected = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let h = header(row)?;
        let charge = h
            .body_bytes
            .checked_add(h.sender_bytes)
            .ok_or_else(|| invalid("transcript page byte overflow"))?;
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
        let sql_limit = limit.sql_limit()?;
        self.profile.database().read(|c| {
            let cursor=after.map(|v|(v.wall_clock.to_be_bytes(),v.sequence.to_be_bytes(),v.ordinal));
            let host = if conversation.is_some() { " AND conversation=?2" } else { " AND ?2 IS NULL" };
            let seek = match (after.is_some(), descending) {
                (false, _) => "",
                (true, false) => " AND (clock,sequence,ordinal)>(?3,?4,?5)",
                (true, true) => " AND (clock,sequence,ordinal)<(?3,?4,?5)",
            };
            let order = if descending { "clock DESC,sequence DESC,ordinal DESC" } else { "clock,sequence,ordinal" };
            let query = format!("SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1{host}{seek} ORDER BY {order} LIMIT ?6");
            let mut statement=c.prepare(&query)?;
            let mut rows=statement.query(params![self.kind,conversation.map(|id|id.as_bytes().as_slice()),cursor.as_ref().map(|v|v.0.as_slice()),cursor.as_ref().map(|v|v.1.as_slice()),cursor.as_ref().map(|v|v.2),sql_limit])?;
            let (headers,more)=admit(&mut rows,limit)?;
            let next=if more {headers.last().map(Header::cursor)}else {None};
            let entries=headers.into_iter().map(|h|self.load_on(c,h)).collect::<Result<Vec<_>,_>>()?;
            Ok(TranscriptPage{rows:entries,next})
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
        admit(&mut rows, limit)
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

//! Authenticated indexed facts used without fetching plaintext at startup.
use super::*;
impl TranscriptRepository {
    pub(in crate::node) fn contains(
        &self,
        conversation: Option<&ConversationId>,
        key: &[u8],
    ) -> Result<bool, LogError> {
        self.profile
            .database()
            .read(|c| Ok(Self::find_header_on(c, self.kind, conversation, key)?.is_some()))
    }
    pub(in crate::node) fn lookup_logical(
        &self,
        conversation: &ConversationId,
        id: EventId,
    ) -> Result<Option<StoredRow>, LogError> {
        self.profile.database().read(|c| {
            let mut statement=c.prepare(&format!("SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND conversation=?2 AND logical_id=?3 ORDER BY ordinal LIMIT 1"))?;
            let mut rows=statement.query(params![self.kind,conversation.as_bytes().as_slice(),id.as_bytes().as_slice()])?;
            let h=rows.next()?.map(header).transpose()?;
            h.map(|h|self.load_on(c,h)).transpose()
        })
    }
    /// Before=None is the conservative recipient restart selection. The caller
    /// classifies sender identity without opening any non-expiring payload.
    pub(in crate::node) fn deadline_page(
        &self,
        before: Option<u64>,
        after: Option<TranscriptDeadlineCursor>,
        limit: usize,
    ) -> Result<Vec<TranscriptDeadline>, LogError> {
        if limit == 0 || limit > MAX_PAGE_ROWS {
            return Err(invalid("deadline page exceeds key budget"));
        }
        self.profile.database().read(|c| {
            let upper=if before.is_some(){" AND deadline<=?2"}else{""};
            let seek=if after.is_some(){" AND (deadline,ordinal)>(?3,?4)"}else{""};
            let query=format!("SELECT {HEADER_COLUMNS},deadline,logical_id FROM transcript_rows WHERE kind=?1 AND deadline IS NOT NULL{upper}{seek} ORDER BY deadline,ordinal LIMIT ?5");
            let mut statement=c.prepare(&query)?;
            let upper=before.map(u64::to_be_bytes);
            let cursor=after.map(|v|(v.deadline.to_be_bytes(),v.ordinal));
            let mut rows=statement.query(params![self.kind,upper.as_ref().map(|v|v.as_slice()),cursor.as_ref().map(|v|v.0.as_slice()),cursor.as_ref().map(|v|v.1),size(limit)?])?;
            let mut selected=Vec::new();let mut bytes=0usize;
            while let Some(row)=rows.next()? {
                let h=header(row)?;
                let charge=h.sender_bytes.checked_add(160).ok_or_else(||invalid("deadline metadata size overflow"))?;
                if bytes.checked_add(charge).is_none_or(|n|n>MAX_PAGE_BYTES) {
                    if selected.is_empty(){return Err(invalid("deadline metadata exceeds byte budget"));}
                    break;
                }
                bytes+=charge;
                let deadline=u64::from_be_bytes(blob(row,7)?);
                let logical_id=match row.get_ref(8)? {rusqlite::types::ValueRef::Null=>None,_=>Some(EventId::new(blob(row,8)?))};
                selected.push((h,deadline,logical_id));
            }
            selected.into_iter().map(|(h,deadline,logical_id)| {
                let mut statement=c.prepare("SELECT sender FROM transcript_rows WHERE ordinal=?1")?;
                let mut rows=statement.query([h.ordinal])?;
                let row=rows.next()?.ok_or_else(||invalid("deadline metadata disappeared"))?;
                let sender=row.get_ref(0)?.as_str().map_err(|_|invalid("invalid deadline sender"))?;
                if sender.len()!=h.sender_bytes {return Err(invalid("deadline sender disagrees with byte count"));}
                let event_id=if self.kind==SENT {None}else {Some(EventId::new(h.key.as_slice().try_into().map_err(|_|invalid("invalid received deadline event id"))?))};
                Ok(TranscriptDeadline{cursor:TranscriptDeadlineCursor{deadline,ordinal:h.ordinal},conversation:h.conversation,event_id,logical_id,sequence:h.sequence,sender:sender.to_owned(),wall_clock:h.wall_clock})
            }).collect()
        })
    }
}

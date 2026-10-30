//! Authenticated indexed facts used without fetching plaintext at startup.
use super::*;
impl TranscriptRepository {
    pub(super) fn refresh_route_on(
        c: &Connection,
        kind: i64,
        conversation: &ConversationId,
    ) -> Result<(), LogError> {
        c.execute("INSERT OR IGNORE INTO transcript_first_routes SELECT kind,conversation,ordinal FROM transcript_rows WHERE kind=?1 AND conversation=?2 AND route_present=1 ORDER BY ordinal LIMIT 1",params![kind,conversation.as_bytes().as_slice()])?;
        Ok(())
    }

    /// First decoded envelope per conversation, preserving legacy route discovery.
    /// The presence flag also retains an invalid first route without hydrating it.
    pub(crate) fn account_routes_page(
        &self,
        after: Option<ConversationId>,
        limit: usize,
    ) -> Result<Vec<(ConversationId, Option<super::super::DmRoute>)>, LogError> {
        if limit == 0 || limit > MAX_PAGE_ROWS {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().read(|c| {
            let mut statement=c.prepare("SELECT k.conversation,r.route_sender,r.route_recipient FROM transcript_first_routes k JOIN transcript_rows r ON r.ordinal=k.ordinal WHERE k.kind=?1 AND k.conversation>?2 ORDER BY k.conversation LIMIT ?3")?;
            let mut rows=statement.query(params![self.kind,after.as_ref().map_or(&[][..],|c|c.as_bytes().as_slice()),size(limit)?])?;
            let mut result=Vec::new();
            while let Some(row)=rows.next()? {
                let sender:Option<String>=row.get(1)?;
                let recipient:Option<String>=row.get(2)?;
                let route=sender.zip(recipient).map(|(sender_account,recipient_account)|super::super::DmRoute{sender_account,recipient_account});
                result.push((ConversationId::new(blob(row,0)?),route));
            }
            Ok(result)
        })
    }
    pub(crate) fn ordinal_for_key(
        &self,
        conversation: ConversationId,
        key: &[u8],
    ) -> Result<Option<i64>, LogError> {
        self.profile.database().read(|c|Ok(c.query_row("SELECT ordinal FROM transcript_rows WHERE kind=?1 AND conversation=?2 AND row_key=?3",params![self.kind,conversation.as_bytes().as_slice(),key],|r|r.get(0)).optional()?))
    }
    pub(crate) fn remove_ordinals(&mut self, ordinals: &[i64]) -> Result<usize, LogError> {
        if ordinals.len() > MAX_PAGE_ROWS || ordinals.iter().any(|n| *n <= 0) {
            return Err(LogError::InvalidPageBudget);
        }
        self.check_write()?;
        let removed=self.profile.database().transaction(|c|{
            let mut removed=0;
            for ordinal in ordinals {
                let row:Option<(Vec<u8>,i64)>=c.query_row("SELECT conversation,body_bytes FROM transcript_rows WHERE kind=?1 AND ordinal=?2",params![self.kind,ordinal],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                let Some((conversation,bytes))=row else{continue;};
                c.execute("DELETE FROM transcript_rows WHERE kind=?1 AND ordinal=?2",params![self.kind,ordinal])?;
                let host=ConversationId::new(conversation.as_slice().try_into().map_err(|_|invalid("invalid erased route host"))?);Self::refresh_route_on(c,self.kind,&host)?;
                c.execute("UPDATE transcript_sources SET record_count=record_count-1,body_bytes=body_bytes-?2 WHERE kind=?1",params![self.kind,bytes])?;
                c.execute("UPDATE transcript_conversations SET record_count=record_count-1,body_bytes=body_bytes-?3,revision=revision+1 WHERE kind=?1 AND conversation=?2",params![self.kind,conversation,bytes])?;
                removed+=1;
            }
            if removed>0{c.execute("UPDATE transcript_sources SET revision=revision+1,rewrite_revision=rewrite_revision+1 WHERE kind=?1",[self.kind])?;}
            Ok(removed)
        })?;
        if removed > 0 {
            self.revision += 1;
            self.rewrite_revision += 1;
        }
        Ok(removed)
    }
    pub(crate) fn maximum_sequence(&self, conversation: &ConversationId) -> Result<u64, LogError> {
        self.profile.database().read(|c|{
            let mut statement=c.prepare("SELECT sequence FROM transcript_rows WHERE kind=?1 AND conversation=?2 ORDER BY sequence DESC LIMIT 1")?;
            let mut rows=statement.query(params![self.kind,conversation.as_bytes().as_slice()])?;
            Ok(rows.next()?.map(|r|blob::<8>(r,0).map(u64::from_be_bytes)).transpose()?.unwrap_or(0))
        })
    }
    pub(crate) fn conversation_revision(
        &self,
        conversation: &ConversationId,
    ) -> Result<u64, LogError> {
        self.profile.database().read(|c| {
            let revision: i64 = c.query_row("SELECT revision FROM transcript_conversations WHERE kind=?1 AND conversation=?2", params![self.kind,conversation.as_bytes().as_slice()], |r|r.get(0)).optional()?.unwrap_or(0);
            u64::try_from(revision).map_err(|_|invalid("invalid conversation revision"))
        })
    }
    pub(crate) fn header_by_ordinal(
        &self,
        ordinal: i64,
    ) -> Result<Option<TranscriptHeader>, LogError> {
        self.profile.database().read(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND ordinal=?2"
            ))?;
            let mut rows = statement.query(params![self.kind, ordinal])?;
            rows.next()?.map(header).transpose()
        })
    }
    pub(in crate::node) fn row_by_ordinal(
        &self,
        ordinal: i64,
    ) -> Result<Option<StoredRow>, LogError> {
        self.profile.database().read(|c| {
            let mut statement = c.prepare(&format!(
                "SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND ordinal=?2"
            ))?;
            let mut rows = statement.query(params![self.kind, ordinal])?;
            rows.next()?
                .map(header)
                .transpose()?
                .map(|h| self.load_on(c, h))
                .transpose()
        })
    }
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
            let query=format!("SELECT {HEADER_COLUMNS} FROM transcript_rows WHERE kind=?1 AND deadline IS NOT NULL{upper}{seek} ORDER BY deadline,ordinal LIMIT ?5");
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
                let deadline=h.deadline.ok_or_else(||invalid("missing selected deadline"))?;
                let logical_id=h.logical_id;
                selected.push((h,deadline,logical_id));
            }
            selected.into_iter().map(|(h,deadline,logical_id)| {
                let mut statement=c.prepare("SELECT sender FROM transcript_rows WHERE ordinal=?1")?;
                let mut rows=statement.query([h.ordinal])?;
                let row=rows.next()?.ok_or_else(||invalid("deadline metadata disappeared"))?;
                let sender=row.get_ref(0)?.as_str().map_err(|_|invalid("invalid deadline sender"))?;
                if sender.len()!=h.sender_bytes {return Err(invalid("deadline sender disagrees with byte count"));}
                let event_id=if self.kind==SENT {None}else {Some(EventId::new(h.key.as_slice().try_into().map_err(|_|invalid("invalid received deadline event id"))?))};
                Ok(TranscriptDeadline{cursor:TranscriptDeadlineCursor{deadline,ordinal:h.ordinal},conversation:h.conversation,event_id,logical_id,sender:sender.to_owned(),wall_clock:h.wall_clock})
            }).collect()
        })
    }
}

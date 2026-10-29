use super::*;
use bincode::Options;
use rusqlite::params;

impl ReceptionEffects {
    /// Snapshot a finite installation prefix. Header lengths admit both raw and
    /// worst-case JSON bytes before the first source body is fetched. Deleted,
    /// suppressed and expired sources still advance the scan cursor; terminal
    /// batch work is independent of its former template body.
    pub(crate) fn page(
        &self,
        after: ReceptionCursor,
        through: ReceptionCursor,
        limit: ReceptionLimit,
        now: u64,
    ) -> Result<ReceptionPage, LogError> {
        limit.validate()?;
        self.profile.database().read(|c| {
            let state=storage::state_on(c,&self.account)?;
            storage::validate_cursor(&state,after)?;
            storage::validate_cursor(&state,through)?;
            if after.sequence > through.sequence { return Err(LogError::StaleCursor); }
            if limit.serialized_bytes < JSON_ENVELOPE_BYTES { return Err(LogError::PageBudgetTooSmall {required:JSON_ENVELOPE_BYTES}); }
            let mut statement=c.prepare("SELECT i.sequence,i.kind,i.conversation,i.event_id,p.metadata,t.ordinal,t.body_bytes,t.sender_bytes,t.clock FROM reception_pending p JOIN reception_identities i ON i.sequence=p.sequence LEFT JOIN transcript_rows t ON i.kind<>4 AND t.kind=CASE WHEN i.kind=3 THEN 3 ELSE 2 END AND t.conversation=i.conversation AND t.row_key=i.event_id AND (t.deadline IS NULL OR t.deadline>?4) AND NOT EXISTS(SELECT 1 FROM transcript_suppression s WHERE s.conversation=t.conversation AND (s.id=t.row_key OR s.id=t.logical_id)) WHERE p.sequence>?1 AND p.sequence<=?2 AND i.account=?5 ORDER BY p.sequence LIMIT ?3")?;
            let mut rows=statement.query(params![integer(after.sequence)?,integer(through.sequence)?,limit.rows as i64,now.to_be_bytes().as_slice(),self.account.as_slice()])?;
            let mut result=ReceptionPage { rows:Vec::new(),next:after,through,complete:false,body_bytes:0,serialized_bound:JSON_ENVELOPE_BYTES };
            let mut examined=0;
            while let Some(row)=rows.next()? {
                let sequence=natural(row.get(0)?)?;
                let kind=ReceptionKind::from_tag(row.get(1)?)?;
                let source=ReceptionSource {kind,conversation:ConversationId::new(blob(row,2)?),event_id:EventId::new(blob(row,3)?)};
                let ordinal:Option<i64>=row.get(5)?;
                if ordinal.is_none() && kind != ReceptionKind::ChannelTerminal {
                    examined+=1; result.next.sequence=sequence; continue;
                }
                let encoded=row.get_ref(4)?.as_blob().map_err(|_|invalid("invalid reception metadata"))?;
                if encoded.len()>MAX_METADATA_BYTES {return Err(invalid("reception metadata exceeds budget"));}
                let metadata:ReceptionMetadata=bincode::DefaultOptions::new().with_fixint_encoding().with_limit(MAX_METADATA_BYTES as u64).reject_trailing_bytes().deserialize(encoded).map_err(|_|invalid("invalid reception metadata"))?;
                metadata.validate(kind)?;
                let body_size=if ordinal.is_some() { usize::try_from(row.get::<_,i64>(6)?).map_err(|_|invalid("invalid reception body length"))? } else {0};
                let sender_size=if ordinal.is_some() { usize::try_from(row.get::<_,i64>(7)?).map_err(|_|invalid("invalid reception sender length"))? } else {0};
                let charge=body_size.checked_add(sender_size).ok_or_else(||invalid("reception size overflow"))?;
                let serialized=charge.checked_add(encoded.len()).and_then(|n|n.checked_mul(6)).and_then(|n|n.checked_add(JSON_ENVELOPE_BYTES)).ok_or_else(||invalid("reception encoded size overflow"))?;
                let next_body=result.body_bytes.checked_add(charge).ok_or_else(||invalid("reception size overflow"))?;
                let next_serialized=result.serialized_bound.checked_add(serialized).ok_or_else(||invalid("reception encoded size overflow"))?;
                if next_body>limit.body_bytes || next_serialized>limit.serialized_bytes {
                    if examined==0 {return Err(LogError::PageBudgetTooSmall {required:if next_body>limit.body_bytes {next_body}else{next_serialized}});}
                    return Ok(result);
                }
                let entry=if kind==ReceptionKind::ChannelTerminal {None} else {
                    let ordinal=ordinal.ok_or_else(||invalid("missing reception source"))?;
                    let wall_clock=u64::from_be_bytes(blob(row,8)?);
                    let mut body_statement=c.prepare("SELECT sender,plaintext FROM transcript_bodies WHERE ordinal=?1")?;
                    let mut body_rows=body_statement.query([ordinal])?;
                    let body=body_rows.next()?.ok_or_else(||invalid("missing reception body"))?;
                    let from=body.get_ref(0)?.as_str().map_err(|_|invalid("invalid reception sender"))?;
                    let plaintext=body.get_ref(1)?.as_blob().map_err(|_|invalid("invalid reception body"))?;
                    if from.len()!=sender_size || plaintext.len()!=body_size {return Err(invalid("reception body disagrees with header"));}
                    #[cfg(test)] self.body_reads.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
                    Some(ReceivedEntry {event_id:source.event_id,conversation:source.conversation,from:from.to_owned(),wall_clock,plaintext:plaintext.to_vec()})
                };
                result.rows.push(ReceptionFact {sequence,source,metadata,entry});
                result.body_bytes=next_body; result.serialized_bound=next_serialized; result.next.sequence=sequence; examined+=1;
            }
            // A fixed scan budget includes hidden rows. An empty follow-up page
            // can finish a prefix without ever collecting its deleted history.
            if examined<limit.rows || result.next.sequence==through.sequence {
                result.next=through; result.complete=true;
            }
            Ok(result)
        })
    }
}

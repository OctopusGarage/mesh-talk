use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConversationMetadata {
    pub conversation: ConversationId,
    pub revision: i64,
    pub count: u64,
    pub stored_bytes: u64,
    pub estimated_bytes: u64,
    pub max_lamport: u64,
    pub channel_v2: bool,
    pub channel_v3: bool,
    pub channel_v4: bool,
}
pub(super) const METADATA_COLUMNS: &str = "conversation,revision,event_count,stored_bytes,max_lamport,v2_count,v3_count,v4_count,estimated_bytes";
pub(super) fn read_metadata(row: &Row<'_>) -> Result<ConversationMetadata, LogError> {
    let count = nonnegative(row.get(2)?)?;
    let v2 = nonnegative(row.get(5)?)?;
    let v3 = nonnegative(row.get(6)?)?;
    let v4 = nonnegative(row.get(7)?)?;
    if v2 > count || v3 > count || v4 > count {
        return Err(corrupt("capability count exceeds event count"));
    }
    Ok(ConversationMetadata {
        conversation: ConversationId::new(blob(row, 0)?),
        revision: nonnegative(row.get(1)?)?,
        count: count as u64,
        stored_bytes: nonnegative(row.get(3)?)? as u64,
        estimated_bytes: nonnegative(row.get(8)?)? as u64,
        max_lamport: u64::from_be_bytes(blob(row, 4)?),
        channel_v2: v2 != 0,
        channel_v3: v3 != 0,
        channel_v4: v4 != 0,
    })
}
/// Physical retained-row charge. Logical hiding must preserve these counters
/// until row reclamation; serialized storage bytes have a different meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DomainChargeMetadata {
    pub event_count: u64,
    pub estimated_bytes: u64,
}
impl EventRepository {
    pub(crate) fn domain_charge_on(
        &self,
        c: &Connection,
    ) -> Result<DomainChargeMetadata, LogError> {
        let (count, bytes): (i64, i64) = c
            .prepare_cached(
                "SELECT event_count,estimated_bytes FROM event_domains WHERE domain=?1",
            )?
            .query_row([self.domain.0.as_slice()], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(DomainChargeMetadata {
            event_count: nonnegative(count)? as u64,
            estimated_bytes: nonnegative(bytes)? as u64,
        })
    }

    pub(crate) fn metadata_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<Option<ConversationMetadata>, LogError> {
        self.require_active_on(c, conv)?;
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {METADATA_COLUMNS} FROM event_conversations WHERE domain=?1 AND conversation=?2"
        ))?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice()
        ])?;
        rows.next()?.map(read_metadata).transpose()
    }
    pub(crate) fn metadata(
        &self,
        conv: &ConversationId,
    ) -> Result<Option<ConversationMetadata>, LogError> {
        self.database.read(|c| self.metadata_on(c, conv))
    }
    pub(crate) fn count(&self, conv: &ConversationId) -> Result<u64, LogError> {
        Ok(self.metadata(conv)?.map_or(0, |m| m.count))
    }
    pub(crate) fn authored_header(
        &self,
        conv: &ConversationId,
        author: Author,
        seq: u64,
    ) -> Result<Option<EventHeader>, LogError> {
        self.database.read(|c|{
            self.require_active_on(c,conv)?;
            let mut statement=c.prepare(&format!("SELECT {HEADER_COLUMNS} FROM retained_events WHERE domain=?1 AND conversation=?2 AND author=?3 AND seq=?4"))?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),author.ed25519_pub().as_slice(),seq.to_be_bytes().as_slice()])?;
            rows.next()?.map(read_header).transpose()
        })
    }
    pub(crate) fn author_max(
        &self,
        conv: &ConversationId,
        author: &Author,
    ) -> Result<Option<u64>, LogError> {
        self.database.read(|c| {
            self.require_active_on(c, conv)?;
            let mut stmt=c.prepare("SELECT max_seq FROM event_author_max WHERE domain=?1 AND conversation=?2 AND author=?3")?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),author.ed25519_pub().as_slice()])?;
            rows.next()?.map(|row|Ok(u64::from_be_bytes(blob(row,0)?))).transpose()
        })
    }
    pub(crate) fn next_sequence(
        &self,
        conv: &ConversationId,
        author: &Author,
    ) -> Result<u64, LogError> {
        self.author_max(conv, author)?
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(LogError::SequenceExhausted)
    }
    pub(crate) fn authored_event(
        &self,
        conv: &ConversationId,
        author: &Author,
        seq: u64,
    ) -> Result<Option<Event>, LogError> {
        self.database.read(|c| {
            self.require_active_on(c, conv)?;
            let mut stmt=c.prepare("SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 AND author=?3 AND seq=?4")?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),author.ed25519_pub().as_slice(),seq.to_be_bytes().as_slice()])?;
            let id=rows.next()?.map(|row|blob(row,0).map(EventId::new)).transpose()?;
            id.map(|id|self.get_on(c,&id)?.ok_or_else(||corrupt("authored event disappeared"))).transpose()
        })
    }
    /// Collecting compatibility operation: memory is proportional to frontier
    /// cardinality. Bounded consumers use heads_page instead.
    pub(crate) fn heads(&self, conv: &ConversationId) -> Result<Vec<EventId>, LogError> {
        self.database.read(|c| self.heads_on(c, conv))
    }
    fn heads_on(&self, c: &Connection, conv: &ConversationId) -> Result<Vec<EventId>, LogError> {
        self.require_active_on(c, conv)?;
        let mut stmt = c.prepare(
            "SELECT id FROM event_heads WHERE domain=?1 AND conversation=?2 ORDER BY id",
        )?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice()
        ])?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next()? {
            ids.push(EventId::new(blob(row, 0)?));
        }
        Ok(ids)
    }
    /// Complete signed parents inherently allocate the complete current frontier.
    /// A caller preparing a local event must account for that single-event cost.
    pub(crate) fn prepare(&self, conv: &ConversationId) -> Result<(Vec<EventId>, u64), LogError> {
        self.database.read(|c| {
            let next = self
                .metadata_on(c, conv)?
                .map_or(0, |m| m.max_lamport)
                .checked_add(1)
                .ok_or(LogError::ClockExhausted)?;
            Ok((self.heads_on(c, conv)?, next))
        })
    }
    /// SHA256(countLE || IDs sorted by ID), unchanged from the wire owner.
    /// First call after mutation scans O(N) indexed IDs; unchanged reopen uses
    /// the persisted revision cache. Neither path reads any event body.
    pub(crate) fn fingerprint_and_len(
        &self,
        conv: &ConversationId,
    ) -> Result<([u8; 32], u64), LogError> {
        self.database.transaction(|tx| {
            let Some(metadata)=self.metadata_on(tx,conv)? else {
                return Ok((Sha256::digest(0u64.to_le_bytes()).into(),0));
            };
            let (cached_revision,cached):(Option<i64>,Option<Vec<u8>>)=tx.query_row("SELECT fingerprint_revision,fingerprint FROM event_conversations WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            match (cached_revision,cached) {
                (Some(revision),Some(digest)) => {
                    nonnegative(revision)?;
                    let digest:[u8;32]=digest.try_into().map_err(|_|corrupt("invalid persisted fingerprint"))?;
                    if revision>metadata.revision {return Err(corrupt("fingerprint revision exceeds conversation"));}
                    if revision==metadata.revision {return Ok((digest,metadata.count));}
                }
                (None,None)=>{},
                _=>return Err(corrupt("incomplete persisted fingerprint")),
            }
            let mut hasher=Sha256::new();hasher.update(metadata.count.to_le_bytes());
            let mut stmt=tx.prepare("SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 ORDER BY id")?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()])?;
            let mut count=0u64;
            while let Some(row)=rows.next()? {
                hasher.update(blob::<32>(row,0)?);count=count.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
                #[cfg(test)] self.fingerprint_ids.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
            }
            if count!=metadata.count {return Err(corrupt("fingerprint event count disagrees"));}
            let digest:[u8;32]=hasher.finalize().into();
            tx.execute("UPDATE event_conversations SET fingerprint_revision=?3,fingerprint=?4 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),metadata.revision,digest.as_slice()])?;
            Ok((digest,count))
        })
    }
    /// One already caller-bounded batch for a new conversation. No all-history
    /// validator or shadow log: earlier inserts are visible inside this SQL tx.
    pub(crate) fn append_channel_batch_atomic(&self, events: &[Event]) -> Result<(), LogError> {
        let Some(first) = events.first() else {
            return Ok(());
        };
        self.database.transaction(|tx| {
            if self
                .metadata_on(tx, &first.conversation_id)?
                .is_some_and(|m| m.count > 0)
            {
                return Err(LogError::AdmissionRejected("channel already exists".into()));
            }
            for event in events {
                if event.conversation_id != first.conversation_id {
                    return Err(LogError::AdmissionRejected(
                        "channel batch spans conversations".into(),
                    ));
                }
                self.append_on(tx, event)?;
            }
            Ok(())
        })
    }
}

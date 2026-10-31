//! Reuse the published combined prefix only for proven append-only inputs.
//! Reordering, visibility changes and interrupted work retain the rebuild path.
use super::*;

const INPUT_SCHEMA: &str = "
CREATE TABLE history_order_inputs(job BLOB PRIMARY KEY CHECK(length(job)=16),stable BLOB NOT NULL CHECK(length(stable)<=4096),sent_after INTEGER NOT NULL CHECK(sent_after>=0),received_after INTEGER NOT NULL CHECK(received_after>=0),files_after INTEGER NOT NULL CHECK(files_after>=0),maximum_clock BLOB NOT NULL CHECK(length(maximum_clock)=8),maximum_id BLOB NOT NULL CHECK(length(maximum_id)=32),event_after INTEGER NOT NULL CHECK(event_after>=-1),sent_sequence BLOB NOT NULL CHECK(length(sent_sequence)=8)) WITHOUT ROWID;
";

const STAMP_MAGIC: &[u8; 8] = b"MTHIST01";

#[derive(Clone, Copy)]
pub(crate) struct HistoryInputStamp<'a> {
    pub visibility: &'a [u8],
    pub file_offers: u64,
    pub file_identities: u64,
}

impl HistoryInputStamp<'_> {
    fn encode(self) -> Result<Vec<u8>, LogError> {
        if self.visibility.len() > 4096 - STAMP_MAGIC.len() - 16 {
            return Err(LogError::InvalidPageBudget);
        }
        let mut bytes = STAMP_MAGIC.to_vec();
        bytes.extend(self.file_offers.to_be_bytes());
        bytes.extend(self.file_identities.to_be_bytes());
        bytes.extend(self.visibility);
        Ok(bytes)
    }
    fn decode(bytes: &[u8]) -> Result<Option<HistoryInputStamp<'_>>, LogError> {
        if !bytes.starts_with(STAMP_MAGIC) {
            return Ok(None);
        }
        if bytes.len() < STAMP_MAGIC.len() + 16 {
            return Err(corrupt("truncated history input stamp"));
        }
        Ok(Some(HistoryInputStamp {
            visibility: &bytes[24..],
            file_offers: u64::from_be_bytes(bytes[8..16].try_into().expect("checked stamp width")),
            file_identities: u64::from_be_bytes(
                bytes[16..24].try_into().expect("checked stamp width"),
            ),
        }))
    }
}

#[derive(Clone, Copy)]
pub(crate) struct HistoryAppendPrefix {
    pub after: [i64; 3],
    pub file_offers: u64,
    pub file_identities: u64,
    maximum: (u64, EventId),
    signed_rank: i64,
}

impl EventRepository {
    pub(super) fn initialize_history_inputs(&self) -> Result<(), LogError> {
        self.database.transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='history_order_inputs')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(INPUT_SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, INPUT_SCHEMA)
        })
    }

    pub(crate) fn history_append_generation(
        &self,
        conversation: &ConversationId,
        token: &[u8],
        stamp: HistoryInputStamp<'_>,
        now: u64,
        signed: Option<OrderGeneration>,
        legacy_author: Option<Author>,
    ) -> Result<Option<(HistoryGeneration, HistoryAppendPrefix)>, LogError> {
        if token.len() > 4096 || stamp.visibility.len() > 4096 - 24 {
            return Err(LogError::InvalidPageBudget);
        }
        self.database.transaction(|c| {
            let mut statement = c.prepare("SELECT j.job,j.ready,j.token,j.next_deadline,j.next_rank,j.signed_job,j.signed_rank,i.stable,i.sent_after,i.received_after,i.files_after,i.maximum_clock,i.maximum_id,i.event_after,i.sent_sequence FROM history_order_jobs j JOIN history_order_inputs i ON i.job=j.job WHERE j.domain=?1 AND j.conversation=?2")?;
            let mut rows = statement.query(params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()])?;
            let Some(row) = rows.next()? else { return Ok(None); };
            let job = blob::<16>(row,0)?;
            let ready: bool = row.get(1)?;
            let previous: Vec<u8> = row.get(2)?;
            let deadline: Option<Vec<u8>> = row.get(3)?;
            let count = usize::try_from(nonnegative(row.get(4)?)?).map_err(|_| LogError::RepositoryCounterExhausted)?;
            let old_signed: Option<Vec<u8>> = row.get(5)?;
            let signed_rank: i64 = row.get(6)?;
            let old_stable: Vec<u8> = row.get(7)?;
            let Some(old_stamp) = HistoryInputStamp::decode(&old_stable)? else { return Ok(None); };
            let prefix = HistoryAppendPrefix {file_offers:old_stamp.file_offers,file_identities:old_stamp.file_identities,after:[row.get(8)?,row.get(9)?,row.get(10)?],maximum:(u64::from_be_bytes(blob(row,11)?),EventId::new(blob(row,12)?)),signed_rank};
            let event_after: i64 = row.get(13)?;
            let sent_sequence = blob::<8>(row,14)?;
            let deadline = deadline.as_deref().map(|v|v.try_into().map(u64::from_be_bytes).map_err(|_|corrupt("invalid history deadline"))).transpose()?;
            if !ready || old_stamp.visibility != stamp.visibility || old_stamp.file_offers > stamp.file_offers || old_stamp.file_identities > stamp.file_identities || deadline.is_some_and(|d|d<=now) || old_signed.as_deref()!=signed.as_ref().map(|s|s.job.as_slice()) { return Ok(None); }
            drop(rows); drop(statement);
            if previous == token { return Ok(Some((HistoryGeneration {job,ready:true,count},prefix))); }
            // A newly arrived original may identify an old sent row or replace
            // an archive root in the signed graph. Neither is an append-only
            // selector change, even if the signed job itself kept its prefix.
            let changes_old: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM retained_events e WHERE e.domain=?1 AND e.conversation=?2 AND e.commit_ordinal>?3 AND ((?4 IS NOT NULL AND e.author=?4 AND e.seq<=?5) OR (?6 AND EXISTS(SELECT 1 FROM history_order_candidates h WHERE h.job=?7 AND h.id=e.id AND h.archive=1))))",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),event_after,legacy_author.as_ref().map(|a|a.ed25519_pub().as_slice()),sent_sequence.as_slice(),signed.is_some(),job.as_slice()],|r|r.get(0))?;
            if changes_old { return Ok(None); }
            if let Some(signed) = signed {
                let first: Option<(Vec<u8>,Vec<u8>)> = c.query_row("SELECT e.wall_clock,e.id FROM event_order_rows r JOIN retained_events e ON e.domain=?1 AND e.id=r.id WHERE r.job=?2 AND r.rank>?3 ORDER BY r.rank LIMIT 1",params![self.domain.0.as_slice(),signed.job.as_slice(),signed_rank],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                if first.is_some_and(|(clock,id)|(clock.as_slice(),id.as_slice())<=(prefix.maximum.0.to_be_bytes().as_slice(),prefix.maximum.1.as_bytes().as_slice())) { return Ok(None); }
            }
            // Publication becomes unavailable before the first candidate edit.
            // A failed append turn can never make the old token appear ready.
            c.execute("UPDATE history_order_jobs SET ready=0,token=?2 WHERE job=?1",params![job.as_slice(),token])?;
            Ok(Some((HistoryGeneration {job,ready:false,count},prefix)))
        })
    }

    pub(crate) fn history_append_candidate(
        &self,
        conversation: &ConversationId,
        job: [u8; 16],
        prefix: HistoryAppendPrefix,
        candidate: HistorySelector,
        deadline: Option<u64>,
    ) -> Result<bool, LogError> {
        let safe = self.database.read(|c| {
            let old: Option<(i64,Vec<u8>,Vec<u8>,i64,bool)> = c.query_row("SELECT priority,clock,sequence,ordinal,archive FROM history_order_candidates WHERE job=?1 AND id=?2",params![job.as_slice(),candidate.id.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
            let signed: Option<Vec<u8>> = c.query_row("SELECT signed_job FROM history_order_jobs WHERE job=?1",[job.as_slice()],|r|r.get(0))?;
            let rank: Option<i64> = if let Some(signed)=&signed {c.query_row("SELECT rank FROM event_order_rows WHERE job=?1 AND id=?2",params![signed,candidate.id.as_bytes().as_slice()],|r|r.get(0)).optional()?} else {None};
            if let Some((priority,clock,sequence,ordinal,archive))=old {
                let new_clock=candidate.clock.to_be_bytes(); let new_sequence=candidate.sequence.to_be_bytes();
                if (candidate.priority,new_clock.as_slice(),new_sequence.as_slice(),candidate.ordinal)>=(priority,clock.as_slice(),sequence.as_slice(),ordinal) { return Ok(true); }
                return Ok(signed.is_some() && !archive && rank.is_some() || clock==new_clock);
            }
            Ok(if let Some(rank)=rank { rank>prefix.signed_rank } else { (candidate.clock,candidate.id)>prefix.maximum })
        })?;
        if safe {
            self.history_add(conversation, job, candidate, deadline)?;
        }
        Ok(safe)
    }

    pub(crate) fn history_finish_inputs(
        &self,
        conversation: &ConversationId,
        job: [u8; 16],
        stamp: HistoryInputStamp<'_>,
        after: [i64; 3],
        sent_sequence: u64,
    ) -> Result<(), LogError> {
        let stable = stamp.encode()?;
        self.database.transaction(|c| {
            let candidate:Option<(Vec<u8>,Vec<u8>)>=c.query_row("SELECT clock,id FROM history_order_candidates WHERE job=?1 ORDER BY clock DESC,id DESC LIMIT 1",[job.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let signed:Option<(Vec<u8>,Vec<u8>)>=c.query_row("SELECT wall_clock,id FROM retained_events WHERE domain=?1 AND conversation=?2 ORDER BY wall_clock DESC,id DESC LIMIT 1",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let maximum = candidate.into_iter().chain(signed).max().unwrap_or((vec![0;8],vec![0;32]));
            let event_after:i64=c.query_row("SELECT COALESCE(MAX(commit_ordinal),-1) FROM retained_events WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()],|r|r.get(0))?;
            c.execute("INSERT INTO history_order_inputs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(job) DO UPDATE SET stable=excluded.stable,sent_after=excluded.sent_after,received_after=excluded.received_after,files_after=excluded.files_after,maximum_clock=excluded.maximum_clock,maximum_id=excluded.maximum_id,event_after=excluded.event_after,sent_sequence=excluded.sent_sequence",params![job.as_slice(),stable,after[0],after[1],after[2],maximum.0,maximum.1,event_after,sent_sequence.to_be_bytes().as_slice()])?;
            Ok(())
        })
    }
}

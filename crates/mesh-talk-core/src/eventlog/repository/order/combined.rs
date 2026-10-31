//! Canonical transcript selectors share the signed Kahn generation. Archive-only
//! roots cannot parent a retained signed event, so merging the current signed
//! head with the least archive root preserves the existing Kahn semantics.
use super::*;
mod incremental;
pub(crate) use incremental::{HistoryAppendPrefix, HistoryInputStamp};
const SCHEMA:&str="
CREATE TABLE history_order_jobs(domain BLOB NOT NULL,conversation BLOB NOT NULL,job BLOB NOT NULL UNIQUE CHECK(length(job)=16),token BLOB NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1)),next_deadline BLOB,signed_job BLOB,signed_rank INTEGER NOT NULL DEFAULT -1,archive_clock BLOB NOT NULL DEFAULT X'',archive_id BLOB NOT NULL DEFAULT X'',next_rank INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(domain,conversation)) WITHOUT ROWID;
CREATE TABLE history_order_candidates(job BLOB NOT NULL,id BLOB NOT NULL CHECK(length(id)=32),clock BLOB NOT NULL CHECK(length(clock)=8),source_kind INTEGER NOT NULL,ordinal INTEGER NOT NULL,priority INTEGER NOT NULL,sequence BLOB NOT NULL CHECK(length(sequence)=8),charged_bytes INTEGER NOT NULL CHECK(charged_bytes>=0),archive INTEGER NOT NULL CHECK(archive IN(0,1)),PRIMARY KEY(job,id)) WITHOUT ROWID;
CREATE INDEX history_candidate_clock ON history_order_candidates(job,clock,id);
CREATE INDEX history_archive_ready ON history_order_candidates(job,archive,clock,id);
CREATE TABLE history_order_rows(job BLOB NOT NULL,rank INTEGER NOT NULL,id BLOB NOT NULL CHECK(length(id)=32),PRIMARY KEY(job,rank),UNIQUE(job,id)) WITHOUT ROWID;
";
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistorySelector {
    pub id: EventId,
    pub clock: u64,
    pub source_kind: i64,
    pub ordinal: i64,
    pub priority: i64,
    pub sequence: u64,
    pub charged_bytes: usize,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct HistoryGeneration {
    pub job: [u8; 16],
    pub ready: bool,
    pub count: usize,
}
impl EventRepository {
    pub(crate) fn initialize_history_order(&self) -> Result<(), LogError> {
        self.database.transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='history_order_jobs')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
        })?;
        self.initialize_history_inputs()
    }
    pub(crate) fn history_generation(
        &self,
        conv: &ConversationId,
        token: &[u8],
        now: u64,
        signed: Option<OrderGeneration>,
    ) -> Result<HistoryGeneration, LogError> {
        if token.len() > 4096 {
            return Err(LogError::InvalidPageBudget);
        }
        let previous=self.database.read(|c|{
            let mut statement=c.prepare("SELECT job,token,ready,next_deadline,next_rank,signed_job FROM history_order_jobs WHERE domain=?1 AND conversation=?2")?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()])?;
            rows.next()?.map(|r|Ok((blob::<16>(r,0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,bool>(2)?,r.get::<_,Option<Vec<u8>>>(3)?,nonnegative(r.get(4)?)?,r.get::<_,Option<Vec<u8>>>(5)?))).transpose()
        })?;
        if let Some((job, old, ready, deadline, count, signed_job)) = &previous {
            let deadline = deadline
                .as_ref()
                .map(|v| {
                    v.as_slice()
                        .try_into()
                        .map(u64::from_be_bytes)
                        .map_err(|_| corrupt("invalid history deadline"))
                })
                .transpose()?;
            if *ready
                && old == token
                && deadline.is_none_or(|d| d > now)
                && signed_job.as_deref() == signed.as_ref().map(|g| g.job.as_slice())
            {
                return Ok(HistoryGeneration {
                    job: *job,
                    ready: true,
                    count: usize::try_from(*count)
                        .map_err(|_| LogError::RepositoryCounterExhausted)?,
                });
            }
        }
        let retired = previous.as_ref().map(|row| row.0);
        if let Some((job, ..)) = previous {
            // Retirement commits in chunks. A failure or restart must not let a
            // reverted visibility token reuse this partially retired generation.
            self.database.transaction(|c| {
                c.execute(
                    "UPDATE history_order_jobs SET ready=0 WHERE job=?1 AND ready=1",
                    [job.as_slice()],
                )?;
                Ok(())
            })?;
            // Obsolete metadata is retired in bounded transactions, never in a
            // history-sized Vec or cascading deletion.
            loop {
                let removed=self.database.transaction(|c|{
                    let mut total=0;
                    for table in ["history_order_rows","history_order_candidates"] {
                        total+=c.execute(&format!("DELETE FROM {table} WHERE job=?1 AND id IN(SELECT id FROM {table} WHERE job=?1 LIMIT 128)"),[job.as_slice()])?;
                    }Ok(total)
                })?;
                if removed == 0 {
                    break;
                }
            }
        }
        let job = *uuid::Uuid::new_v4().as_bytes();
        self.database.transaction(|c|{
            if let Some(retired) = retired { c.execute("DELETE FROM history_order_inputs WHERE job=?1",[retired.as_slice()])?; }
            c.execute("DELETE FROM history_order_jobs WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conv.as_bytes().as_slice()])?;
            c.execute("INSERT INTO history_order_jobs(domain,conversation,job,token,ready,signed_job) VALUES(?1,?2,?3,?4,0,?5)",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),job.as_slice(),token,signed.as_ref().map(|g|g.job.as_slice())])?;Ok(())
        })?;
        Ok(HistoryGeneration {
            job,
            ready: false,
            count: 0,
        })
    }
    pub(crate) fn history_add(
        &self,
        conv: &ConversationId,
        job: [u8; 16],
        row: HistorySelector,
        deadline: Option<u64>,
    ) -> Result<(), LogError> {
        self.database.transaction(|c|{
            let scope:Option<i64>=c.query_row("SELECT s.lifecycle FROM retained_events e JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation WHERE e.domain=?1 AND e.id=?2 AND e.conversation=?3",params![self.domain.0.as_slice(),row.id.as_bytes().as_slice(),conv.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            if scope.is_some_and(|state|state!=0){return Ok(());}
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM history_order_jobs WHERE domain=?1 AND conversation=?2 AND job=?3 AND ready=0)",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),job.as_slice()],|r|r.get(0))?;
            if !exists{return Err(LogError::StaleCursor);}
            c.execute("INSERT INTO history_order_candidates VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(job,id) DO UPDATE SET clock=excluded.clock,source_kind=excluded.source_kind,ordinal=excluded.ordinal,priority=excluded.priority,sequence=excluded.sequence,charged_bytes=excluded.charged_bytes,archive=excluded.archive WHERE (excluded.priority,excluded.clock,excluded.sequence,excluded.ordinal)<(priority,clock,sequence,ordinal)",params![job.as_slice(),row.id.as_bytes().as_slice(),row.clock.to_be_bytes().as_slice(),row.source_kind,row.ordinal,row.priority,row.sequence.to_be_bytes().as_slice(),integer(row.charged_bytes)?,i64::from(scope.is_none())])?;
            if let Some(deadline)=deadline {c.execute("UPDATE history_order_jobs SET next_deadline=?2 WHERE job=?1 AND (next_deadline IS NULL OR next_deadline>?2)",params![job.as_slice(),deadline.to_be_bytes().as_slice()])?;}
            Ok(())
        })
    }
    pub(crate) fn history_order_step(&self, job: [u8; 16]) -> Result<HistoryGeneration, LogError> {
        self.database.transaction(|c|{
            let (ready,signed,mut signed_rank,mut clock,mut archive,mut rank):(bool,Option<Vec<u8>>,i64,Vec<u8>,Vec<u8>,i64)=c.query_row("SELECT ready,signed_job,signed_rank,archive_clock,archive_id,next_rank FROM history_order_jobs WHERE job=?1",[job.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            if ready{return Ok(HistoryGeneration{job,ready:true,count:usize::try_from(rank).map_err(|_|LogError::RepositoryCounterExhausted)?});}
            let mut done=false;
            for _ in 0..128 {
                let signed_head:Option<(Vec<u8>,Vec<u8>,i64)>=if let Some(signed)=&signed {c.query_row("SELECT e.wall_clock,e.id,r.rank FROM event_order_rows r JOIN retained_events e ON e.domain=?1 AND e.id=r.id WHERE r.job=?2 AND r.rank>?3 ORDER BY r.rank LIMIT 1",params![self.domain.0.as_slice(),signed,signed_rank],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?}else{None};
                let archive_filter=if signed.is_some(){" AND archive=1"}else{""};
                let archive_query=format!("SELECT clock,id FROM history_order_candidates WHERE job=?1{archive_filter} AND (clock,id)>(?2,?3) ORDER BY clock,id LIMIT 1");
                let archive_head:Option<(Vec<u8>,Vec<u8>)>=c.query_row(&archive_query,params![job.as_slice(),clock,archive],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                let id=match(signed_head,archive_head){
                    (None,None)=>{done=true;break;},
                    (Some((sc,si,sr)),Some((ac,ai)))=>if (ac.as_slice(),ai.as_slice())<(sc.as_slice(),si.as_slice()){clock=ac;archive=ai.clone();ai}else{signed_rank=sr;si},
                    (Some((_,id,sr)),None)=>{signed_rank=sr;id},
                    (None,Some((ac,ai)))=>{clock=ac;archive=ai.clone();ai},
                };
                let inserted=c.execute("INSERT INTO history_order_rows SELECT ?1,?2,id FROM history_order_candidates WHERE job=?1 AND id=?3",params![job.as_slice(),rank,id])?;
                if inserted!=0{rank=advance(rank)?;}
            }
            c.execute("UPDATE history_order_jobs SET ready=?2,signed_rank=?3,archive_clock=?4,archive_id=?5,next_rank=?6 WHERE job=?1",params![job.as_slice(),done,signed_rank,clock,archive,rank])?;
            Ok(HistoryGeneration{job,ready:done,count:usize::try_from(rank).map_err(|_|LogError::RepositoryCounterExhausted)?})
        })
    }
    pub(crate) fn history_rank(
        &self,
        job: [u8; 16],
        id: EventId,
    ) -> Result<Option<usize>, LogError> {
        self.database.read(|c| {
            let rank: Option<i64> = c
                .query_row(
                    "SELECT rank FROM history_order_rows WHERE job=?1 AND id=?2",
                    params![job.as_slice(), id.as_bytes().as_slice()],
                    |r| r.get(0),
                )
                .optional()?;
            rank.map(|n| usize::try_from(n).map_err(|_| corrupt("invalid history rank")))
                .transpose()
        })
    }
    pub(crate) fn history_selectors(
        &self,
        job: [u8; 16],
        start: usize,
        end: usize,
        bytes: usize,
    ) -> Result<Vec<HistorySelector>, LogError> {
        if end < start || end - start > 128 || bytes == 0 || bytes > 8 * 1024 * 1024 {
            return Err(LogError::InvalidPageBudget);
        }
        self.database.read(|c|{
            let mut statement=c.prepare("SELECT s.id,s.clock,s.source_kind,s.ordinal,s.priority,s.sequence,s.charged_bytes FROM history_order_rows r JOIN history_order_candidates s ON s.job=r.job AND s.id=r.id WHERE r.job=?1 AND r.rank>=?2 AND r.rank<?3 ORDER BY r.rank")?;
            let mut rows=statement.query(params![job.as_slice(),integer(start)?,integer(end)?])?;
            let mut result=Vec::new();let mut charged=0usize;
            while let Some(row)=rows.next()? {
                let cost=usize::try_from(nonnegative(row.get(6)?)?).map_err(|_|LogError::RepositoryCounterExhausted)?;
                charged=charged.checked_add(cost).ok_or(LogError::RepositoryCounterExhausted)?;
                if charged>bytes{return Err(LogError::PageBudgetTooSmall{required:charged});}
                result.push(HistorySelector{id:EventId::new(blob(row,0)?),clock:u64::from_be_bytes(blob(row,1)?),source_kind:row.get(2)?,ordinal:row.get(3)?,priority:row.get(4)?,sequence:u64::from_be_bytes(blob(row,5)?),charged_bytes:cost});
            }Ok(result)
        })
    }
}

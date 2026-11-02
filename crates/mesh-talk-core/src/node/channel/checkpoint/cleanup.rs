//! A new canonical generation never accumulates a third history-sized derived
//! copy. Obsolete facts retire in bounded transactions before replay continues.
use super::*;
const SCHEMA: &str = "
CREATE TABLE channel_retired_work(channel BLOB PRIMARY KEY CHECK(length(channel)=32),epoch INTEGER NOT NULL) WITHOUT ROWID;
CREATE INDEX channel_projection_work_channel ON channel_projection_work(channel,job);
CREATE INDEX channel_legacy_scan_channel ON channel_legacy_scan(channel,job);
CREATE INDEX channel_projection_queries_channel ON channel_projection_queries(channel,job);
CREATE INDEX channel_legacy_cuts_channel ON channel_legacy_cuts(channel,source_job,event);
";
const TRIGGER:&str = "CREATE TRIGGER channel_retired_queue AFTER UPDATE OF lifecycle ON event_conversations WHEN NEW.domain=x'4d657368506172744576656e74735631' AND NEW.lifecycle=1 AND OLD.lifecycle=0 BEGIN
 INSERT INTO channel_retired_work SELECT NEW.conversation,NEW.destructive_epoch WHERE EXISTS(SELECT 1 FROM channel_current WHERE channel=NEW.conversation) OR EXISTS(SELECT 1 FROM channel_projection_work WHERE channel=NEW.conversation) ON CONFLICT(channel) DO UPDATE SET epoch=excluded.epoch;
END";
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_retired_work')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
        c.execute_batch(TRIGGER)?;
        c.execute("INSERT INTO channel_retired_work SELECT s.channel,COALESCE(e.destructive_epoch,-1) FROM channel_current s LEFT JOIN event_conversations e ON e.domain=?1 AND e.conversation=s.channel WHERE e.conversation IS NULL OR e.lifecycle=1 OR e.event_count=0",[b"MeshPartEventsV1".as_slice()])?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    let actual: Option<String> = c
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='channel_retired_queue'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if actual.as_deref() != Some(TRIGGER) {
        return Err(invalid("incompatible channel retirement trigger"));
    }
    Ok(())
}
impl ChannelCheckpointStore {
    /// Logical retirement queues this cleanup even when nobody reads the
    /// channel again. Consumed keys/evidence/receipts are not disposable facts.
    pub(in crate::node) fn retire_orphan_step(&self, limit: usize) -> Result<usize, LogError> {
        if limit == 0 {
            return Err(LogError::InvalidPageBudget);
        }
        let limit = limit.min(128);
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let row:Option<(Vec<u8>,i64)>=c.query_row("SELECT channel,epoch FROM channel_retired_work ORDER BY channel LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((id,epoch))=row else{return Ok(0);};
            let channel=ConversationId::new(id.try_into().map_err(|_|invalid("invalid retired channel"))?);
            let reused:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_conversations WHERE domain=?1 AND conversation=?2 AND (destructive_epoch<>?3 OR (lifecycle=0 AND event_count>0)))",params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),epoch],|r|r.get(0))?;
            if reused {return Ok(c.execute("DELETE FROM channel_retired_work WHERE channel=?1",[channel.as_bytes().as_slice()])?);}
            let cut:Option<(Vec<u8>,Vec<u8>,Vec<u8>)>=c.query_row("SELECT source_job,event,selection FROM channel_legacy_cuts WHERE channel=?1 ORDER BY source_job,event LIMIT 1",[channel.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((job,event,selection))=cut {
                let marked=c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query=?1 AND retired=0",[selection])?;
                if marked!=0 {return Ok(marked);}
                return Ok(c.execute("DELETE FROM channel_legacy_cuts WHERE source_job=?1 AND channel=?2 AND event=?3",params![job,channel.as_bytes().as_slice(),event])?);
            }
            let query:Option<(Vec<u8>,bool)>=c.query_row("SELECT job,retired FROM channel_projection_queries WHERE channel=?1 ORDER BY job LIMIT 1",[channel.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((job,retired))=query {
                if !retired {return Ok(c.execute("UPDATE channel_projection_queries SET retired=1 WHERE job=?1",[&job])?);}
                return causal::cleanup_step_on(c,limit);
            }
            let job:Option<Vec<u8>>=c.query_row("SELECT job FROM channel_projection_work WHERE channel=?1 ORDER BY job LIMIT 1",[channel.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            let job=match job {Some(job)=>Some(job),None=>c.query_row("SELECT job FROM channel_legacy_scan WHERE channel=?1 ORDER BY job LIMIT 1",[channel.as_bytes().as_slice()],|r|r.get(0)).optional()?};
            if let Some(job)=job {let removed=retire_job_on(c,&job,channel,limit as i64)?;if removed!=0 {return Ok(removed);}}
            let removed=crate::node::group_metadata::retire_index_on(c,channel,limit)?;
            if removed!=0 {return Ok(removed);}
            for table in ["channel_book_metadata","channel_book_work","channel_runtime_work","channel_dirty","channel_current"] {
                let removed=c.execute(&format!("DELETE FROM {table} WHERE channel=?1"),[channel.as_bytes().as_slice()])?;
                if removed!=0 {return Ok(removed);}
            }
            Ok(c.execute("DELETE FROM channel_retired_work WHERE channel=?1",[channel.as_bytes().as_slice()])?)
        })
    }
    pub(super) fn cleanup_generation_step_with_limit(
        &self,
        channel: ConversationId,
        current: [u8; 16],
        limit: usize,
    ) -> Result<usize, LogError> {
        let limit = i64::try_from(limit).map_err(|_| LogError::InvalidPageBudget)?;
        if limit == 0 {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let old:Option<Vec<u8>>=c.query_row("SELECT job FROM (SELECT job,channel FROM channel_projection_work UNION SELECT job,channel FROM channel_legacy_scan) WHERE channel=?1 AND job NOT IN (SELECT job FROM channel_projection_queries) AND job<>?2 AND job<>COALESCE((SELECT source_job FROM channel_current WHERE channel=?1),x'') LIMIT 1",params![channel.as_bytes().as_slice(),current.as_slice()],|r|r.get(0)).optional()?;
            let Some(job)=old else {return Ok(0);};
            retire_job_on(c,&job,channel,limit)

        })
    }
}

fn retire_job_on(
    c: &Connection,
    job: &[u8],
    channel: ConversationId,
    limit: i64,
) -> Result<usize, LogError> {
    // Exactly one table and at most the admitted identities are reclaimed per
    // turn. No cascading delete or unbounded SQLite temporary set.
    for (table, key) in [
        ("channel_decision_answers", "key"),
        ("channel_decision_work", "channel"),
        ("channel_event_authority", "event"),
        ("channel_departure_facts", "member,event"),
        ("channel_membership_snapshots", "rank"),
        ("channel_causal_states", "event"),
        ("channel_activities", "event"),
        ("channel_replica_preferences", "member"),
        ("channel_history_policy_cache", "channel"),
        ("channel_history_controls", "event"),
        ("channel_history_pending", "event"),
        ("channel_history_scan", "channel"),
        ("channel_history_send_scan", "channel"),
        (
            "channel_receive_shipments",
            "recipient_ed,recipient_x,grant_account,grant_incarnation,epoch,author",
        ),
        ("channel_authority_facts", "kind,key"),
        ("channel_authority_current", "channel"),
        ("channel_legacy_candidates", "event"),
        ("channel_legacy_scan", "channel"),
    ] {
        if table == "channel_decision_work" {
            super::super::graph::preparing::retire_selection_on(
                c,
                job,
                channel.as_bytes().as_slice(),
            )?;
        }
        let sql = format!(
            "DELETE FROM {table} WHERE job=?1 AND channel=?2 AND ({key}) IN (SELECT {key} FROM {table} WHERE job=?1 AND channel=?2 LIMIT ?3)"
        );
        let removed = c.execute(&sql, params![job, channel.as_bytes().as_slice(), limit])?;
        if removed != 0 {
            return Ok(removed);
        }
    }
    Ok(c.execute(
        "DELETE FROM channel_projection_work WHERE job=?1 AND channel=?2",
        params![job, channel.as_bytes().as_slice()],
    )?)
}

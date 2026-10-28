//! Logical scope retirement and persistent, bounded physical reclamation.
use super::*;
#[cfg(test)]
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ConversationChargeMetadata {
    pub count: u64,
    pub stored_bytes: u64,
    pub estimated_bytes: u64,
    pub retiring: bool,
}
#[derive(Clone)]
pub(crate) struct RetirementToken {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: ConversationId,
    epoch: i64,
    revision: i64,
    _lease: RepositoryOwner,
}
pub(crate) enum RetirementBegin {
    Absent,
    Started(RetirementToken),
    Existing(RetirementToken),
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetirementBudget {
    pub auxiliary: usize,
    pub edges: usize,
    pub vertices: usize,
    pub bytes: usize,
}
impl Default for RetirementBudget {
    fn default() -> Self {
        Self {
            auxiliary: 128,
            edges: 512,
            vertices: 128,
            bytes: 64 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RetirementProgress {
    pub complete: bool,
    pub phase: i64,
    pub auxiliary: usize,
    pub edges: usize,
    pub vertices: usize,
    pub deleted_vertices: usize,
    pub deleted_auxiliary: usize,
    /// Estimated work, not reclaimed disk space.
    pub work_bytes: usize,
    pub reclaimed_estimate: u64,
    pub deleted_body_bytes: u64,
    pub oversized_body_bytes: Option<u64>,
    /// Set only by the committing wrapper; `_on` does not own COMMIT.
    #[cfg(test)]
    pub transaction_latency: Option<Duration>,
}
pub(super) struct Scope {
    pub(super) lifecycle: i64,
    epoch: i64,
    revision: i64,
    count: i64,
}
struct Job {
    phase: i64,
    vertex: Option<[u8; 32]>,
    commit: Option<i64>,
}
impl EventRepository {
    pub(super) fn scope_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<Option<Scope>, LogError> {
        let scope = c.prepare_cached("SELECT lifecycle,destructive_epoch,revision,event_count FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok(Scope{lifecycle:r.get(0)?,epoch:r.get(1)?,revision:r.get(2)?,count:r.get(3)?})).optional()?;
        let job = self.retirement_job_on(c, conv, scope.as_ref())?;
        if let Some(s) = &scope {
            nonnegative(s.epoch)?;
            nonnegative(s.revision)?;
            nonnegative(s.count)?;
            if !(0..=1).contains(&s.lifecycle) || (s.lifecycle == 1) != job.is_some() {
                return Err(corrupt("conversation retirement authority disagrees"));
            }
        } else if job.is_some() {
            return Err(corrupt("retirement job lacks conversation"));
        }
        Ok(scope)
    }
    fn retirement_job_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        scope: Option<&Scope>,
    ) -> Result<Option<Job>, LogError> {
        let mut stmt=c.prepare_cached("SELECT store_uuid,epoch,revision,phase,vertex,vertex_commit,auxiliary,edges,vertices,work_bytes,reclaimed_estimate,deleted_body_bytes,deleted_auxiliary,deleted_vertices FROM event_retirement_jobs WHERE domain=?1 AND conversation=?2")?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice()
        ])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let phase: i64 = r.get(3)?;
        let vertex = r
            .get::<_, Option<Vec<u8>>>(4)?
            .map(|v| {
                v.try_into()
                    .map_err(|_| corrupt("invalid retirement vertex"))
            })
            .transpose()?;
        let commit: Option<i64> = r.get(5)?;
        let epoch: i64 = r.get(1)?;
        let revision: i64 = r.get(2)?;
        if blob::<16>(r, 0)? != self.store_uuid
            || scope.is_none_or(|s| s.lifecycle != 1 || epoch != s.epoch || revision != s.revision)
            || !(0..=7).contains(&phase)
            || vertex.is_some() != commit.is_some()
            || vertex.is_some() != matches!(phase, 5 | 6)
        {
            return Err(corrupt("invalid retirement job authority or progress"));
        }
        if let Some(n) = commit {
            nonnegative(n)?;
        }
        for col in 6..14 {
            nonnegative(r.get(col)?)?;
        }
        Ok(Some(Job {
            phase,
            vertex,
            commit,
        }))
    }
    pub(super) fn require_active_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        if self.scope_on(c, conv)?.is_some_and(|s| s.lifecycle == 1) {
            return Err(LogError::ConversationRetiring);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn scope_revision_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<i64, LogError> {
        // Cursor binding precedes interpretation of any retiring work state.
        let revision: Option<i64> = c
            .query_row(
                "SELECT revision FROM event_conversations WHERE domain=?1 AND conversation=?2",
                params![self.domain.0.as_slice(), conv.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        nonnegative(revision.unwrap_or(0))
    }
    pub(super) fn require_visible_id_on(
        &self,
        c: &Connection,
        id: &EventId,
    ) -> Result<(), LogError> {
        let conv: Option<Vec<u8>> = c
            .query_row(
                "SELECT conversation FROM retained_events WHERE domain=?1 AND id=?2",
                params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(conv) = conv {
            self.require_active_on(
                c,
                &ConversationId::new(
                    conv.try_into()
                        .map_err(|_| corrupt("invalid event scope"))?,
                ),
            )?;
        }
        Ok(())
    }
    /// Physical charge only: partial retired graph summaries are not logical metadata.
    pub(crate) fn conversation_charge_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<Option<ConversationChargeMetadata>, LogError> {
        let scope = self.scope_on(c, conv)?;
        let Some(scope) = scope else { return Ok(None) };
        let (stored,estimate):(i64,i64)=c.prepare_cached("SELECT stored_bytes,estimated_bytes FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        Ok(Some(ConversationChargeMetadata {
            count: scope.count as u64,
            stored_bytes: nonnegative(stored)? as u64,
            estimated_bytes: nonnegative(estimate)? as u64,
            retiring: scope.lifecycle == 1,
        }))
    }
    pub(super) fn require_retirement_connection_on(&self, c: &Connection) -> Result<(), LogError> {
        let actual = {
            let mut stmt =
                c.prepare("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1")?;
            let mut rows = stmt.query([])?;
            blob::<16>(
                rows.next()?
                    .ok_or_else(|| corrupt("missing retirement database identity"))?,
                0,
            )?
        };
        if actual != self.store_uuid {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    fn retirement_token(&self, conv: ConversationId, s: &Scope) -> RetirementToken {
        RetirementToken {
            store_uuid: self.store_uuid,
            domain: self.domain,
            conversation: conv,
            epoch: s.epoch,
            revision: s.revision,
            _lease: self.database.clone(),
        }
    }
    #[cfg(test)]
    pub(crate) fn resume_retirement(
        &self,
        conv: &ConversationId,
    ) -> Result<Option<RetirementToken>, LogError> {
        self.database.read(|c| self.resume_retirement_on(c, conv))
    }
    pub(crate) fn resume_retirement_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<Option<RetirementToken>, LogError> {
        self.require_retirement_connection_on(c)?;
        Ok(self
            .scope_on(c, conv)?
            .filter(|s| s.lifecycle == 1)
            .map(|s| self.retirement_token(*conv, &s)))
    }
    #[cfg(test)]
    pub(crate) fn begin_retirement(
        &self,
        conv: &ConversationId,
    ) -> Result<RetirementBegin, LogError> {
        self.database
            .transaction(|c| self.begin_retirement_on(c, conv))
    }
    pub(crate) fn begin_retirement_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<RetirementBegin, LogError> {
        self.require_retirement_connection_on(c)?;
        let Some(mut scope) = self.scope_on(c, conv)? else {
            return Ok(RetirementBegin::Absent);
        };
        if scope.lifecycle == 1 {
            return Ok(RetirementBegin::Existing(
                self.retirement_token(*conv, &scope),
            ));
        }
        if scope.count == 0 {
            return Ok(RetirementBegin::Absent);
        }
        scope.epoch = advance(scope.epoch)?;
        scope.revision = advance(scope.revision)?;
        scope.lifecycle = 1;
        let source: i64 = c.query_row(
            "SELECT source_revision FROM event_domains WHERE domain=?1",
            [self.domain.0.as_slice()],
            |r| r.get(0),
        )?;
        let domain = self.domain.0.as_slice();
        let conversation = conv.as_bytes().as_slice();
        c.execute("UPDATE event_conversations SET lifecycle=1,destructive_epoch=?3,revision=?4,fingerprint_revision=NULL,fingerprint=NULL WHERE domain=?1 AND conversation=?2",params![domain,conversation,scope.epoch,scope.revision])?;
        c.execute(
            "UPDATE event_domains SET source_revision=?2 WHERE domain=?1",
            params![domain, advance(source)?],
        )?;
        c.execute("INSERT INTO event_retirement_jobs(domain,conversation,store_uuid,epoch,revision) VALUES(?1,?2,?3,?4,?5)",params![domain,conversation,self.store_uuid.as_slice(),scope.epoch,scope.revision])?;
        c.execute(
            "DELETE FROM event_profile_requests WHERE domain=?1 AND conversation=?2",
            params![domain, conversation],
        )?;
        for kind in 0..=1 {
            c.execute("DELETE FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![domain,conversation,kind])?;
            c.execute("UPDATE event_order_jobs SET phase=4 WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![domain,conversation,kind])?;
        }
        Ok(RetirementBegin::Started(
            self.retirement_token(*conv, &scope),
        ))
    }
    #[cfg(test)]
    pub(crate) fn reclaim_retirement(
        &self,
        token: &RetirementToken,
        budget: RetirementBudget,
    ) -> Result<RetirementProgress, LogError> {
        let started = Instant::now();
        let mut p = self
            .database
            .transaction(|c| self.reclaim_retirement_on(c, token, budget))?;
        p.transaction_latency = Some(started.elapsed());
        Ok(p)
    }
}
mod reclaim;
#[cfg(test)]
mod tests;

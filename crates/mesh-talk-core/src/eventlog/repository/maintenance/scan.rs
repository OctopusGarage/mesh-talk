//! Durable bounded snapshot collection and abandoned candidate cleanup.
use super::*;

impl EventRepository {
    /// Pending requests remain durable across snapshot invalidation and failed
    /// application. A duplicate request does not invalidate an active snapshot.
    pub(crate) fn request_profile_maintenance(
        &self,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        self.database
            .transaction(|c| self.request_profile_maintenance_on(c, conv))
    }
    fn request_profile_maintenance_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        if self.metadata_on(c, conv)?.is_none_or(|m| m.count == 0) {
            return Ok(());
        }
        let changed=c.execute("INSERT INTO event_profile_requests VALUES(?1,?2) ON CONFLICT(domain,conversation) DO NOTHING",params![self.domain.0.as_slice(),conv.as_bytes().as_slice()])?;
        if changed != 0 {
            let revision = advance(self.domain_maintenance_on(c)?.0)?;
            c.execute(
                "UPDATE event_domains SET source_revision=?2 WHERE domain=?1",
                params![self.domain.0.as_slice(), revision],
            )?;
        }
        Ok(())
    }
    /// O(1) snapshot binding. Stale candidates are removed by subsequent bounded
    /// steps before the replacement snapshot starts; begin never bulk-cleans.
    pub(crate) fn begin_profile_maintenance(&self) -> Result<ProfileMaintenanceToken, LogError> {
        self.database
            .transaction(|c| self.begin_profile_maintenance_on(c))
    }
    fn begin_profile_maintenance_on(
        &self,
        c: &Connection,
    ) -> Result<ProfileMaintenanceToken, LogError> {
        let revision = self.domain_maintenance_on(c)?.0;
        let proposed = *uuid::Uuid::new_v4().as_bytes();
        c.execute("INSERT INTO event_profile_jobs(job,domain,source_revision,candidate_count,candidate_bytes,complete) VALUES(?1,?2,?3,0,0,0) ON CONFLICT(domain) DO NOTHING",params![proposed.as_slice(),self.domain.0.as_slice(),revision])?;
        let mut stmt = c.prepare("SELECT job FROM event_profile_jobs WHERE domain=?1")?;
        let mut rows = stmt.query([self.domain.0.as_slice()])?;
        let row = rows
            .next()?
            .ok_or_else(|| corrupt("maintenance job missing after begin"))?;
        Ok(ProfileMaintenanceToken {
            store_uuid: self.store_uuid,
            domain: self.domain,
            job: blob(row, 0)?,
            revision,
        })
    }
    pub(super) fn profile_job(
        &self,
        c: &Connection,
        token: ProfileMaintenanceToken,
    ) -> Result<Option<ProfileJob>, LogError> {
        if token.store_uuid != self.store_uuid
            || token.domain != self.domain
            || token.revision != self.domain_maintenance_on(c)?.0
        {
            return Ok(None);
        }
        let mut stmt=c.prepare("SELECT source_revision,conversation,after_author,after_clock,after_id,candidate_count,candidate_bytes,complete FROM event_profile_jobs WHERE job=?1 AND domain=?2")?;
        let mut rows = stmt.query(params![token.job.as_slice(), self.domain.0.as_slice()])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let job = ProfileJob {
            revision: nonnegative(row.get(0)?)?,
            conversation: optional_blob(row, 1)?,
            author: optional_blob(row, 2)?,
            clock: optional_blob(row, 3)?,
            id: optional_blob(row, 4)?,
            count: nonnegative(row.get(5)?)?,
            bytes: nonnegative(row.get(6)?)?,
            complete: row.get(7)?,
        };
        if job.revision > token.revision
            || job.author.is_some() != job.clock.is_some()
            || job.author.is_some() != job.id.is_some()
            || (job.author.is_some() && job.conversation.is_none())
        {
            return Err(corrupt("invalid maintenance snapshot state"));
        }
        Ok(Some(job))
    }
    /// Each examined profile (including referenced leaves), conversation change,
    /// snapshot reset and removed stale candidate consumes one row and 512 bytes.
    /// SQLite's fixed cache and constant statement/cursor scratch are separate.
    pub(crate) fn step_profile_maintenance(
        &self,
        token: ProfileMaintenanceToken,
        budget: PageBudget,
    ) -> Result<MaintenanceProgress, LogError> {
        if budget.rows == 0 || budget.bytes == 0 || i64::try_from(budget.rows).is_err() {
            return Err(LogError::InvalidPageBudget);
        }
        if budget.bytes < WORK_BYTES {
            return Err(LogError::PageBudgetTooSmall {
                required: WORK_BYTES,
            });
        }
        self.database
            .transaction(|c| self.step_profile_maintenance_on(c, token, budget))
    }
    fn step_profile_maintenance_on(
        &self,
        c: &Connection,
        token: ProfileMaintenanceToken,
        budget: PageBudget,
    ) -> Result<MaintenanceProgress, LogError> {
        let mut progress = MaintenanceProgress {
            state: MaintenanceState::Pending,
            examined: 0,
            charged_bytes: 0,
            candidates: 0,
            candidate_bytes: 0,
        };
        while progress.examined < budget.rows && budget.bytes - progress.charged_bytes >= WORK_BYTES
        {
            let Some(job) = self.profile_job(c, token)? else {
                progress.state = MaintenanceState::Stale;
                return Ok(progress);
            };
            let job_id = token.job.as_slice();
            if job.revision != token.revision {
                // One stale metadata row at a time keeps abandonment cleanup
                // within the same row/byte budget as candidate collection.
                let removed=c.execute("DELETE FROM event_profile_candidates WHERE job=?1 AND (conversation,id) IN (SELECT conversation,id FROM event_profile_candidates WHERE job=?1 ORDER BY conversation,id LIMIT 1)",[job_id])?;
                if removed == 0 {
                    clear_profile_leaves(c, job_id)?;
                    c.execute("UPDATE event_profile_jobs SET source_revision=?2,conversation=NULL,after_author=NULL,after_clock=NULL,after_id=NULL,candidate_count=0,candidate_bytes=0,complete=0 WHERE job=?1",params![job_id,token.revision])?;
                }
            } else if job.complete {
                progress.state = MaintenanceState::Ready;
                progress.candidates = job.count as u64;
                progress.candidate_bytes = job.bytes as u64;
                return Ok(progress);
            } else if let Some(conv) = job.conversation {
                let after = if job.author.is_some() {
                    "AND (author,lamport,id)>(?3,?4,?5)"
                } else {
                    "AND ?3 IS NULL AND ?4 IS NULL AND ?5 IS NULL"
                };
                let mut stmt=c.prepare(&format!("SELECT id,author,lamport,ciphertext_bytes,parent_count FROM retained_events WHERE domain=?1 AND conversation=?2 AND kind=8 {after} ORDER BY author,lamport,id LIMIT 1"))?;
                let mut rows = stmt.query(params![
                    self.domain.0.as_slice(),
                    conv.as_slice(),
                    job.author.as_ref().map(|v| v.as_slice()),
                    job.clock.as_ref().map(|v| v.as_slice()),
                    job.id.as_ref().map(|v| v.as_slice())
                ])?;
                if let Some(row) = rows.next()? {
                    let id: [u8; 32] = blob(row, 0)?;
                    let author: [u8; 32] = blob(row, 1)?;
                    let clock: [u8; 8] = blob(row, 2)?;
                    let estimate = estimated_cost(size(row, 3)?, size(row, 4)?)?;
                    if job.author != Some(author) {
                        clear_profile_leaves(c, job_id)?;
                    }
                    let referenced:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM retained_event_parents WHERE domain=?1 AND parent=?2)",params![self.domain.0.as_slice(),id.as_slice()],|r|r.get(0))?;
                    if !referenced {
                        c.execute(
                            "INSERT INTO event_profile_leaves VALUES(?1,?2,?3,?4)",
                            params![job_id, clock.as_slice(), id.as_slice(), estimate],
                        )?;
                        let leaves:i64=c.query_row("SELECT count(*) FROM (SELECT 1 FROM event_profile_leaves WHERE job=?1 LIMIT 4)",[job_id],|r|r.get(0))?;
                        if leaves > 3 {
                            return Err(corrupt("maintenance leaf buffer exceeds bound"));
                        }
                        if leaves == 3 {
                            let mut leaf=c.prepare("SELECT lamport,id,estimated_bytes FROM event_profile_leaves WHERE job=?1 ORDER BY lamport,id LIMIT 1")?;
                            let mut leaves = leaf.query([job_id])?;
                            let leaf = leaves
                                .next()?
                                .ok_or_else(|| corrupt("maintenance leaf disappeared"))?;
                            let old_clock: [u8; 8] = blob(leaf, 0)?;
                            let old_id: [u8; 32] = blob(leaf, 1)?;
                            let cost = nonnegative(leaf.get(2)?)?;
                            let count = advance(job.count)?;
                            let bytes = checked_total(job.bytes, cost)?;
                            c.execute(
                                "INSERT INTO event_profile_candidates VALUES(?1,?2,?3,?4,?5)",
                                params![
                                    job_id,
                                    conv.as_slice(),
                                    old_id.as_slice(),
                                    author.as_slice(),
                                    cost
                                ],
                            )?;
                            c.execute("DELETE FROM event_profile_leaves WHERE job=?1 AND lamport=?2 AND id=?3",params![job_id,old_clock.as_slice(),old_id.as_slice()])?;
                            c.execute("UPDATE event_profile_jobs SET candidate_count=?2,candidate_bytes=?3 WHERE job=?1",params![job_id,count,bytes])?;
                        }
                    }
                    c.execute("UPDATE event_profile_jobs SET after_author=?2,after_clock=?3,after_id=?4 WHERE job=?1",params![job_id,author.as_slice(),clock.as_slice(),id.as_slice()])?;
                } else {
                    self.advance_profile_conversation(c, token, Some(conv))?;
                }
            } else {
                self.advance_profile_conversation(c, token, None)?;
            }
            progress.examined += 1;
            progress.charged_bytes += WORK_BYTES;
        }
        let Some(job) = self.profile_job(c, token)? else {
            progress.state = MaintenanceState::Stale;
            return Ok(progress);
        };
        if job.revision == token.revision {
            progress.candidates = job.count as u64;
            progress.candidate_bytes = job.bytes as u64;
            if job.complete {
                progress.state = MaintenanceState::Ready
            }
        }
        Ok(progress)
    }
    fn advance_profile_conversation(
        &self,
        c: &Connection,
        token: ProfileMaintenanceToken,
        after: Option<[u8; 32]>,
    ) -> Result<(), LogError> {
        if let Some(conversation) = after {
            let candidates:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_profile_candidates WHERE job=?1 AND conversation=?2)",params![token.job.as_slice(),conversation.as_slice()],|r|r.get(0))?;
            if !candidates {
                // This exhausted conversation has no snapshot candidates. Keep
                // candidate-bearing requests through publication: deleting old
                // children can expose superseded parents on the next pass.
                c.execute(
                    DELETE_REQUEST,
                    params![self.domain.0.as_slice(), conversation.as_slice()],
                )?;
            }
        }
        let filter = if after.is_some() {
            "AND conversation>?2"
        } else {
            "AND ?2 IS NULL"
        };
        let next:Option<Vec<u8>>=c.query_row(&format!("SELECT conversation FROM event_profile_requests WHERE domain=?1 {filter} ORDER BY conversation LIMIT 1"),params![self.domain.0.as_slice(),after.as_ref().map(|v|v.as_slice())],|r|r.get(0)).optional()?;
        let next = next
            .map(|v| {
                v.try_into()
                    .map_err(|_| corrupt("invalid requested conversation"))
            })
            .transpose()?;
        let next: Option<[u8; 32]> = next;
        clear_profile_leaves(c, token.job.as_slice())?;
        c.execute("UPDATE event_profile_jobs SET conversation=?2,after_author=NULL,after_clock=NULL,after_id=NULL,complete=?3 WHERE job=?1",params![token.job.as_slice(),next.as_ref().map(|v|v.as_slice()),next.is_none()])?;
        Ok(())
    }
}

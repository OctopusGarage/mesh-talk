//! Durable bounded snapshot collection and abandoned candidate cleanup.
use super::*;

impl EventRepository {
    /// Queue only currently retained active Profile sources from one discovery
    /// page, including our own author. Presentation entitlement is independent.
    pub(crate) fn request_discovered_profile_maintenance(
        &self,
        headers: &[CommittedHeader],
    ) -> Result<(), LogError> {
        if headers.len() > 128 {
            return Err(LogError::InvalidPageBudget);
        }
        if !headers
            .iter()
            .any(|row| row.active && row.header.kind == EventKind::Profile)
        {
            return Ok(());
        }
        let owner = self.receiving_owner()?;
        self.database.transaction(|c| {
            crate::storage::profile::participant::event_ownership::check_event_owner_on(c, &owner)?;
            let mut conversations = std::collections::HashMap::new();
            for row in headers {
                if !row.active || row.header.kind != EventKind::Profile {
                    continue;
                }
                let retained: bool = c.prepare_cached(
                    "SELECT EXISTS(SELECT 1 FROM retained_events e JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation WHERE e.domain=?1 AND e.id=?2 AND e.commit_ordinal=?3 AND e.conversation=?4 AND e.kind=?5 AND s.lifecycle=0)"
                )?.query_row(params![self.domain.0.as_slice(), row.header.id.as_bytes().as_slice(), row.ordinal, row.header.conversation.as_bytes().as_slice(), EventKind::Profile as i64], |r| r.get(0))?;
                if retained {
                    let processed = c.query_row("SELECT EXISTS(SELECT 1 FROM profile_receive_processed WHERE account=?1 AND id=?2)", params![owner.account().as_slice(),row.header.id.as_bytes().as_slice()], |r|r.get::<_,bool>(0))?;
                    if !processed { conversations.entry(row.header.conversation).and_modify(|cut: &mut i64| *cut=(*cut).max(row.ordinal)).or_insert(row.ordinal); }
                }
            }
            for (conversation,cut) in conversations { self.request_profile_maintenance_through_on(c,&conversation,cut)?; }
            Ok(())
        })
    }
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
        let through = self.latest_profile_request_cut_on(c)?;
        self.request_profile_maintenance_through_on(c, conv, through)
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
        // At most one legacy request gets a conservative bound before the token.
        self.migrate_profile_request_bounds_on(c)?;
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
        // Foreign or stale work has no authority even to advance migration.
        if self.profile_job(c, token)?.is_none() {
            progress.state = MaintenanceState::Stale;
            return Ok(progress);
        }
        // Validate the fixed-size scratch before migration can consume the
        // caller quantum; corruption must not be hidden by bootstrap progress.
        check_profile_leaves(c, token.job.as_slice())?;
        // Qualify legacy requests before any candidate generation. The durable
        // cursor advances independently of candidate resets, one indexed row at
        // a time, so initialization never repeatedly rebuilds partial snapshots.
        while !self.profile_request_bounds_ready_on(c)?
            && progress.examined < budget.rows
            && budget.bytes - progress.charged_bytes >= WORK_BYTES
        {
            self.migrate_profile_request_bounds_on(c)?;
            progress.examined += 1;
            progress.charged_bytes += WORK_BYTES;
            if self.domain_maintenance_on(c)?.0 != token.revision {
                progress.state = MaintenanceState::Stale;
                return Ok(progress);
            }
        }
        if !self.profile_request_bounds_ready_on(c)? {
            return Ok(progress);
        }
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
                self.require_active_on(c, &ConversationId::new(conv))?;
                let Some(through) = self.profile_request_bound_on(c, &conv)? else {
                    self.restart_profile_request_bounds_on(c)?;
                    progress.state = MaintenanceState::Stale;
                    return Ok(progress);
                };
                let after = if job.author.is_some() {
                    "AND (author,lamport,id)>(?3,?4,?5)"
                } else {
                    "AND ?3 IS NULL AND ?4 IS NULL AND ?5 IS NULL"
                };
                let mut stmt=c.prepare(&format!("SELECT id,author,lamport,ciphertext_bytes,parent_count,commit_ordinal FROM retained_events WHERE domain=?1 AND conversation=?2 AND kind=8 {after} ORDER BY author,lamport,id LIMIT 1"))?;
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
                    // Late raw replay is charged and advances the scan cursor,
                    // but cannot expand this request's destructive eligibility.
                    if nonnegative(row.get(5)?)? <= through && !referenced {
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
        if let Some(conversation) = next
            && self.profile_request_bound_on(c, &conversation)?.is_none()
        {
            let through = self.latest_profile_request_cut_on(c)?;
            self.request_profile_maintenance_through_on(
                c,
                &ConversationId::new(conversation),
                through,
            )?;
            self.restart_profile_request_bounds_on(c)?;
            // The revision advance makes this old token stale. Do not mark the
            // remaining unqualified requests complete or consume their demand.
            return Ok(());
        }
        clear_profile_leaves(c, token.job.as_slice())?;
        c.execute("UPDATE event_profile_jobs SET conversation=?2,after_author=NULL,after_clock=NULL,after_id=NULL,complete=?3 WHERE job=?1",params![token.job.as_slice(),next.as_ref().map(|v|v.as_slice()),next.is_none()])?;
        Ok(())
    }
}

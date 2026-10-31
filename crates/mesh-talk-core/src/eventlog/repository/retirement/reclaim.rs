use super::*;
pub(super) const SELECT_VERTEX_METADATA: &str = "SELECT stored_bytes,ciphertext_bytes,parent_count,kind FROM retained_events WHERE domain=?1 AND conversation=?2 AND id=?3";
pub(super) const SELECT_VERTEX_MAXIMUM: &str = "SELECT id,commit_ordinal FROM retained_events INDEXED BY retained_events_commit WHERE domain=?1 AND conversation=?2 ORDER BY commit_ordinal DESC,id DESC LIMIT 1";
const AUX_BYTES: usize = 128;
const EDGE_BYTES: usize = 64;
const VERTEX_BYTES: usize = 128;
impl RetirementProgress {
    fn aux_room(&self, b: RetirementBudget) -> bool {
        self.auxiliary < b.auxiliary && self.work_bytes <= b.bytes - AUX_BYTES
    }
    fn aux(&mut self) {
        self.auxiliary += 1;
        self.work_bytes += AUX_BYTES;
    }
}
impl EventRepository {
    /// Caller owns one short transaction and its commit timing. Checkpoints and
    /// mutations roll back together. Fixed authority probes are O(1) per unit.
    pub(crate) fn reclaim_retirement_on(
        &self,
        c: &Connection,
        t: &RetirementToken,
        b: RetirementBudget,
    ) -> Result<RetirementProgress, LogError> {
        self.require_retirement_connection_on(c)?;
        if b.auxiliary == 0 || b.edges == 0 || b.vertices == 0 || b.bytes < AUX_BYTES + VERTEX_BYTES
        {
            return Err(LogError::InvalidPageBudget);
        }
        if t.store_uuid != self.store_uuid || t.domain != self.domain {
            return Err(LogError::StaleCursor);
        }
        // Reject an obsolete incarnation before interpreting any newer job's
        // progress. A correctly bound token still validates full authority below.
        let binding: Option<(i64, i64)> = c.query_row(
            "SELECT destructive_epoch,revision FROM event_conversations WHERE domain=?1 AND conversation=?2",
            params![self.domain.0.as_slice(), t.conversation.as_bytes().as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        if binding != Some((t.epoch, t.revision)) {
            return Err(LogError::StaleCursor);
        }
        let s = self
            .scope_on(c, &t.conversation)?
            .ok_or(LogError::StaleCursor)?;
        if s.lifecycle == 0 {
            if s.count != 0 {
                return Err(LogError::StaleCursor);
            }
            return Ok(RetirementProgress {
                complete: true,
                phase: 7,
                ..Default::default()
            });
        }
        let mut job = self
            .retirement_job_on(c, &t.conversation, Some(&s))?
            .ok_or_else(|| corrupt("retirement progress missing"))?;
        let mut p = RetirementProgress {
            phase: job.phase,
            ..Default::default()
        };
        loop {
            match job.phase {
                0 => {
                    if !p.aux_room(b) {
                        break;
                    }
                    if !self.retirement_order_unit(c, t, &mut p)? {
                        self.retirement_phase(c, t, &mut job, 1)?;
                    }
                    p.aux();
                }
                1 => {
                    if !p.aux_room(b) {
                        break;
                    }
                    if !self.retirement_candidate_unit(c, t, &mut p)? {
                        self.retirement_phase(c, t, &mut job, 2)?;
                    }
                    p.aux();
                }
                2 => {
                    if !p.aux_room(b) {
                        break;
                    }
                    if !self.retirement_leaf_unit(c, t, &mut p)? {
                        self.retirement_phase(c, t, &mut job, 3)?;
                    }
                    p.aux();
                }
                3 => {
                    if !p.aux_room(b) {
                        break;
                    }
                    let author = key(
                        c,
                        "SELECT author FROM event_author_max WHERE domain=?1 AND conversation=?2 ORDER BY author LIMIT 1",
                        params![
                            self.domain.0.as_slice(),
                            t.conversation.as_bytes().as_slice()
                        ],
                    )?;
                    if let Some(author) = author {
                        p.deleted_auxiliary+=c.execute("DELETE FROM event_author_max WHERE domain=?1 AND conversation=?2 AND author=?3",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice(),author.as_slice()])?;
                    } else {
                        self.retirement_phase(c, t, &mut job, 4)?;
                    }
                    p.aux();
                }
                4 => {
                    if !p.aux_room(b)
                        || p.vertices == b.vertices
                        || b.bytes - p.work_bytes < VERTEX_BYTES + AUX_BYTES
                    {
                        break;
                    }
                    let next = self.retirement_maximum(c, &t.conversation)?;
                    if let Some((id, commit)) = next {
                        c.execute("UPDATE event_retirement_jobs SET phase=5,vertex=?3,vertex_commit=?4 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice(),id.as_slice(),commit])?;
                        job.phase = 5;
                        job.vertex = Some(id);
                        job.commit = Some(commit);
                        p.vertices += 1;
                        p.work_bytes += VERTEX_BYTES;
                    } else {
                        self.retirement_phase(c, t, &mut job, 7)?;
                    }
                    p.aux();
                }
                5 => {
                    if p.edges == b.edges || b.bytes - p.work_bytes < EDGE_BYTES {
                        break;
                    }
                    let id = job
                        .vertex
                        .ok_or_else(|| corrupt("retirement drain lacks vertex"))?;
                    self.retirement_vertex(c, t, &job)?;
                    let parent = key(
                        c,
                        "SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 ORDER BY parent LIMIT 1",
                        params![self.domain.0.as_slice(), id.as_slice()],
                    )?;
                    if let Some(parent) = parent {
                        c.execute("DELETE FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent=?3",params![self.domain.0.as_slice(),id.as_slice(),parent.as_slice()])?;
                        p.edges += 1;
                        p.work_bytes += EDGE_BYTES;
                        cut(9)?;
                    } else {
                        if !p.aux_room(b) {
                            break;
                        }
                        self.retirement_phase(c, t, &mut job, 6)?;
                        p.aux();
                    }
                }
                6 => {
                    if !p.aux_room(b)
                        || p.vertices == b.vertices
                        || b.bytes - p.work_bytes < VERTEX_BYTES + AUX_BYTES
                    {
                        break;
                    }
                    let (stored, estimate, kind) = self.retirement_vertex(c, t, &job)?;
                    let cost = usize::try_from(stored)
                        .ok()
                        .and_then(|n| n.checked_add(VERTEX_BYTES + AUX_BYTES))
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    if cost > b.bytes - p.work_bytes {
                        // One SQLite body row cannot be split. Yield before it if
                        // any earlier unit mutated this transaction; otherwise
                        // report the atomic exception and return immediately.
                        if p.work_bytes != 0 {
                            break;
                        }
                        p.oversized_body_bytes = Some(stored as u64);
                    }
                    let id = job
                        .vertex
                        .ok_or_else(|| corrupt("retirement delete lacks vertex"))?;
                    let incident:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM retained_event_parents WHERE domain=?1 AND (child=?2)) OR EXISTS(SELECT 1 FROM retained_event_parents WHERE domain=?1 AND parent=?2)",params![self.domain.0.as_slice(),id.as_slice()],|r|r.get(0))?;
                    if incident {
                        return Err(corrupt("retirement vertex still has incident edges"));
                    }
                    p.deleted_auxiliary += c.execute(
                        "DELETE FROM event_heads WHERE domain=?1 AND conversation=?2 AND id=?3",
                        params![
                            self.domain.0.as_slice(),
                            t.conversation.as_bytes().as_slice(),
                            id.as_slice()
                        ],
                    )?;
                    if c.execute(
                        "DELETE FROM retained_events WHERE domain=?1 AND id=?2",
                        params![self.domain.0.as_slice(), id.as_slice()],
                    )? != 1
                    {
                        return Err(corrupt("retirement selected vertex disappeared"));
                    }
                    // Charges are owned by the event row, not separately by edges.
                    // All arithmetic is validated before committing the row delete.
                    let charge = self
                        .conversation_charge_on(c, &t.conversation)?
                        .ok_or_else(|| corrupt("retirement charge missing"))?;
                    let domain = self.domain_charge_on(c)?;
                    let count = minus(charge.count, 1)?;
                    let stored_left = minus(charge.stored_bytes, stored as u64)?;
                    let estimate_left = minus(charge.estimated_bytes, estimate as u64)?;
                    let dcount = minus(domain.event_count, 1)?;
                    let destimate = minus(domain.estimated_bytes, estimate as u64)?;
                    c.execute("UPDATE event_conversations SET event_count=?3,stored_bytes=?4,estimated_bytes=?5,v2_count=v2_count-?6,v3_count=v3_count-?7,v4_count=v4_count-?8 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice(),count,stored_left,estimate_left,i64::from(kind==14),i64::from(matches!(kind,15|16)),i64::from(matches!(kind,17|18))])?;
                    c.execute("UPDATE event_domains SET event_count=?2,estimated_bytes=?3 WHERE domain=?1",params![self.domain.0.as_slice(),dcount,destimate])?;
                    c.execute("UPDATE event_retirement_jobs SET phase=4,vertex=NULL,vertex_commit=NULL WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice()])?;
                    job.phase = 4;
                    job.vertex = None;
                    job.commit = None;
                    p.deleted_vertices += 1;
                    cut(10)?;
                    p.aux();
                    p.vertices += 1;
                    p.work_bytes = p
                        .work_bytes
                        .checked_add(cost - AUX_BYTES)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    p.reclaimed_estimate = p
                        .reclaimed_estimate
                        .checked_add(estimate as u64)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    p.deleted_body_bytes = p
                        .deleted_body_bytes
                        .checked_add(stored as u64)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    if p.oversized_body_bytes.is_some() {
                        break;
                    }
                }
                7 => {
                    if !p.aux_room(b) {
                        break;
                    }
                    self.retirement_finalize(c, t, &mut p)?;
                    p.aux();
                    p.complete = true;
                    break;
                }
                _ => return Err(corrupt("unknown retirement phase")),
            }
            // A cut rolls back this whole bounded step, including progress.
            cut(job.phase)?;
        }
        p.phase = job.phase;
        if !p.complete {
            c.execute("UPDATE event_retirement_jobs SET auxiliary=auxiliary+?3,edges=edges+?4,vertices=vertices+?5,work_bytes=work_bytes+?6,reclaimed_estimate=reclaimed_estimate+?7,deleted_body_bytes=deleted_body_bytes+?8,deleted_auxiliary=deleted_auxiliary+?9,deleted_vertices=deleted_vertices+?10 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice(),integer(p.auxiliary)?,integer(p.edges)?,integer(p.vertices)?,integer(p.work_bytes)?,i64::try_from(p.reclaimed_estimate).map_err(|_|LogError::RepositoryCounterExhausted)?,i64::try_from(p.deleted_body_bytes).map_err(|_|LogError::RepositoryCounterExhausted)?,integer(p.deleted_auxiliary)?,integer(p.deleted_vertices)?])?;
        }
        cut(if p.complete { 8 } else { job.phase })?;
        Ok(p)
    }
    fn retirement_phase(
        &self,
        c: &Connection,
        t: &RetirementToken,
        j: &mut Job,
        phase: i64,
    ) -> Result<(), LogError> {
        c.execute(
            "UPDATE event_retirement_jobs SET phase=?3 WHERE domain=?1 AND conversation=?2",
            params![
                self.domain.0.as_slice(),
                t.conversation.as_bytes().as_slice(),
                phase
            ],
        )?;
        j.phase = phase;
        Ok(())
    }
    fn retirement_order_unit(
        &self,
        c: &Connection,
        t: &RetirementToken,
        p: &mut RetirementProgress,
    ) -> Result<bool, LogError> {
        let job:Option<Vec<u8>>=c.query_row("SELECT job FROM event_order_jobs WHERE domain=?1 AND conversation=?2 ORDER BY order_kind LIMIT 1",params![self.domain.0.as_slice(),t.conversation.as_bytes().as_slice()],|r|r.get(0)).optional()?;
        let Some(job) = job else { return Ok(false) };
        if let Some(id) = key(
            c,
            "SELECT id FROM event_order_work WHERE job=?1 ORDER BY id LIMIT 1",
            [job.as_slice()],
        )? {
            p.deleted_auxiliary += c.execute(
                "DELETE FROM event_order_work WHERE job=?1 AND id=?2",
                params![job, id.as_slice()],
            )?;
        } else if let Some(rank) = c
            .query_row(
                "SELECT rank FROM event_order_rows WHERE job=?1 ORDER BY rank LIMIT 1",
                [job.as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
        {
            p.deleted_auxiliary += c.execute(
                "DELETE FROM event_order_rows WHERE job=?1 AND rank=?2",
                params![job, nonnegative(rank)?],
            )?;
        } else {
            p.deleted_auxiliary += c.execute("DELETE FROM event_order_jobs WHERE job=?1", [job])?;
        }
        Ok(true)
    }
    fn retirement_profile_job(&self, c: &Connection) -> Result<Option<[u8; 16]>, LogError> {
        let mut stmt = c.prepare("SELECT job FROM event_profile_jobs WHERE domain=?1")?;
        let mut rows = stmt.query([self.domain.0.as_slice()])?;
        rows.next()?.map(|r| blob(r, 0)).transpose()
    }
    fn retirement_candidate_unit(
        &self,
        c: &Connection,
        t: &RetirementToken,
        p: &mut RetirementProgress,
    ) -> Result<bool, LogError> {
        let Some(job) = self.retirement_profile_job(c)? else {
            return Ok(false);
        };
        let id = key(
            c,
            "SELECT id FROM event_profile_candidates WHERE job=?1 AND conversation=?2 ORDER BY id LIMIT 1",
            params![job.as_slice(), t.conversation.as_bytes().as_slice()],
        )?;
        let Some(id) = id else { return Ok(false) };
        self.require_stale_profile(c, &job)?;
        p.deleted_auxiliary += c.execute(
            "DELETE FROM event_profile_candidates WHERE job=?1 AND conversation=?2 AND id=?3",
            params![
                job.as_slice(),
                t.conversation.as_bytes().as_slice(),
                id.as_slice()
            ],
        )?;
        Ok(true)
    }
    fn require_stale_profile(&self, c: &Connection, job: &[u8; 16]) -> Result<(), LogError> {
        let stale:bool=c.query_row("SELECT j.source_revision<d.source_revision FROM event_profile_jobs j JOIN event_domains d ON d.domain=j.domain WHERE j.job=?1",[job.as_slice()],|r|r.get(0))?;
        if !stale {
            return Err(corrupt("current profile work names retired conversation"));
        }
        Ok(())
    }
    fn retirement_leaf_unit(
        &self,
        c: &Connection,
        t: &RetirementToken,
        p: &mut RetirementProgress,
    ) -> Result<bool, LogError> {
        let Some(job) = self.retirement_profile_job(c)? else {
            return Ok(false);
        };
        let matches: bool = c.query_row(
            "SELECT conversation IS ?2 FROM event_profile_jobs WHERE job=?1",
            params![job.as_slice(), t.conversation.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if !matches {
            return Ok(false);
        }
        self.require_stale_profile(c, &job)?;
        let leaf: Option<([u8; 8], [u8; 32])> = {
            let mut stmt=c.prepare("SELECT lamport,id FROM event_profile_leaves WHERE job=?1 ORDER BY lamport,id LIMIT 1")?;
            let mut rows = stmt.query([job.as_slice()])?;
            rows.next()?
                .map(|r| Ok::<_, LogError>((blob(r, 0)?, blob(r, 1)?)))
                .transpose()?
        };
        if let Some((clock, id)) = leaf {
            p.deleted_auxiliary += c.execute(
                "DELETE FROM event_profile_leaves WHERE job=?1 AND lamport=?2 AND id=?3",
                params![job.as_slice(), clock.as_slice(), id.as_slice()],
            )?;
        } else {
            c.execute("UPDATE event_profile_jobs SET conversation=NULL,after_author=NULL,after_clock=NULL,after_id=NULL,complete=0 WHERE job=?1",[job.as_slice()])?;
        }
        Ok(true)
    }
    fn retirement_maximum(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<Option<([u8; 32], i64)>, LogError> {
        let mut stmt = c.prepare(SELECT_VERTEX_MAXIMUM)?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice()
        ])?;
        rows.next()?
            .map(|r| Ok((blob(r, 0)?, nonnegative(r.get(1)?)?)))
            .transpose()
    }
    fn retirement_vertex(
        &self,
        c: &Connection,
        t: &RetirementToken,
        j: &Job,
    ) -> Result<(i64, i64, i64), LogError> {
        let id = j
            .vertex
            .ok_or_else(|| corrupt("retirement vertex cursor missing"))?;
        let row: Option<(i64, i64, i64, i64)> = c
            .query_row(
                SELECT_VERTEX_METADATA,
                params![
                    self.domain.0.as_slice(),
                    t.conversation.as_bytes().as_slice(),
                    id.as_slice()
                ],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((stored, ciphertext, parents, kind)) = row else {
            return Err(corrupt("retirement vertex cursor disappeared"));
        };
        let commit = j
            .commit
            .ok_or_else(|| corrupt("retirement ordinal cursor missing"))?;
        if self.retirement_maximum(c, &t.conversation)? != Some((id, commit))
            || !(0..=18).contains(&kind)
        {
            return Err(corrupt(
                "retirement vertex cursor is not descending maximum",
            ));
        }
        Ok((
            nonnegative(stored)?,
            estimated_cost(
                usize::try_from(nonnegative(ciphertext)?)
                    .map_err(|_| LogError::RepositoryCounterExhausted)?,
                usize::try_from(nonnegative(parents)?)
                    .map_err(|_| LogError::RepositoryCounterExhausted)?,
            )?,
            kind,
        ))
    }
    fn retirement_finalize(
        &self,
        c: &Connection,
        t: &RetirementToken,
        p: &mut RetirementProgress,
    ) -> Result<(), LogError> {
        let domain = self.domain.0.as_slice();
        let conv = t.conversation.as_bytes().as_slice();
        for table in [
            "retained_events",
            "event_heads",
            "event_author_max",
            "event_order_jobs",
            "event_order_current",
            "event_profile_requests",
        ] {
            let exists: bool = c.query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM {table} WHERE domain=?1 AND conversation=?2)"
                ),
                params![domain, conv],
                |r| r.get(0),
            )?;
            if exists {
                return Err(corrupt(
                    "retirement finalization has remaining dependent rows",
                ));
            }
        }
        if let Some(job) = self.retirement_profile_job(c)? {
            let exists:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_profile_candidates WHERE job=?1 AND conversation=?2) OR EXISTS(SELECT 1 FROM event_profile_jobs WHERE job=?1 AND conversation=?2)",params![job.as_slice(),conv],|r|r.get(0))?;
            if exists {
                return Err(corrupt("retirement finalization has profile dependencies"));
            }
        }
        let charge = self
            .conversation_charge_on(c, &t.conversation)?
            .ok_or_else(|| corrupt("retirement finalization missing charge"))?;
        if charge.count != 0 || charge.stored_bytes != 0 || charge.estimated_bytes != 0 {
            return Err(corrupt("retirement final charge is nonzero"));
        }
        c.execute("UPDATE event_conversations SET lifecycle=0,max_lamport=?3,v2_count=0,v3_count=0,v4_count=0,fingerprint_revision=NULL,fingerprint=NULL WHERE domain=?1 AND conversation=?2",params![domain,conv,0u64.to_be_bytes().as_slice()])?;
        p.deleted_auxiliary += c.execute(
            "DELETE FROM event_retirement_jobs WHERE domain=?1 AND conversation=?2",
            params![domain, conv],
        )?;
        Ok(())
    }
}
fn key(
    c: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<[u8; 32]>, LogError> {
    let mut stmt = c.prepare(sql)?;
    let mut rows = stmt.query(args)?;
    rows.next()?.map(|r| blob(r, 0)).transpose()
}
fn minus(value: u64, delta: u64) -> Result<i64, LogError> {
    i64::try_from(
        value
            .checked_sub(delta)
            .ok_or_else(|| corrupt("retirement charge underflow"))?,
    )
    .map_err(|_| LogError::RepositoryCounterExhausted)
}
fn cut(_phase: i64) -> Result<(), LogError> {
    #[cfg(test)]
    if super::tests::CUT.with(|v| v.get() == Some(_phase)) {
        return Err(corrupt("injected retirement transaction cut"));
    }
    Ok(())
}

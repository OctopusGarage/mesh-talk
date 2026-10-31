use super::*;

impl EventRepository {
    pub(crate) fn order_step(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
        budget: OrderWorkBudget,
    ) -> Result<OrderStep, LogError> {
        if budget.vertices == 0 || budget.edges == 0 || budget.bytes < VERTEX_BYTES {
            return Err(LogError::InvalidPageBudget);
        }
        self.database
            .transaction(|c| self.order_step_on(c, conv, kind, budget))
    }
    fn order_step_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        kind: CausalOrder,
        budget: OrderWorkBudget,
    ) -> Result<OrderStep, LogError> {
        let mut step = OrderStep::new();
        if self.ensure_order_on(c, conv, kind)? == OrderReadiness::Ready {
            step.readiness = OrderReadiness::Ready;
            return Ok(step);
        }
        let mut job = self
            .order_job(c, conv, kind)?
            .ok_or_else(|| corrupt("missing private order job"))?;
        loop {
            match job.phase {
                OBSOLETE => {
                    if !step.vertex_room(budget) {
                        break;
                    }
                    // No cascade or history-sized delete. Exactly one ordinary
                    // row per cleanup unit, then one replacement job at most.
                    if let Some(id) = c
                        .query_row(
                            "SELECT id FROM event_order_work WHERE job=?1 ORDER BY id LIMIT 1",
                            [job.id.as_slice()],
                            |r| r.get::<_, Vec<u8>>(0),
                        )
                        .optional()?
                    {
                        c.execute(
                            "DELETE FROM event_order_work WHERE job=?1 AND id=?2",
                            params![job.id.as_slice(), id],
                        )?;
                    } else if let Some(rank) = c
                        .query_row(
                            "SELECT rank FROM event_order_rows WHERE job=?1 ORDER BY rank LIMIT 1",
                            [job.id.as_slice()],
                            |r| r.get::<_, i64>(0),
                        )
                        .optional()?
                    {
                        c.execute(
                            "DELETE FROM event_order_rows WHERE job=?1 AND rank=?2",
                            params![job.id.as_slice(), rank],
                        )?;
                    } else {
                        c.execute(
                            "DELETE FROM event_order_current WHERE job=?1",
                            [job.id.as_slice()],
                        )?;
                        c.execute(
                            "DELETE FROM event_order_jobs WHERE job=?1",
                            [job.id.as_slice()],
                        )?;
                        step.cleaned += 1;
                        step.charged_bytes += VERTEX_BYTES;
                        self.ensure_order_on(c, conv, kind)?;
                        break;
                    }
                    step.cleaned += 1;
                    step.charged_bytes += VERTEX_BYTES;
                }
                SEED => {
                    if !step.vertex_room(budget) {
                        break;
                    }
                    let mut stmt=c.prepare(&format!("SELECT id,{},parent_count,commit_ordinal FROM retained_events WHERE domain=?1 AND conversation=?2 AND commit_ordinal>?3 AND commit_ordinal<=?4 ORDER BY commit_ordinal LIMIT 1",kind.column()))?;
                    let mut rows = stmt.query(params![
                        self.domain.0.as_slice(),
                        conv.as_bytes().as_slice(),
                        job.seed_after,
                        job.cutoff
                    ])?;
                    if let Some(r) = rows.next()? {
                        let id = blob::<32>(r, 0)?;
                        let priority = blob::<8>(r, 1)?;
                        let degree = nonnegative(r.get(2)?)?;
                        let commit = nonnegative(r.get(3)?)?;
                        if commit <= job.seed_after || commit > job.cutoff {
                            return Err(corrupt("seed outside immutable base"));
                        }
                        c.execute("INSERT INTO event_order_work(job,id,priority,remaining_degree,status) VALUES(?1,?2,?3,?4,0)",params![job.id.as_slice(),id.as_slice(),priority.as_slice(),degree])?;
                        job.seed_after = commit;
                        job.seeded = advance(job.seeded)?;
                        c.execute(
                            "UPDATE event_order_jobs SET seed_after=?2,seeded=?3 WHERE job=?1",
                            params![job.id.as_slice(), job.seed_after, job.seeded],
                        )?;
                        step.seeded += 1;
                        step.charged_bytes += VERTEX_BYTES;
                    } else {
                        if job.seeded != job.base_count {
                            return Err(corrupt("order base count disagrees with source"));
                        }
                        job.phase = RUN;
                        c.execute(
                            "UPDATE event_order_jobs SET phase=1 WHERE job=?1",
                            [job.id.as_slice()],
                        )?;
                    }
                }
                RUN => {
                    if let Some(pop) = job.pop {
                        // child_commit is derived from the immutable child's
                        // append ordinal in the source INSERT transaction.
                        // This covering range excludes all safe tail writes.
                        if !step.edge_room(budget) {
                            break;
                        }
                        let edge:Option<(Vec<u8>,i64)>=c.query_row("SELECT child,child_commit FROM retained_event_parents WHERE domain=?1 AND parent=?2 AND child_commit>?3 AND child_commit<=?4 ORDER BY child_commit,child LIMIT 1",params![self.domain.0.as_slice(),pop.as_slice(),job.child_after,job.cutoff],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                        if let Some((child, commit)) = edge {
                            let child: [u8; 32] =
                                child.try_into().map_err(|_| corrupt("invalid child id"))?;
                            if commit <= job.child_after || commit > job.cutoff {
                                return Err(corrupt("edge outside immutable base"));
                            }
                            let actual_commit:Option<i64>=c.query_row("SELECT commit_ordinal FROM retained_events WHERE domain=?1 AND conversation=?2 AND id=?3",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),child.as_slice()],|r|r.get(0)).optional()?;
                            if actual_commit != Some(commit) {
                                return Err(corrupt("child edge commit disagrees with source"));
                            }
                            if c.execute("UPDATE event_order_work SET remaining_degree=remaining_degree-1 WHERE job=?1 AND id=?2 AND status=0 AND remaining_degree>0",params![job.id.as_slice(),child.as_slice()])?!=1 {return Err(corrupt("invalid causal order indegree"));}
                            job.child_after = commit;
                            // Cursor and decrement commit together. Rollback or
                            // process restart cannot apply this edge twice.
                            c.execute(
                                "UPDATE event_order_jobs SET child_after=?2 WHERE job=?1",
                                params![job.id.as_slice(), commit],
                            )?;
                            step.edges += 1;
                            step.charged_bytes += EDGE_BYTES;
                        } else {
                            if c.execute("UPDATE event_order_work SET status=2 WHERE job=?1 AND id=?2 AND status=1",params![job.id.as_slice(),pop.as_slice()])?!=1 {return Err(corrupt("missing draining vertex"));}
                            job.pop = None;
                            job.child_after = -1;
                            c.execute(
                                "UPDATE event_order_jobs SET pop=NULL,child_after=-1 WHERE job=?1",
                                [job.id.as_slice()],
                            )?;
                        }
                    } else {
                        if job.next_rank == job.base_count {
                            // Index existence probe, not COUNT/sort/all graph.
                            let unresolved:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_order_work WHERE job=?1 AND status<2)",[job.id.as_slice()],|r|r.get(0))?;
                            if unresolved {
                                return Err(corrupt("ranked base still has unresolved vertices"));
                            }
                            job.phase = TAIL;
                            c.execute(
                                "UPDATE event_order_jobs SET phase=2 WHERE job=?1",
                                [job.id.as_slice()],
                            )?;
                            continue;
                        }
                        if !step.vertex_room(budget) {
                            break;
                        }
                        let id:Option<Vec<u8>>=c.query_row("SELECT id FROM event_order_work WHERE job=?1 AND status=0 AND remaining_degree=0 ORDER BY priority,id LIMIT 1",[job.id.as_slice()],|r|r.get(0)).optional()?;
                        let Some(id) = id else {
                            return Err(corrupt(
                                "causal order has remaining vertices but no ready vertex",
                            ));
                        };
                        let id: [u8; 32] =
                            id.try_into().map_err(|_| corrupt("invalid ready vertex"))?;
                        c.execute(
                            "INSERT INTO event_order_rows(job,rank,id) VALUES(?1,?2,?3)",
                            params![job.id.as_slice(), job.next_rank, id.as_slice()],
                        )?;
                        if c.execute("UPDATE event_order_work SET status=1 WHERE job=?1 AND id=?2 AND status=0 AND remaining_degree=0",params![job.id.as_slice(),id.as_slice()])?!=1 {return Err(corrupt("ready vertex changed"));}
                        job.next_rank = advance(job.next_rank)?;
                        job.pop = Some(id);
                        c.execute("UPDATE event_order_jobs SET next_rank=?2,pop=?3,child_after=-1 WHERE job=?1",params![job.id.as_slice(),job.next_rank,id.as_slice()])?;
                        step.ranked += 1;
                        step.charged_bytes += VERTEX_BYTES;
                    }
                }
                TAIL => {
                    // Safe tail is canonical in commit order only because
                    // every append proved > MAX(all preceding priorities).
                    // Publish immediately when caught up, even if the work
                    // budget was consumed by the final tail vertex.
                    if job.next_rank == job.count {
                        let m = self
                            .metadata_on(c, conv)?
                            .ok_or_else(|| corrupt("order source disappeared"))?;
                        if job.revision != m.revision
                            || u64::try_from(job.count).ok() != Some(m.count)
                            || job.seeded != job.base_count
                        {
                            return Err(corrupt("order publication source changed"));
                        }
                        c.execute("INSERT INTO event_order_current(domain,conversation,order_kind,source_revision,job) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(domain,conversation,order_kind) DO UPDATE SET source_revision=excluded.source_revision,job=excluded.job",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),kind.code(),job.revision,job.id.as_slice()])?;
                        c.execute(
                            "UPDATE event_order_jobs SET phase=3 WHERE job=?1",
                            [job.id.as_slice()],
                        )?;
                        step.readiness = OrderReadiness::Ready;
                        break;
                    }
                    if !step.vertex_room(budget) {
                        break;
                    }
                    let next:Option<(Vec<u8>,i64)>=c.query_row("SELECT id,commit_ordinal FROM retained_events WHERE domain=?1 AND conversation=?2 AND commit_ordinal>?3 ORDER BY commit_ordinal LIMIT 1",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),job.tail_after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                    let Some((id, commit)) = next else {
                        return Err(corrupt("safe tail count disagrees with source"));
                    };
                    if commit <= job.tail_after {
                        return Err(corrupt("invalid safe tail ordinal"));
                    }
                    let id: [u8; 32] =
                        id.try_into().map_err(|_| corrupt("invalid safe tail id"))?;
                    c.execute(
                        "INSERT INTO event_order_rows(job,rank,id) VALUES(?1,?2,?3)",
                        params![job.id.as_slice(), job.next_rank, id.as_slice()],
                    )?;
                    job.next_rank = advance(job.next_rank)?;
                    job.tail_after = commit;
                    c.execute(
                        "UPDATE event_order_jobs SET next_rank=?2,tail_after=?3 WHERE job=?1",
                        params![job.id.as_slice(), job.next_rank, job.tail_after],
                    )?;
                    step.ranked += 1;
                    step.charged_bytes += VERTEX_BYTES;
                }
                READY => {
                    step.readiness = OrderReadiness::Ready;
                    break;
                }
                _ => return Err(corrupt("unknown order job phase")),
            }
        }
        Ok(step)
    }
}

//! Atomic publication of one complete metadata-only maintenance snapshot.
use super::*;

impl EventRepository {
    /// Complete-snapshot publication is one durable transaction. Its SQL/disk work
    /// is linear in selected rows and affected metadata, not fixed-latency work.
    pub(crate) fn apply_profile_maintenance(
        &self,
        token: ProfileMaintenanceToken,
    ) -> Result<MaintenanceApply, LogError> {
        self.database
            .transaction(|c| self.apply_profile_maintenance_on(c, token))
    }
    fn apply_profile_maintenance_on(
        &self,
        c: &Connection,
        token: ProfileMaintenanceToken,
    ) -> Result<MaintenanceApply, LogError> {
        let Some(job) = self.profile_job(c, token)? else {
            return Ok(MaintenanceApply {
                state: MaintenanceState::Stale,
                dropped: 0,
            });
        };
        if job.revision != token.revision || !job.complete {
            return Ok(MaintenanceApply {
                state: MaintenanceState::Pending,
                dropped: 0,
            });
        }
        let (source, total) = self.domain_maintenance_on(c)?;
        if job.count < 8 || job.bytes < total / 8 + i64::from(total % 8 != 0) {
            return Ok(MaintenanceApply {
                state: MaintenanceState::Ready,
                dropped: 0,
            });
        }
        let new_source = advance(source)?;
        let domain = self.domain.0.as_slice();
        let job_id = token.job.as_slice();
        let (count,bytes):(i64,i64)=c.query_row("SELECT count(*),coalesce(sum(estimated_bytes),0) FROM event_profile_candidates WHERE job=?1",[job_id],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if count != job.count || bytes != job.bytes {
            return Err(corrupt("maintenance staged counters disagree"));
        }
        let mut dropped = 0usize;
        while let Some(conv) = select_blob::<32>(
            c,
            "SELECT conversation FROM event_profile_candidates WHERE job=?1 ORDER BY conversation,id LIMIT 1",
            [job_id],
        )? {
            let conversation = ConversationId::new(conv);
            let metadata = self
                .metadata_on(c, &conversation)?
                .ok_or_else(|| corrupt("maintenance source conversation missing"))?;
            let revision = advance(metadata.revision)?;
            let epoch = advance(self.network_epoch_on(c, &conversation)?)?;
            c.execute("UPDATE event_conversations SET destructive_epoch=?3 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),epoch])?;
            self.invalidate_conversation_order(c, &conversation)?;
            let mut removed = 0i64;
            let mut estimate = 0i64;
            let mut stored = 0i64;
            loop {
                // Release both statement/row lifetimes before any mutation.
                let candidate = {
                    let mut stmt=c.prepare("SELECT x.id,x.author,x.estimated_bytes,e.ciphertext_bytes,e.parent_count,e.stored_bytes,e.kind,e.conversation,e.author FROM event_profile_candidates x LEFT JOIN retained_events e ON e.domain=?1 AND e.id=x.id WHERE x.job=?2 AND x.conversation=?3 ORDER BY x.id LIMIT 1")?;
                    let mut rows = stmt.query(params![domain, job_id, conv.as_slice()])?;
                    rows.next()?
                        .map(|r| {
                            let id: [u8; 32] = blob(r, 0)?;
                            let author: [u8; 32] = blob(r, 1)?;
                            let indexed = nonnegative(r.get(2)?)?;
                            let cost = estimated_cost(size(r, 3)?, size(r, 4)?)?;
                            let stored = nonnegative(r.get(5)?)?;
                            if r.get::<_, i64>(6)? != 8
                                || blob::<32>(r, 7)? != conv
                                || blob::<32>(r, 8)? != author
                                || cost != indexed
                            {
                                return Err(corrupt(
                                    "maintenance candidate disagrees with retained snapshot",
                                ));
                            }
                            Ok((id, author, cost, stored))
                        })
                        .transpose()?
                };
                let Some((id, author, cost, body_bytes)) = candidate else {
                    break;
                };
                let referenced:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM retained_event_parents WHERE domain=?1 AND parent=?2)",params![domain,id.as_slice()],|r|r.get(0))?;
                if referenced {
                    return Err(corrupt("maintenance candidate is a referenced ancestor"));
                }
                self.delete_event_edges(c, &id)?;
                c.execute(DELETE_HEAD, params![domain, conv.as_slice(), id.as_slice()])?;
                if c.execute(DELETE_EVENT, params![domain, id.as_slice()])? != 1 {
                    return Err(corrupt("maintenance candidate disappeared"));
                }
                let maximum=select_blob::<8>(c,"SELECT seq FROM retained_events WHERE domain=?1 AND conversation=?2 AND author=?3 ORDER BY seq DESC LIMIT 1",params![domain,conv.as_slice(),author.as_slice()])?.ok_or_else(||corrupt("maintenance removed all retained author profiles"))?;
                let changed=c.execute("UPDATE event_author_max SET max_seq=?4 WHERE domain=?1 AND conversation=?2 AND author=?3",params![domain,conv.as_slice(),author.as_slice(),maximum.as_slice()])?;
                if changed != 1 {
                    return Err(corrupt("maintenance author maximum missing"));
                }
                c.execute(
                    DELETE_CANDIDATE,
                    params![job_id, conv.as_slice(), id.as_slice()],
                )?;
                removed = advance(removed)?;
                estimate = checked_total(estimate, cost)?;
                stored = checked_total(stored, body_bytes)?;
                dropped = dropped
                    .checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
            }
            let remaining = subtract(
                i64::try_from(metadata.count).map_err(|_| LogError::RepositoryCounterExhausted)?,
                removed,
            )?;
            let remaining_estimate = subtract(
                i64::try_from(metadata.estimated_bytes)
                    .map_err(|_| LogError::RepositoryCounterExhausted)?,
                estimate,
            )?;
            let remaining_stored = subtract(
                i64::try_from(metadata.stored_bytes)
                    .map_err(|_| LogError::RepositoryCounterExhausted)?,
                stored,
            )?;
            let max=select_blob::<8>(c,"SELECT lamport FROM retained_events WHERE domain=?1 AND conversation=?2 ORDER BY lamport DESC,id DESC LIMIT 1",params![domain,conv.as_slice()])?.ok_or_else(||corrupt("maintenance removed complete conversation"))?;
            c.execute("UPDATE event_conversations SET revision=?3,event_count=?4,stored_bytes=?5,estimated_bytes=?6,max_lamport=?7,fingerprint_revision=NULL,fingerprint=NULL WHERE domain=?1 AND conversation=?2",params![domain,conv.as_slice(),revision,remaining,remaining_stored,remaining_estimate,max.as_slice()])?;
        }
        if i64::try_from(dropped).ok() != Some(job.count) {
            return Err(corrupt("maintenance final count disagrees"));
        }
        c.execute(
            "UPDATE event_domains SET source_revision=?2,estimated_bytes=?3 WHERE domain=?1",
            params![domain, new_source, subtract(total, job.bytes)?],
        )?;
        clear_profile_leaves(c, job_id)?;
        Ok(MaintenanceApply {
            state: MaintenanceState::Ready,
            dropped,
        })
    }
}

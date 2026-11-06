//! SQL-native maintenance. Snapshot collection and abandoned-work cleanup are
//! bounded steps; publication/drop are atomic SQL operations proportional to
//! removed disk rows. These APIs never load retained event bodies.
use super::pages::PageBudget;
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileWorkObservation {
    pub source_revision: i64,
    pub requests_pending: bool,
    pub job: Option<ProfileJobObservation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProfileJobObservation {
    pub revision: i64,
    pub complete: bool,
    pub candidates: i64,
    pub candidate_bytes: i64,
    pub cursor_tag: [u8; 8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaintenanceState {
    Pending,
    Ready,
    Stale,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct ProfileMaintenanceToken {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    job: [u8; 16],
    revision: i64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct MaintenanceProgress {
    pub state: MaintenanceState,
    pub examined: usize,
    pub charged_bytes: usize,
    pub candidates: u64,
    pub candidate_bytes: u64,
}
#[derive(Clone, Copy, Debug)]
pub(crate) struct MaintenanceApply {
    pub state: MaintenanceState,
    pub dropped: usize,
}

impl EventRepository {
    /// Baseline skips existing history. Later calls inspect at most 64 headers,
    /// never bodies or mutable work state. Ordinals are observation only.
    pub(crate) fn observe_committed_kinds(
        &self,
        after: Option<i64>,
    ) -> Result<(i64, [u64; 20], bool, bool), LogError> {
        let next = self.database.read(|c| {
            let next: i64 = c.query_row(
                "SELECT next_commit FROM event_domains WHERE domain=?1",
                [self.domain.0.as_slice()],
                |r| r.get(0),
            )?;
            nonnegative(next)
        })?;
        let latest = next - 1;
        let mut counts = [0u64; 20];
        let Some(after) = after else {
            return Ok((latest, counts, false, false));
        };
        if after > latest || after < -1 {
            return Ok((latest, counts, false, true));
        }
        let rows = self.committed_headers_page((after >= 0).then_some(after), 64)?;
        let mut cursor = after;
        for row in rows {
            if row.ordinal > latest {
                break;
            }
            let count = counts
                .get_mut(row.header.kind as usize)
                .ok_or_else(|| corrupt("unknown observed event kind"))?;
            *count = count.saturating_add(1);
            cursor = row.ordinal;
        }
        // Deleted header gaps do not pin this observational cursor forever.
        let more = cursor < latest && counts.iter().sum::<u64>() == 64;
        if !more {
            cursor = latest;
        }
        Ok((cursor, counts, more, false))
    }
    /// A non-Profile append without Profile parents preserves every selected
    /// leaf and its rank, and only increases the denominator of the byte gate.
    /// Carry only a current, complete negative result; old tokens still fail
    /// the domain revision fence. Caller owns the append transaction.
    pub(super) fn carry_negative_profile_snapshot_on(
        &self,
        c: &Connection,
        old_revision: i64,
        new_revision: i64,
        old_total: i64,
    ) -> Result<(), LogError> {
        c.execute(
            "UPDATE event_profile_jobs SET source_revision=?3 WHERE domain=?1 AND source_revision=?2 AND complete=1 AND (candidate_count<8 OR candidate_bytes<?4)",
            params![self.domain.0.as_slice(), old_revision, new_revision,
                old_total / 8 + i64::from(old_total % 8 != 0)],
        )?;
        Ok(())
    }

    /// One metadata row, including stale jobs which ordinary mutation fences
    /// reject. Observation never advances a cursor or authenticates deletion.
    pub(crate) fn observe_profile_work(&self) -> Result<ProfileWorkObservation, LogError> {
        self.database.read(|c| {
            let mut statement = c.prepare_cached("SELECT d.source_revision,j.job,j.source_revision,j.complete,j.candidate_count,j.candidate_bytes,j.conversation,j.after_author,j.after_clock,j.after_id,EXISTS(SELECT 1 FROM event_profile_requests r WHERE r.domain=d.domain) FROM event_domains d LEFT JOIN event_profile_jobs j ON j.domain=d.domain WHERE d.domain=?1")?;
            let mut rows = statement.query([self.domain.0.as_slice()])?;
            let row = rows.next()?.ok_or_else(|| corrupt("observed maintenance domain missing"))?;
            let source_revision = nonnegative(row.get(0)?)?;
            let requests_pending = row.get(10)?;
            let job = if let Some(job) = optional_blob::<16>(row, 1)? {
                let revision = nonnegative(row.get(2)?)?;
                let complete: i64 = row.get(3)?;
                let candidates = nonnegative(row.get(4)?)?;
                let candidate_bytes = nonnegative(row.get(5)?)?;
                let conversation = optional_blob::<32>(row, 6)?;
                let author = optional_blob::<32>(row, 7)?;
                let clock = optional_blob::<8>(row, 8)?;
                let id = optional_blob::<32>(row, 9)?;
                if revision > source_revision || !(0..=1).contains(&complete)
                    || author.is_some() != clock.is_some()
                    || author.is_some() != id.is_some()
                    || (author.is_some() && conversation.is_none()) {
                    return Err(corrupt("invalid observed maintenance progress"));
                }
                let mut hash = Sha256::new();
                hash.update(b"mesh-talk-profile-work-v1");
                hash.update(self.store_uuid);
                hash.update(self.domain.0);
                hash.update(job);
                for field in [conversation.as_ref().map(|v| v.as_slice()),
                    author.as_ref().map(|v| v.as_slice()),
                    clock.as_ref().map(|v| v.as_slice()),
                    id.as_ref().map(|v| v.as_slice())] {
                    hash.update([u8::from(field.is_some())]);
                    if let Some(bytes) = field { hash.update(bytes); }
                }
                let mut cursor_tag = [0; 8];
                cursor_tag.copy_from_slice(&hash.finalize()[..8]);
                Some(ProfileJobObservation { revision, complete: complete == 1,
                    candidates, candidate_bytes, cursor_tag })
            } else { None };
            Ok(ProfileWorkObservation { source_revision, requests_pending, job })
        })
    }

    #[cfg(test)]
    pub(crate) fn estimated_bytes(&self) -> Result<u64, LogError> {
        self.database
            .read(|c| Ok(self.domain_maintenance_on(c)?.1 as u64))
    }
    fn domain_maintenance_on(&self, c: &Connection) -> Result<(i64, i64), LogError> {
        let (revision, bytes): (i64, i64) = c.query_row(
            "SELECT source_revision,estimated_bytes FROM event_domains WHERE domain=?1",
            [self.domain.0.as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((nonnegative(revision)?, nonnegative(bytes)?))
    }
    /// Profile publication retires derived generations in the source mutation
    /// transaction. Readers lose their binding immediately; `order_step` owns
    /// bounded reclamation of their rank/work rows before replacement.
    fn invalidate_profile_order(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        let domain = self.domain.0.as_slice();
        let conv = conv.as_bytes().as_slice();
        for kind in 0..=1 {
            c.execute(
                "UPDATE event_order_jobs SET phase=4 WHERE domain=?1 AND conversation=?2 AND order_kind=?3",
                params![domain, conv, kind],
            )?;
            c.execute(
                "DELETE FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",
                params![domain, conv, kind],
            )?;
        }
        Ok(())
    }
    fn invalidate_conversation_order(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        let domain = self.domain.0.as_slice();
        let conv = conv.as_bytes().as_slice();
        for kind in 0..=1 {
            let job = select_blob::<16>(
                c,
                "SELECT job FROM event_order_jobs WHERE domain=?1 AND conversation=?2 AND order_kind=?3",
                params![domain, conv, kind],
            )?;
            c.execute("DELETE FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![domain,conv,kind])?;
            let Some(job) = job else { continue };
            loop {
                let rank: Option<i64> = c
                    .query_row(
                        "SELECT rank FROM event_order_rows WHERE job=?1 ORDER BY rank LIMIT 1",
                        [job.as_slice()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(rank) = rank else { break };
                c.execute(
                    DELETE_ORDER_ROW,
                    params![job.as_slice(), nonnegative(rank)?],
                )?;
            }
            while let Some(id) = select_blob::<32>(
                c,
                "SELECT id FROM event_order_work WHERE job=?1 ORDER BY id LIMIT 1",
                [job.as_slice()],
            )? {
                c.execute(DELETE_ORDER_WORK, params![job.as_slice(), id.as_slice()])?;
            }
            c.execute(DELETE_ORDER_JOB, [job.as_slice()])?;
        }
        Ok(())
    }
    fn delete_event_edges(&self, c: &Connection, id: &[u8; 32]) -> Result<(), LogError> {
        while let Some(parent) = select_blob::<32>(
            c,
            "SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 ORDER BY parent LIMIT 1",
            params![self.domain.0.as_slice(), id.as_slice()],
        )? {
            c.execute(
                DELETE_EDGE,
                params![self.domain.0.as_slice(), id.as_slice(), parent.as_slice()],
            )?;
        }
        Ok(())
    }
    pub(crate) fn drop_conversation(&self, conv: &ConversationId) -> Result<usize, LogError> {
        self.database
            .transaction(|c| self.drop_conversation_on(c, conv))
    }
    pub(super) fn drop_conversation_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<usize, LogError> {
        let Some(metadata) = self.metadata_on(c, conv)?.filter(|m| m.count > 0) else {
            return Ok(0);
        };
        let (source, estimate) = self.domain_maintenance_on(c)?;
        let domain_count = subtract(
            i64::try_from(self.domain_charge_on(c)?.event_count)
                .map_err(|_| LogError::RepositoryCounterExhausted)?,
            i64::try_from(metadata.count).map_err(|_| LogError::RepositoryCounterExhausted)?,
        )?;
        let source = advance(source)?;
        let revision = advance(metadata.revision)?;
        let estimate = subtract(
            estimate,
            i64::try_from(metadata.estimated_bytes)
                .map_err(|_| LogError::RepositoryCounterExhausted)?,
        )?;
        let domain = self.domain.0.as_slice();
        let conversation = conv.as_bytes().as_slice();
        let epoch = advance(self.network_epoch_on(c, conv)?)?;
        c.execute("UPDATE event_conversations SET destructive_epoch=?3 WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),(conv).as_bytes().as_slice(),epoch])?;
        self.invalidate_conversation_order(c, conv)?;
        // First remove every selected child's edges before deleting any
        // vertex: ID order need not put descendants before their parents.
        let mut after: Option<[u8; 32]> = None;
        loop {
            let filter = if after.is_some() {
                "AND id>?3"
            } else {
                "AND ?3 IS NULL"
            };
            let id = select_blob::<32>(
                c,
                &format!(
                    "SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 {filter} ORDER BY id LIMIT 1"
                ),
                params![domain, conversation, after.as_ref().map(|id| id.as_slice())],
            )?;
            let Some(id) = id else { break };
            self.delete_event_edges(c, &id)?;
            after = Some(id);
        }
        let mut count = 0usize;
        while let Some(id) = select_blob::<32>(
            c,
            "SELECT id FROM retained_events WHERE domain=?1 AND conversation=?2 ORDER BY id LIMIT 1",
            params![domain, conversation],
        )? {
            c.execute(DELETE_HEAD, params![domain, conversation, id.as_slice()])?;
            if c.execute(DELETE_EVENT, params![domain, id.as_slice()])? != 1 {
                return Err(corrupt("selected drop event disappeared"));
            }
            count = count
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
        }
        while let Some(author) = select_blob::<32>(
            c,
            "SELECT author FROM event_author_max WHERE domain=?1 AND conversation=?2 ORDER BY author LIMIT 1",
            params![domain, conversation],
        )? {
            c.execute(
                DELETE_AUTHOR,
                params![domain, conversation, author.as_slice()],
            )?;
        }
        if u64::try_from(count).ok() != Some(metadata.count) {
            return Err(corrupt("drop count disagrees with retained metadata"));
        }
        c.execute("UPDATE event_conversations SET revision=?3,event_count=0,stored_bytes=0,estimated_bytes=0,max_lamport=?4,v2_count=0,v3_count=0,v4_count=0,fingerprint_revision=NULL,fingerprint=NULL WHERE domain=?1 AND conversation=?2",params![domain,conversation,revision,0u64.to_be_bytes().as_slice()])?;
        c.execute(
            "UPDATE event_domains SET source_revision=?2,estimated_bytes=?3,event_count=?4 WHERE domain=?1",
            params![domain, source, estimate, domain_count],
        )?;
        c.execute(
            "DELETE FROM event_profile_requests WHERE domain=?1 AND conversation=?2",
            params![domain, conversation],
        )?;
        Ok(count)
    }
}
fn subtract(value: i64, delta: i64) -> Result<i64, LogError> {
    nonnegative(value)?
        .checked_sub(nonnegative(delta)?)
        .filter(|n| *n >= 0)
        .ok_or_else(|| corrupt("maintenance counter underflow"))
}
mod apply;
mod bounds;
mod scan;
#[cfg(test)]
mod tests;

const WORK_BYTES: usize = 512;
// Exact primary-key mutation prevents SQLite's two-pass DELETE from building
// retained-history-sized temporary keysets under encrypted temp_store=MEMORY.
const DELETE_EVENT: &str = "DELETE FROM retained_events WHERE domain=?1 AND id=?2";
const DELETE_EDGE: &str =
    "DELETE FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent=?3";
const DELETE_HEAD: &str = "DELETE FROM event_heads WHERE domain=?1 AND conversation=?2 AND id=?3";
const DELETE_AUTHOR: &str =
    "DELETE FROM event_author_max WHERE domain=?1 AND conversation=?2 AND author=?3";
const DELETE_ORDER_ROW: &str = "DELETE FROM event_order_rows WHERE job=?1 AND rank=?2";
const DELETE_ORDER_WORK: &str = "DELETE FROM event_order_work WHERE job=?1 AND id=?2";
const DELETE_ORDER_JOB: &str = "DELETE FROM event_order_jobs WHERE job=?1";
const DELETE_CANDIDATE: &str =
    "DELETE FROM event_profile_candidates WHERE job=?1 AND conversation=?2 AND id=?3";
const DELETE_REQUEST: &str =
    "DELETE FROM event_profile_requests WHERE domain=?1 AND conversation=?2";
fn select_blob<const N: usize>(
    c: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Option<[u8; N]>, LogError> {
    let mut stmt = c.prepare(sql)?;
    let mut rows = stmt.query(parameters)?;
    rows.next()?.map(|row| blob(row, 0)).transpose()
}
struct ProfileJob {
    revision: i64,
    conversation: Option<[u8; 32]>,
    author: Option<[u8; 32]>,
    clock: Option<[u8; 8]>,
    id: Option<[u8; 32]>,
    count: i64,
    bytes: i64,
    complete: bool,
}
fn optional_blob<const N: usize>(r: &Row<'_>, column: usize) -> Result<Option<[u8; N]>, LogError> {
    r.get::<_, Option<Vec<u8>>>(column)?
        .map(|v| {
            v.try_into()
                .map_err(|_| corrupt("invalid maintenance cursor blob"))
        })
        .transpose()
}

fn check_profile_leaves(c: &Connection, job: &[u8]) -> Result<(), LogError> {
    let count: i64 = c.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM event_profile_leaves WHERE job=?1 LIMIT 3)",
        [job],
        |r| r.get(0),
    )?;
    if count > 2 {
        return Err(corrupt("maintenance leaf scratch exceeds bound"));
    }
    Ok(())
}

fn clear_profile_leaves(c: &Connection, job: &[u8]) -> Result<(), LogError> {
    check_profile_leaves(c, job)?;
    // At most two exact-PK deletions, including corrupted-source resets.
    loop {
        let leaf = {
            let mut stmt=c.prepare("SELECT lamport,id FROM event_profile_leaves WHERE job=?1 ORDER BY lamport,id LIMIT 1")?;
            let mut rows = stmt.query([job])?;
            rows.next()?
                .map(|r| Ok::<_, LogError>((blob::<8>(r, 0)?, blob::<32>(r, 1)?)))
                .transpose()?
        };
        let Some((clock, id)) = leaf else { break };
        c.execute(
            "DELETE FROM event_profile_leaves WHERE job=?1 AND lamport=?2 AND id=?3",
            params![job, clock.as_slice(), id.as_slice()],
        )?;
    }
    Ok(())
}

//! Exact canonical Kahn generations for signed retained events only. Archive-only
//! transcript roots require a future combined source and revision owner.
use super::*;

mod engine;
mod read;
#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CausalOrder {
    /// Ready vertices compare their signed Lamport clock, then EventId.
    Logical,
    /// Signed events only; plaintext archive-only roots are not included.
    WallClock,
}
impl CausalOrder {
    fn code(self) -> i64 {
        match self {
            Self::Logical => 0,
            Self::WallClock => 1,
        }
    }
    fn column(self) -> &'static str {
        match self {
            Self::Logical => "lamport",
            Self::WallClock => "wall_clock",
        }
    }
    fn priority(self, e: &Event) -> u64 {
        match self {
            Self::Logical => e.lamport,
            Self::WallClock => e.wall_clock,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OrderReadiness {
    Pending,
    Ready,
}
#[derive(Debug)]
pub(crate) enum OrderReadState<T> {
    Pending,
    Ready(OrderPage<T>),
}
#[derive(Clone, Debug)]
pub(crate) struct OrderCursor {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: ConversationId,
    order: CausalOrder,
    bodies: bool,
    job: [u8; 16],
    revision: i64,
    rank: i64,
}
#[derive(Debug)]
pub(crate) struct OrderPage<T> {
    pub items: Vec<T>,
    pub next: Option<OrderCursor>,
    pub charged_bytes: usize,
}
/// Work is charged for each seeded/ranked/cleaned vertex and each examined edge,
/// including rows that produce no output. Fixed metadata scratch is separate from
/// SQLite's connection cache. A step never fetches or decodes a body.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OrderWorkBudget {
    pub vertices: usize,
    pub edges: usize,
    pub bytes: usize,
}
impl Default for OrderWorkBudget {
    fn default() -> Self {
        Self {
            vertices: 128,
            edges: 512,
            bytes: 64 * 1024,
        }
    }
}
#[derive(Debug)]
pub(crate) struct OrderStep {
    pub readiness: OrderReadiness,
    pub seeded: usize,
    pub ranked: usize,
    pub cleaned: usize,
    pub edges: usize,
    pub charged_bytes: usize,
}
impl OrderStep {
    fn new() -> Self {
        Self {
            readiness: OrderReadiness::Pending,
            seeded: 0,
            ranked: 0,
            cleaned: 0,
            edges: 0,
            charged_bytes: 0,
        }
    }
    fn vertex_room(&self, b: OrderWorkBudget) -> bool {
        self.seeded + self.ranked + self.cleaned < b.vertices
            && b.bytes - self.charged_bytes >= VERTEX_BYTES
    }
    fn edge_room(&self, b: OrderWorkBudget) -> bool {
        self.edges < b.edges && b.bytes - self.charged_bytes >= EDGE_BYTES
    }
}
const VERTEX_BYTES: usize = 128;
const EDGE_BYTES: usize = 64;
const SEED: i64 = 0;
const RUN: i64 = 1;
const TAIL: i64 = 2;
const READY: i64 = 3;
const OBSOLETE: i64 = 4;
struct Job {
    id: [u8; 16],
    revision: i64,
    count: i64,
    base_count: i64,
    cutoff: i64,
    phase: i64,
    seed_after: i64,
    seeded: i64,
    next_rank: i64,
    pop: Option<[u8; 32]>,
    child_after: i64,
    tail_after: i64,
}
impl EventRepository {
    fn order_job(
        &self,
        c: &Connection,
        conv: &ConversationId,
        kind: CausalOrder,
    ) -> Result<Option<Job>, LogError> {
        let mut stmt=c.prepare("SELECT job,source_revision,target_count,base_count,cutoff,phase,seed_after,seeded,next_rank,pop,child_after,tail_after FROM event_order_jobs WHERE domain=?1 AND conversation=?2 AND order_kind=?3")?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice(),
            kind.code()
        ])?;
        let Some(r) = rows.next()? else {
            return Ok(None);
        };
        let pop: Option<Vec<u8>> = r.get(9)?;
        let j = Job {
            id: blob(r, 0)?,
            revision: nonnegative(r.get(1)?)?,
            count: nonnegative(r.get(2)?)?,
            base_count: nonnegative(r.get(3)?)?,
            cutoff: nonnegative(r.get(4)?)?,
            phase: r.get(5)?,
            seed_after: r.get(6)?,
            seeded: nonnegative(r.get(7)?)?,
            next_rank: nonnegative(r.get(8)?)?,
            pop: pop
                .map(|v| v.try_into().map_err(|_| corrupt("invalid order pop id")))
                .transpose()?,
            child_after: r.get(10)?,
            tail_after: r.get(11)?,
        };
        if !(SEED..=OBSOLETE).contains(&j.phase)
            || j.seed_after < -1
            || j.seed_after > j.cutoff
            || j.child_after > j.cutoff
            || j.child_after < -1
            || j.tail_after < j.cutoff
            || j.base_count > j.count
            || j.seeded > j.base_count
            || j.next_rank > j.count
            || (j.pop.is_some() && j.phase != RUN && j.phase != OBSOLETE)
        {
            return Err(corrupt("invalid persistent order state"));
        }
        Ok(Some(j))
    }
    /// Starts a private immutable base. Repeated ensures do not restart an active
    /// job. Obsolete work must be removed by bounded steps before replacement.
    pub(crate) fn ensure_order(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
    ) -> Result<OrderReadiness, LogError> {
        self.database
            .transaction(|c| self.ensure_order_on(c, conv, kind))
    }
    fn ensure_order_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        kind: CausalOrder,
    ) -> Result<OrderReadiness, LogError> {
        let Some(m) = self.metadata_on(c, conv)? else {
            return Ok(OrderReadiness::Ready);
        };
        if m.count == 0 {
            return Ok(OrderReadiness::Ready);
        }
        if let Some(job) = self.order_job(c, conv, kind)? {
            if job.phase != OBSOLETE
                && (job.revision != m.revision || u64::try_from(job.count).ok() != Some(m.count))
            {
                return Err(corrupt(
                    "private order source disagrees with retained events",
                ));
            }
            if job.phase != READY {
                return Ok(OrderReadiness::Pending);
            }
            let current: Option<(Vec<u8>,i64)> = c.query_row("SELECT job,source_revision FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),kind.code()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if current != Some((job.id.to_vec(), job.revision)) || job.next_rank != job.count {
                return Err(corrupt("ready causal generation disagrees with source"));
            }
            return Ok(OrderReadiness::Ready);
        }
        let cutoff: i64 = c.query_row(
            "SELECT next_commit-1 FROM event_domains WHERE domain=?1",
            [self.domain.0.as_slice()],
            |r| r.get(0),
        )?;
        nonnegative(cutoff)?;
        let id = *uuid::Uuid::new_v4().as_bytes();
        c.execute("INSERT INTO event_order_jobs(job,domain,conversation,order_kind,source_revision,target_count,base_count,cutoff,phase,seed_after,seeded,next_rank,pop,child_after,tail_after) VALUES(?1,?2,?3,?4,?5,?6,?6,?7,0,-1,0,0,NULL,-1,?7)",params![id.as_slice(),self.domain.0.as_slice(),conv.as_bytes().as_slice(),kind.code(),m.revision,i64::try_from(m.count).map_err(|_|LogError::RepositoryCounterExhausted)?,cutoff])?;
        Ok(OrderReadiness::Pending)
    }
    /// Called within the same source insertion transaction, after event metadata
    /// publication. The covering maximum includes Profile and inverted ancestors.
    pub(super) fn order_appended(
        &self,
        c: &Connection,
        e: &Event,
        revision: i64,
        count: i64,
    ) -> Result<(), LogError> {
        self.require_active_on(c, &e.conversation_id)?;
        for kind in [CausalOrder::Logical, CausalOrder::WallClock] {
            let Some(job) = self.order_job(c, &e.conversation_id, kind)? else {
                continue;
            };
            if job.phase == OBSOLETE {
                continue;
            }
            if advance(job.revision)? != revision || advance(job.count)? != count {
                return Err(corrupt("order source revision disagrees with append"));
            }
            let max:Option<(Vec<u8>,Vec<u8>)>=c.query_row(&format!("SELECT {},id FROM retained_events WHERE domain=?1 AND conversation=?2 AND id<>?3 ORDER BY {} DESC,id DESC LIMIT 1",kind.column(),kind.column()),params![self.domain.0.as_slice(),e.conversation_id.as_bytes().as_slice(),e.id.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let safe = match max {
                None => true,
                Some((clock, id)) => {
                    let clock: [u8; 8] = clock
                        .try_into()
                        .map_err(|_| corrupt("invalid priority maximum"))?;
                    let id: [u8; 32] = id.try_into().map_err(|_| corrupt("invalid maximum id"))?;
                    (kind.priority(e).to_be_bytes(), *e.id.as_bytes()) > (clock, id)
                }
            };
            if !safe {
                c.execute(
                    "UPDATE event_order_jobs SET phase=4 WHERE job=?1",
                    [job.id.as_slice()],
                )?;
                c.execute("DELETE FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![self.domain.0.as_slice(),e.conversation_id.as_bytes().as_slice(),kind.code()])?;
                continue;
            }
            if job.phase == READY {
                c.execute(
                    "INSERT INTO event_order_rows(job,rank,id) VALUES(?1,?2,?3)",
                    params![job.id.as_slice(), job.next_rank, e.id.as_bytes().as_slice()],
                )?;
                c.execute(
                    "UPDATE event_order_jobs SET next_rank=?2,target_count=?3,source_revision=?4 WHERE job=?1",
                    params![job.id.as_slice(), advance(job.next_rank)?,count,revision],
                )?;
                let changed=c.execute("UPDATE event_order_current SET source_revision=?4 WHERE domain=?1 AND conversation=?2 AND order_kind=?3 AND job=?5",params![self.domain.0.as_slice(),e.conversation_id.as_bytes().as_slice(),kind.code(),revision,job.id.as_slice()])?;
                if changed != 1 {
                    return Err(corrupt("ready order lacks current generation"));
                }
            }
            c.execute(
                "UPDATE event_order_jobs SET source_revision=?2,target_count=?3 WHERE job=?1",
                params![job.id.as_slice(), revision, count],
            )?;
        }
        Ok(())
    }
}

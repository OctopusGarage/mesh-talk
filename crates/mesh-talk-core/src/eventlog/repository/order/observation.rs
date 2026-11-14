//! Bounded diagnostic reads. These observations confer no work or commit authority.
use super::*;
use sha2::{Digest, Sha256};

#[derive(Clone)]
pub(crate) struct OrderWorkCursor {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: [u8; 32],
    order: CausalOrder,
}
pub(crate) struct OrderWorkObservation {
    pub job: [u8; 16],
    pub scope_tag: [u8; 8],
    pub order: CausalOrder,
    pub phase: &'static str,
    pub revision: i64,
    pub target_count: i64,
    pub base_count: i64,
    pub cutoff: i64,
    pub seeded: i64,
    pub next_rank: i64,
    pub seed_after: i64,
    pub child_after: i64,
    pub tail_after: i64,
    pub has_pop: bool,
}
pub(crate) struct OrderWorkPage {
    pub items: Vec<OrderWorkObservation>,
    pub next: Option<OrderWorkCursor>,
}
const SELECT_WORK: &str = "SELECT job,source_revision,target_count,base_count,cutoff,phase,seed_after,seeded,next_rank,pop,child_after,tail_after,conversation,order_kind FROM event_order_jobs WHERE domain=?1 AND (conversation,order_kind)>(?2,?3) ORDER BY conversation,order_kind LIMIT ?4";
impl EventRepository {
    /// Examines at most `limit` physical job rows, including ready/obsolete jobs.
    /// A full page retains a cursor; its successor may be empty. This is a live
    /// metadata observation, not a consistent generation snapshot across pages.
    pub(crate) fn observe_order_work(
        &self,
        after: Option<&OrderWorkCursor>,
        limit: usize,
    ) -> Result<OrderWorkPage, LogError> {
        if !(1..=64).contains(&limit) {
            return Err(LogError::InvalidPageBudget);
        }
        if after.is_some_and(|a| a.store_uuid != self.store_uuid || a.domain != self.domain) {
            return Err(LogError::StaleCursor);
        }
        self.database.read(|c| {
            let mut statement = c.prepare(SELECT_WORK)?;
            let mut rows = statement.query(params![
                self.domain.0.as_slice(),
                after.map_or(&[][..], |a| a.conversation.as_slice()),
                after.map_or(-1, |a| a.order.code()),
                limit as i64
            ])?;
            let mut items = Vec::with_capacity(limit);
            let mut next = None;
            while let Some(row) = rows.next()? {
                let job = Job::read(row)?;
                let conversation: [u8; 32] = blob(row, 12)?;
                let order = match row.get::<_, i64>(13)? {
                    0 => CausalOrder::Logical,
                    1 => CausalOrder::WallClock,
                    _ => return Err(corrupt("invalid observed order kind")),
                };
                let mut hash = Sha256::new();
                hash.update(self.store_uuid);
                hash.update(conversation);
                let mut scope_tag = [0; 8];
                scope_tag.copy_from_slice(&hash.finalize()[..8]);
                items.push(OrderWorkObservation {
                    job: job.id,
                    scope_tag,
                    order,
                    phase: match job.phase {
                        SEED => "seed",
                        RUN => "run",
                        TAIL => "tail",
                        READY => "ready",
                        OBSOLETE => "obsolete",
                        _ => unreachable!(),
                    },
                    revision: job.revision,
                    target_count: job.count,
                    base_count: job.base_count,
                    cutoff: job.cutoff,
                    seeded: job.seeded,
                    next_rank: job.next_rank,
                    seed_after: job.seed_after,
                    child_after: job.child_after,
                    tail_after: job.tail_after,
                    has_pop: job.pop.is_some(),
                });
                next = Some(OrderWorkCursor {
                    store_uuid: self.store_uuid,
                    domain: self.domain,
                    conversation,
                    order,
                });
            }
            if items.len() < limit {
                next = None;
            }
            Ok(OrderWorkPage { items, next })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::tests::{database, event};
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn order_work_observation_pages_reopen_and_fences_without_writes_or_bodies() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(&dir);
        let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        let identity = DeviceIdentity::generate();
        let first = event(&identity, 1, vec![], 1, EventKind::Message);
        repo.append(&first).unwrap();
        for order in [CausalOrder::Logical, CausalOrder::WallClock] {
            repo.ensure_order(&first.conversation_id, order).unwrap();
        }
        // The first physical row is ready; it must still consume a page slot.
        loop {
            if repo
                .order_step(
                    &first.conversation_id,
                    CausalOrder::Logical,
                    OrderWorkBudget::default(),
                )
                .unwrap()
                .readiness
                == OrderReadiness::Ready
            {
                break;
            }
        }
        let before = db.read(|c| Ok(c.total_changes())).unwrap();
        let bodies = repo.body_read_counts();
        let page = repo.observe_order_work(None, 1).unwrap();
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].phase, "ready");
        assert_eq!(page.items[0].order, CausalOrder::Logical);
        assert_eq!(page.items[0].target_count, 1);
        assert_eq!(page.items[0].next_rank, 1);
        let cursor = page.next.unwrap();
        let second = repo.observe_order_work(Some(&cursor), 1).unwrap();
        assert_eq!(second.items.len(), 1);
        assert_eq!(second.items[0].phase, "seed");
        assert_eq!(second.items[0].order, CausalOrder::WallClock);
        assert_ne!(page.items[0].job, second.items[0].job);
        assert_eq!(page.items[0].scope_tag, second.items[0].scope_tag);
        assert!(
            repo.observe_order_work(second.next.as_ref(), 1)
                .unwrap()
                .items
                .is_empty()
        );
        assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), before);
        assert_eq!(repo.body_read_counts(), bodies);
        assert!(matches!(
            repo.observe_order_work(None, 0),
            Err(LogError::InvalidPageBudget)
        ));
        assert!(matches!(
            repo.observe_order_work(None, 65),
            Err(LogError::InvalidPageBudget)
        ));
        let other_domain = EventRepository::new(db.clone(), EventDomainId([2; 16])).unwrap();
        assert!(matches!(
            other_domain.observe_order_work(Some(&cursor), 1),
            Err(LogError::StaleCursor)
        ));
        let other_dir = tempfile::tempdir().unwrap();
        let other_store = EventRepository::new(database(&other_dir), repo.domain).unwrap();
        assert!(matches!(
            other_store.observe_order_work(Some(&cursor), 1),
            Err(LogError::StaleCursor)
        ));
        drop(other_domain);
        drop(repo);
        drop(db);
        let reopened = EventRepository::new(database(&dir), EventDomainId([1; 16])).unwrap();
        let resumed = reopened.observe_order_work(Some(&cursor), 1).unwrap();
        assert_eq!(resumed.items[0].job, second.items[0].job);
        assert_eq!(resumed.items[0].scope_tag, second.items[0].scope_tag);
    }

    #[test]
    fn order_work_observation_propagates_corruption_with_indexed_bounded_selection() {
        let dir = tempfile::tempdir().unwrap();
        let db = database(&dir);
        let repo = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        let first = event(
            &DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repo.append(&first).unwrap();
        repo.ensure_order(&first.conversation_id, CausalOrder::Logical)
            .unwrap();
        db.read(|c| {
            let mut statement = c.prepare(&format!("EXPLAIN QUERY PLAN {SELECT_WORK}"))?;
            let plans = statement
                .query_map(params![repo.domain.0.as_slice(), &[][..], -1, 1], |r| {
                    r.get::<_, String>(3)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            assert!(
                plans
                    .iter()
                    .any(|p| p.contains("SEARCH event_order_jobs USING INDEX")
                        && p.contains("domain=?")),
                "{plans:?}"
            );
            assert!(
                plans
                    .iter()
                    .all(|p| !p.contains("TEMP B-TREE") && !p.contains("SCAN")),
                "{plans:?}"
            );
            Ok(())
        })
        .unwrap();
        // Cross-field invariant is deliberately not a schema CHECK.
        db.transaction(|c| {
            c.execute("UPDATE event_order_jobs SET seed_after=cutoff+1", [])?;
            Ok(())
        })
        .unwrap();
        assert!(repo.observe_order_work(None, 1).is_err());
    }
}

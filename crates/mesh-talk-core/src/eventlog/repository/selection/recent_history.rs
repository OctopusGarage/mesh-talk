//! Recent-message keyset pages over an already qualified outgoing selection.
//! Completing a page never certifies whole-history equality.
use super::*;
use crate::eventlog::sync::network::scan_limit;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct RecentCursor {
    selection: [u8; 16],
    binding: super::super::network::Binding,
    before: i64,
}
pub(crate) struct RecentIdPage {
    pub ids: Vec<EventId>,
    pub next: Option<RecentCursor>,
    pub examined: usize,
    pub positions: Vec<i64>,
}
impl EventRepository {
    pub(crate) fn selected_recent_message_page(
        &self,
        selection: &Selection,
        view: &NetworkView,
        after: Option<&RecentCursor>,
        budget: IdPageBudget,
    ) -> Result<RecentIdPage, LogError> {
        budget.validate()?;
        if after.is_some_and(|cursor| {
            cursor.selection != selection.id || cursor.binding != view.binding || cursor.before < 0
        }) {
            return Err(LogError::StaleCursor);
        }
        let before = after.map_or(i64::MAX, |cursor| cursor.before);
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            let mut statement = c.prepare_cached("SELECT r.id,r.rank,r.outgoing,e.kind FROM participant_work.participant_sync_rows r JOIN retained_events e ON e.domain=?2 AND e.id=r.id WHERE r.view=?1 AND r.rank<?3 AND e.conversation=?4 AND e.commit_ordinal<?5 ORDER BY r.rank DESC LIMIT ?6")?;
            let mut rows = statement.query(params![selection.id.as_slice(), self.domain.0.as_slice(), before, view.binding.conversation.as_bytes().as_slice(), view.binding.cutoff, scan_limit(budget.examined)?])?;
            let mut ids = Vec::new();
            let mut examined = 0;
            let mut positions = Vec::new();
            let mut cursor = before;
            while let Some(row) = rows.next()? {
                if examined == budget.examined || ids.len() == budget.ids {
                    return Ok(RecentIdPage { ids, examined, positions, next: Some(RecentCursor { selection: selection.id, binding: view.binding, before: cursor }) });
                }
                cursor = nonnegative(row.get(1)?)?;
                examined += 1;
                if row.get::<_, bool>(2)? && row.get::<_, i64>(3)? == EventKind::Message as i64 {
                    ids.push(EventId::new(blob(row, 0)?));
                    positions.push(cursor);
                }
            }
            Ok(RecentIdPage { ids, examined, positions, next: None })
        })
    }
}

impl RecentCursor {
    pub(crate) fn at(selection: &Selection, view: &NetworkView, before: i64) -> Self {
        Self {
            selection: selection.id,
            binding: view.binding,
            before,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::repository::tests::{database, event};
    use crate::eventlog::sync::network::NetworkSyncStore;
    struct Fixture {
        _directory: tempfile::TempDir,
        repo: EventRepository,
        view: NetworkView,
        selection: Selection,
        ids: Vec<EventId>,
    }
    fn fixture(outgoing: bool) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let repo = EventRepository::new(database(&directory), EventDomainId([1; 16])).unwrap();
        repo.initialize_selections().unwrap();
        let signer = crate::identity::device::DeviceIdentity::generate();
        let mut ids = Vec::new();
        for n in 1..=201 {
            let e = event(
                &signer,
                n,
                ids.last().copied().into_iter().collect(),
                n,
                EventKind::Message,
            );
            repo.append(&e).unwrap();
            ids.push(e.id);
        }
        let conversation = repo.get(&ids[0]).unwrap().unwrap().conversation_id;
        let view = repo.open_view(&conversation).unwrap();
        let selection = repo.begin_selection(&view, 4, 512).unwrap().unwrap();
        repo.database.selection_work_transaction(|c|{for (rank,id) in ids.iter().enumerate(){c.execute("INSERT INTO participant_work.participant_sync_rows(view,id,rank,candidate,selected,own,outgoing) VALUES(?1,?2,?3,1,1,1,?4)",params![selection.id.as_slice(),id.as_bytes().as_slice(),rank as i64,outgoing])?;}Ok(())}).unwrap();
        Fixture {
            _directory: directory,
            repo,
            view,
            selection,
            ids,
        }
    }
    fn budget() -> IdPageBudget {
        IdPageBudget {
            ids: 200,
            examined: 1000,
        }
    }
    #[test]
    fn recent_keyset_200_then_older_pins_source_cutoff_and_selection() {
        let f = fixture(true);
        let first = f
            .repo
            .selected_recent_message_page(&f.selection, &f.view, None, budget())
            .unwrap();
        assert_eq!(first.ids.len(), 200);
        assert_eq!(first.ids[0], f.ids[200]);
        assert_eq!(first.ids[199], f.ids[1]);
        let cursor = first.next.unwrap();
        let signer = crate::identity::device::DeviceIdentity::generate();
        let e = event(&signer, 1, vec![], 999, EventKind::Message);
        f.repo.append(&e).unwrap();
        let second = f
            .repo
            .selected_recent_message_page(&f.selection, &f.view, Some(&cursor), budget())
            .unwrap();
        assert_eq!(second.ids, vec![f.ids[0]]);
        assert!(second.next.is_none());
        let other = f.repo.begin_selection(&f.view, 4, 512).unwrap().unwrap();
        assert!(matches!(
            f.repo
                .selected_recent_message_page(&other, &f.view, Some(&cursor), budget()),
            Err(LogError::StaleCursor)
        ));
        let fresh = f.repo.open_view(&e.conversation_id).unwrap();
        assert!(matches!(
            f.repo
                .selected_recent_message_page(&f.selection, &fresh, Some(&cursor), budget()),
            Err(LogError::StaleCursor)
        ));
    }
    #[test]
    fn ordered_anchor_window_seeks_near_dependency_instead_of_replaying_old_prefix() {
        let f = fixture(true);
        let limit = ExportPageBudget {
            examined: 100,
            events: 10,
            encoded_bytes: 60 * 1024,
            allocated_bytes: 240 * 1024,
        };
        let (events, next, complete) = f
            .repo
            .selected_upload_rank_page(
                &f.selection,
                &f.view,
                -1,
                &[f.ids[180], f.ids[190]],
                201,
                limit,
            )
            .unwrap();
        assert_eq!(
            events.iter().map(|e| e.id).collect::<Vec<_>>(),
            f.ids[171..181]
        );
        assert_eq!(next, 180);
        assert!(!complete);
        let (events, next, complete) = f
            .repo
            .selected_upload_rank_page(&f.selection, &f.view, next, &[], 201, limit)
            .unwrap();
        assert_eq!(events[0].id, f.ids[181]);
        assert_eq!(next, 190);
        assert!(!complete);
        assert!(
            f.repo
                .selected_upload_rank_page(
                    &f.selection,
                    &f.view,
                    -1,
                    &[EventId::new([255; 32])],
                    201,
                    limit
                )
                .is_err()
        );
        let invalid = RecentCursor::at(&f.selection, &f.view, -1);
        assert!(matches!(
            f.repo
                .selected_recent_message_page(&f.selection, &f.view, Some(&invalid), budget()),
            Err(LogError::StaleCursor)
        ));
    }
    #[test]
    fn recent_filtered_empty_pages_advance_without_skipping_eligible_messages() {
        let f = fixture(false);
        f.repo.database.selection_work_transaction(|c|{c.execute("UPDATE participant_work.participant_sync_rows SET outgoing=1 WHERE view=?1 AND rank=0",[f.selection.id.as_slice()])?;Ok(())}).unwrap();
        let mut cursor = None;
        let mut collected = Vec::new();
        let mut pages = 0;
        loop {
            let page = f
                .repo
                .selected_recent_message_page(
                    &f.selection,
                    &f.view,
                    cursor.as_ref(),
                    IdPageBudget {
                        ids: 10,
                        examined: 10,
                    },
                )
                .unwrap();
            assert!(page.examined <= 10);
            if pages == 0 {
                assert!(page.ids.is_empty());
                assert!(page.next.is_some());
            }
            if let (Some(before), Some(next)) = (&cursor, &page.next) {
                assert!(next.before < before.before);
            }
            collected.extend(page.ids);
            pages += 1;
            cursor = page.next;
            if cursor.is_none() {
                break;
            }
            assert!(pages <= 22);
        }
        assert_eq!(collected, vec![f.ids[0]]);
    }
    #[test]
    fn recent_query_plan_uses_rank_keyset_without_sort_or_offset() {
        let f = fixture(true);
        let plans=f.repo.database.read(|c|{
            let mut stmt=c.prepare("EXPLAIN QUERY PLAN SELECT r.id,r.rank,r.outgoing,e.kind FROM participant_work.participant_sync_rows r JOIN retained_events e ON e.domain=?2 AND e.id=r.id WHERE r.view=?1 AND r.rank<?3 AND e.conversation=?4 AND e.commit_ordinal<?5 ORDER BY r.rank DESC LIMIT ?6")?;
            let rows=stmt.query_map(params![f.selection.id.as_slice(),f.repo.domain.0.as_slice(),i64::MAX,f.view.binding.conversation.as_bytes().as_slice(),f.view.binding.cutoff,1001],|r|r.get::<_,String>(3))?;
            rows.collect::<Result<Vec<_>,_>>().map_err(Into::into)
        }).unwrap();
        let plan = plans.join("\n");
        assert!(!plan.contains("TEMP B-TREE"), "{plan}");
        assert!(!plan.contains("SCAN r"), "{plan}");
        assert!(
            plan.contains("SEARCH r") && plan.contains("rank<?"),
            "{plan}"
        );
    }
}

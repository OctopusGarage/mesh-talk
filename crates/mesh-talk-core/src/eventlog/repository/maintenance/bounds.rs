//! One finite eligibility cutoff per pending Profile compaction request.
//! Raw rehydration remains ordinary retained data, not fresh destructive demand.
use super::*;
const SCHEMA: &str = "CREATE TABLE event_profile_request_bounds(domain BLOB NOT NULL CHECK(typeof(domain)='blob' AND length(domain)=16),conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32),through_commit INTEGER NOT NULL CHECK(typeof(through_commit)='integer' AND through_commit>=0),PRIMARY KEY(domain,conversation),FOREIGN KEY(domain,conversation) REFERENCES event_profile_requests(domain,conversation) ON DELETE CASCADE) WITHOUT ROWID;";
const NEXT_REQUEST_SQL: &str = "SELECT conversation FROM event_profile_requests WHERE domain=?1 AND conversation>COALESCE(?2,x'') ORDER BY conversation LIMIT 1";
const BOOTSTRAP_SCHEMA: &str = "CREATE TABLE event_profile_request_bootstrap(domain BLOB PRIMARY KEY CHECK(typeof(domain)='blob' AND length(domain)=16),after_conversation BLOB CHECK(after_conversation IS NULL OR (typeof(after_conversation)='blob' AND length(after_conversation)=32)),complete INTEGER NOT NULL CHECK(complete IN(0,1)),FOREIGN KEY(domain) REFERENCES event_domains(domain)) WITHOUT ROWID;";
fn ensure_on(c: &Connection) -> Result<(), LogError> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_profile_request_bounds')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    if !c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_profile_request_bootstrap')",
        [],
        |r| r.get::<_, bool>(0),
    )? {
        c.execute_batch(BOOTSTRAP_SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, BOOTSTRAP_SCHEMA)
}
impl EventRepository {
    pub(super) fn latest_profile_request_cut_on(&self, c: &Connection) -> Result<i64, LogError> {
        let next: i64 = c.query_row(
            "SELECT next_commit FROM event_domains WHERE domain=?1",
            [self.domain.0.as_slice()],
            |r| r.get(0),
        )?;
        nonnegative(next)?
            .checked_sub(1)
            .ok_or(LogError::RepositoryCounterExhausted)
    }
    pub(super) fn advance_profile_request_revision_on(
        &self,
        c: &Connection,
    ) -> Result<(), LogError> {
        let revision = advance(self.domain_maintenance_on(c)?.0)?;
        c.execute(
            "UPDATE event_domains SET source_revision=?2 WHERE domain=?1",
            params![self.domain.0.as_slice(), revision],
        )?;
        Ok(())
    }
    /// Before constructing a token, conservatively qualify a bounded batch of
    /// requests left by an older binary. Missing rows never authorize deletion.
    pub(super) fn migrate_profile_request_bounds_on(&self, c: &Connection) -> Result<(), LogError> {
        ensure_on(c)?;
        let cutoff = self.latest_profile_request_cut_on(c)?;
        c.execute(
            "INSERT OR IGNORE INTO event_profile_request_bootstrap VALUES(?1,NULL,0)",
            [self.domain.0.as_slice()],
        )?;
        let (after,complete):(Option<Vec<u8>>,bool)=c.query_row("SELECT after_conversation,complete FROM event_profile_request_bootstrap WHERE domain=?1",[self.domain.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if complete {
            return Ok(());
        }
        let next: Option<Vec<u8>> = c
            .query_row(
                NEXT_REQUEST_SQL,
                params![self.domain.0.as_slice(), after],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(conversation) = next {
            let inserted = c.execute(
                "INSERT OR IGNORE INTO event_profile_request_bounds VALUES(?1,?2,?3)",
                params![self.domain.0.as_slice(), conversation, cutoff],
            )?;
            if inserted != 0 {
                self.advance_profile_request_revision_on(c)?;
            }
            c.execute(
                "UPDATE event_profile_request_bootstrap SET after_conversation=?2 WHERE domain=?1",
                params![self.domain.0.as_slice(), conversation],
            )?;
        } else {
            c.execute(
                "UPDATE event_profile_request_bootstrap SET complete=1 WHERE domain=?1",
                [self.domain.0.as_slice()],
            )?;
        }
        Ok(())
    }
    pub(super) fn profile_request_bounds_ready_on(&self, c: &Connection) -> Result<bool, LogError> {
        c.query_row("SELECT COALESCE((SELECT complete FROM event_profile_request_bootstrap WHERE domain=?1),0)",[self.domain.0.as_slice()],|r|r.get(0)).map_err(Into::into)
    }
    pub(super) fn restart_profile_request_bounds_on(&self, c: &Connection) -> Result<(), LogError> {
        c.execute("UPDATE event_profile_request_bootstrap SET after_conversation=NULL,complete=0 WHERE domain=?1",[self.domain.0.as_slice()])?;
        Ok(())
    }
    pub(super) fn request_profile_maintenance_through_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        through: i64,
    ) -> Result<(), LogError> {
        if self.metadata_on(c, conv)?.is_none_or(|m| m.count == 0) {
            return Ok(());
        }
        ensure_on(c)?;
        let latest = self.latest_profile_request_cut_on(c)?;
        if through < 0 || through > latest {
            return Err(LogError::StaleCursor);
        }
        let added=c.execute("INSERT INTO event_profile_requests VALUES(?1,?2) ON CONFLICT(domain,conversation) DO NOTHING",params![self.domain.0.as_slice(),conv.as_bytes().as_slice()])?;
        // An old binary may have created this request without its sidecar. Its
        // previous unrestricted demand conservatively covers the current cut.
        let floor = if added == 0 { latest } else { through };
        let seeded=c.execute("INSERT INTO event_profile_request_bounds VALUES(?1,?2,?3) ON CONFLICT(domain,conversation) DO NOTHING",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),floor])?;
        let advanced=c.execute("UPDATE event_profile_request_bounds SET through_commit=?3 WHERE domain=?1 AND conversation=?2 AND through_commit<?3",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),through])?;
        if added != 0 || seeded != 0 || advanced != 0 {
            self.advance_profile_request_revision_on(c)?;
        }
        Ok(())
    }
    pub(super) fn profile_request_bound_on(
        &self,
        c: &Connection,
        conv: &[u8; 32],
    ) -> Result<Option<i64>, LogError> {
        c.query_row("SELECT through_commit FROM event_profile_request_bounds WHERE domain=?1 AND conversation=?2",params![self.domain.0.as_slice(),conv.as_slice()],|r|r.get(0)).optional().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::repository::tests::{database, event};
    use crate::identity::device::DeviceIdentity;
    fn setup() -> (
        tempfile::TempDir,
        EventRepository,
        DeviceIdentity,
        Vec<Event>,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let repository =
            EventRepository::new(database(&directory), EventDomainId([1; 16])).unwrap();
        let author = DeviceIdentity::generate();
        let mut events = Vec::new();
        for seq in 1..=12 {
            let item = event(&author, seq, vec![], seq, EventKind::Profile);
            repository.append(&item).unwrap();
            events.push(item);
        }
        (directory, repository, author, events)
    }
    #[test]
    fn profile_request_cutoff_advance_fences_existing_job_and_unchanged_demand_is_quiet() {
        let (_directory, repository, author, events) = setup();
        let conversation = events[0].conversation_id;
        repository
            .request_profile_maintenance(&conversation)
            .unwrap();
        let extra = event(&author, 13, vec![], 13, EventKind::Profile);
        repository.append(&extra).unwrap();
        // The worker starts after raw admission, before the semantic demand advance.
        let old = repository.begin_profile_maintenance().unwrap();
        repository
            .request_profile_maintenance(&conversation)
            .unwrap();
        assert_eq!(
            repository
                .step_profile_maintenance(old, PageBudget::default())
                .unwrap()
                .state,
            MaintenanceState::Stale
        );
        assert_eq!(
            repository.apply_profile_maintenance(old).unwrap().state,
            MaintenanceState::Stale
        );
        let changes = repository.database.read(|c| Ok(c.total_changes())).unwrap();
        repository
            .request_profile_maintenance(&conversation)
            .unwrap();
        assert_eq!(
            repository.database.read(|c| Ok(c.total_changes())).unwrap(),
            changes
        );
        assert_eq!(
            repository
                .database
                .read(|c| c
                    .query_row("SELECT count(*) FROM event_profile_requests", [], |r| r
                        .get::<_, i64>(0))
                    .map_err(Into::into))
                .unwrap(),
            1
        );
    }
    #[test]
    fn profile_request_cutoff_legacy_backfill_is_one_row_and_preserves_pending_demand() {
        let (_directory, repository, _author, events) = setup();
        let conversation = events[0].conversation_id;
        repository
            .request_profile_maintenance(&conversation)
            .unwrap();
        repository
            .database
            .transaction(|c| {
                c.execute("DELETE FROM event_profile_request_bounds", [])?;
                Ok(())
            })
            .unwrap();
        let before = repository.source_revision().unwrap();
        let token = repository.begin_profile_maintenance().unwrap();
        assert!(token.revision > before);
        assert_eq!(
            repository
                .database
                .read(|c| c
                    .query_row(
                        "SELECT count(*) FROM event_profile_request_bounds",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .map_err(Into::into))
                .unwrap(),
            1
        );
        repository.begin_profile_maintenance().unwrap();
        let changes = repository.database.read(|c| Ok(c.total_changes())).unwrap();
        repository.begin_profile_maintenance().unwrap();
        assert_eq!(
            repository.database.read(|c| Ok(c.total_changes())).unwrap(),
            changes
        );
        // Deleting a consumed request cannot leave an orphan eligibility epoch.
        repository
            .database
            .transaction(|c| {
                c.execute("DELETE FROM event_profile_requests", [])?;
                Ok(())
            })
            .unwrap();
        assert_eq!(
            repository
                .database
                .read(|c| c
                    .query_row(
                        "SELECT count(*) FROM event_profile_request_bounds",
                        [],
                        |r| r.get::<_, i64>(0)
                    )
                    .map_err(Into::into))
                .unwrap(),
            0
        );
    }
    #[test]
    fn profile_request_cutoff_late_raw_rows_are_charged_but_not_candidates() {
        let (_directory, repository, author, events) = setup();
        let conversation = events[0].conversation_id;
        repository
            .request_profile_maintenance(&conversation)
            .unwrap();
        let mut late = Vec::new();
        for seq in 13..=22 {
            let item = event(&author, seq, vec![], seq, EventKind::Profile);
            repository.append(&item).unwrap();
            late.push(item.id);
        }
        let token = repository.begin_profile_maintenance().unwrap();
        let mut examined = 0;
        let mut ready = false;
        for _ in 0..256 {
            let step = repository
                .step_profile_maintenance(
                    token,
                    PageBudget {
                        rows: 1,
                        bytes: 512,
                    },
                )
                .unwrap();
            assert!(step.examined <= 1);
            assert!(step.charged_bytes <= 512);
            examined += step.examined;
            if step.state == MaintenanceState::Ready {
                assert_eq!(step.candidates, 10);
                ready = true;
                break;
            }
        }
        assert!(ready);
        assert!(
            examined >= 22,
            "late rows must consume explicit scan budget"
        );
        assert_eq!(
            repository.apply_profile_maintenance(token).unwrap().dropped,
            10
        );
        for id in late {
            assert!(repository.header(&id).unwrap().is_some());
        }
    }
    #[test]
    fn profile_request_cutoff_bootstrap_seeks_one_row_through_qualified_prefix() {
        let (_directory, repository, author, events) = setup();
        repository
            .request_profile_maintenance(&events[0].conversation_id)
            .unwrap();
        repository.begin_profile_maintenance().unwrap();
        repository
            .database
            .transaction(|c| {
                c.execute("DELETE FROM event_profile_requests", [])?;
                repository.restart_profile_request_bounds_on(c)
            })
            .unwrap();
        for n in 1..=101u8 {
            let conv = ConversationId::new([n; 32]);
            let sequence = 13 + u64::from(n);
            let item = Event::new(
                &author,
                conv,
                sequence,
                vec![],
                sequence,
                0,
                EventKind::Profile,
                vec![1],
            );
            repository.append(&item).unwrap();
            repository
                .database
                .transaction(|c| {
                    c.execute(
                        "INSERT INTO event_profile_requests VALUES(?1,?2)",
                        params![repository.domain.0.as_slice(), conv.as_bytes().as_slice()],
                    )?;
                    if n < 101 {
                        let cut = repository.latest_profile_request_cut_on(c)?;
                        c.execute(
                            "INSERT INTO event_profile_request_bounds VALUES(?1,?2,?3)",
                            params![
                                repository.domain.0.as_slice(),
                                conv.as_bytes().as_slice(),
                                cut
                            ],
                        )?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        let before = repository.source_revision().unwrap();
        for n in 1..=100u8 {
            repository.begin_profile_maintenance().unwrap();
            let cursor:Vec<u8>=repository.database.read(|c|c.query_row("SELECT after_conversation FROM event_profile_request_bootstrap WHERE domain=?1",[repository.domain.0.as_slice()],|r|r.get(0)).map_err(Into::into)).unwrap();
            assert_eq!(cursor, vec![n; 32]);
            assert_eq!(
                repository.source_revision().unwrap(),
                before,
                "qualified bootstrap rows cannot invalidate candidate work"
            );
        }
        repository
            .database
            .read(|c| {
                let mut stmt = c.prepare(NEXT_REQUEST_SQL)?;
                let next: Vec<u8> = stmt.query_row(
                    params![repository.domain.0.as_slice(), [100u8; 32].as_slice()],
                    |r| r.get(0),
                )?;
                assert_eq!(next, vec![101; 32]);
                assert!(
                    stmt.get_status(rusqlite::StatementStatus::VmStep) < 80,
                    "bootstrap seek must not filter through the qualified prefix"
                );
                Ok(())
            })
            .unwrap();
        repository.begin_profile_maintenance().unwrap();
        assert!(repository.source_revision().unwrap() > before);
        assert!(
            !repository
                .database
                .read(|c| repository.profile_request_bounds_ready_on(c))
                .unwrap()
        );
        repository.begin_profile_maintenance().unwrap();
        assert!(
            repository
                .database
                .read(|c| repository.profile_request_bounds_ready_on(c))
                .unwrap()
        );
    }
    #[test]
    fn profile_request_cutoff_foreign_and_stale_tokens_cannot_advance_bootstrap() {
        let (_directory, repository, author, events) = setup();
        repository
            .request_profile_maintenance(&events[0].conversation_id)
            .unwrap();
        let token = repository.begin_profile_maintenance().unwrap();
        assert!(
            !repository
                .database
                .read(|c| repository.profile_request_bounds_ready_on(c))
                .unwrap()
        );
        let mut foreign = token;
        foreign.store_uuid = [213; 16];
        let before = repository.database.read(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            repository
                .step_profile_maintenance(foreign, PageBudget::default())
                .unwrap()
                .state,
            MaintenanceState::Stale
        );
        assert_eq!(
            repository.database.read(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        repository
            .append(&event(&author, 13, vec![], 13, EventKind::Profile))
            .unwrap();
        let before = repository.database.read(|c| Ok(c.total_changes())).unwrap();
        assert_eq!(
            repository
                .step_profile_maintenance(token, PageBudget::default())
                .unwrap()
                .state,
            MaintenanceState::Stale
        );
        assert_eq!(
            repository.database.read(|c| Ok(c.total_changes())).unwrap(),
            before
        );
        assert!(
            !repository
                .database
                .read(|c| repository.profile_request_bounds_ready_on(c))
                .unwrap()
        );
    }
}

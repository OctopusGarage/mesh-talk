//! Disposable participant authorization selections in encrypted runtime scratch.
//! Rows contain identities and scalar policy facts, never event bodies. The
//! exchange owner supplies its existing active-query limit. Idle node indexes
//! park their runtime permits under a separate bounded cache; SQL ownership and
//! authority checks remain unchanged. Legacy MAIN indices and retired runtime
//! work drain under one bounded allowance before another selection is admitted.
//! Restart reconstructs scratch; durable policy/classification remains in MAIN.
use super::network::{ExportCursor, HaveCursor, NetworkView};
use super::*;
use crate::eventlog::sync::network::{ExportPage, ExportPageBudget, IdPageBudget, SyncPage};
use std::collections::HashSet;

mod export;
mod metadata;
mod preparation;
pub(crate) use preparation::{AncestryProgress, SelectionRow};

const STEP: usize = 512;
const DDL: &str = "
CREATE TABLE IF NOT EXISTS participant_sync_views(
 id BLOB PRIMARY KEY CHECK(length(id)=16), owner BLOB NOT NULL CHECK(length(owner)=16),
 domain BLOB NOT NULL CHECK(length(domain)=16), retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN (0,1))
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS participant_sync_retired ON participant_sync_views(domain,retired,owner,id);
CREATE TABLE IF NOT EXISTS participant_sync_rows(
 view BLOB NOT NULL, id BLOB NOT NULL CHECK(length(id)=32), rank INTEGER NOT NULL CHECK(rank>=0),
 candidate INTEGER NOT NULL, selected INTEGER NOT NULL, own INTEGER NOT NULL,
 sibling INTEGER NOT NULL DEFAULT 0, outgoing INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(view,id), UNIQUE(view,rank), FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_have(
 view BLOB NOT NULL, id BLOB NOT NULL CHECK(length(id)=32),
 PRIMARY KEY(view,id), FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_ancestors(
 view BLOB NOT NULL,id BLOB NOT NULL CHECK(length(id)=32),PRIMARY KEY(view,id),FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_departures(
 view BLOB NOT NULL,pass INTEGER NOT NULL,member TEXT NOT NULL,event BLOB NOT NULL CHECK(length(event)=32),PRIMARY KEY(view,pass,member,event),FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_authors(
 view BLOB NOT NULL,author BLOB NOT NULL CHECK(length(author)=32),PRIMARY KEY(view,author),FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_progress(
 view BLOB PRIMARY KEY,context BLOB NOT NULL,FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_sync_receipts(
 view BLOB NOT NULL, id BLOB NOT NULL CHECK(length(id)=32),
 PRIMARY KEY(view,id), FOREIGN KEY(view) REFERENCES participant_sync_views(id)
) WITHOUT ROWID;";

struct SelectionAdmission(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl SelectionAdmission {
    fn acquire(
        counter: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        limit: usize,
    ) -> Result<Self, LogError> {
        use std::sync::atomic::Ordering;
        i64::try_from(limit).map_err(|_| LogError::InvalidPageBudget)?;
        let mut active = counter.load(Ordering::Acquire);
        loop {
            if active >= limit {
                return Err(LogError::AdmissionRejected(
                    "participant sync connection capacity exhausted".into(),
                ));
            }
            match counter.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => active = current,
            }
        }
        Ok(Self(counter.clone()))
    }
}
impl Drop for SelectionAdmission {
    fn drop(&mut self) {
        let previous = self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}

pub(crate) struct Selection {
    id: [u8; 16],
    domain: EventDomainId,
    owner: [u8; 16],
    database: RepositoryOwner,
    admission: Option<SelectionAdmission>,
    admission_counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    admission_limit: usize,
}
impl Selection {
    /// Idle derived indexes do not consume an active query permit.
    pub(crate) fn park(&mut self) {
        drop(self.admission.take());
    }
    pub(crate) fn activate(&mut self) -> Result<(), LogError> {
        if self.admission.is_none() {
            self.admission = Some(SelectionAdmission::acquire(
                &self.admission_counter,
                self.admission_limit,
            )?);
        }
        Ok(())
    }
}
impl Drop for Selection {
    fn drop(&mut self) {
        let result = self.database.selection_work_transaction(|tx| {
            tx.execute(
                "UPDATE participant_work.participant_sync_views SET retired=1 WHERE id=?1 AND owner=?2",
                params![self.id.as_slice(), self.owner.as_slice()],
            )?;
            cleanup_on(tx, self.domain, self.owner, STEP)
        });
        // Only deferred reclamation needs another local turn. This must never
        // publish a network source change, and runs after the SQL guard retires.
        if !matches!(&result, Ok(false)) {
            self.database.maintenance_waker().notify();
        }
        if let Err(error) = result {
            log::warn!("participant sync selection cleanup deferred: {error}");
        }
    }
}
#[derive(Clone, Copy)]
enum SelectionStorage {
    LegacyMain,
    Runtime,
}
impl SelectionStorage {
    fn schema(self) -> &'static str {
        match self {
            Self::LegacyMain => "main",
            Self::Runtime => "participant_work",
        }
    }
}

fn initialize_selection_schema(c: &Connection, storage: SelectionStorage) -> Result<(), LogError> {
    for ddl in [DDL, export::DDL] {
        // These are closed schema identifiers, never protocol or user input.
        // Only declarations are qualified; foreign keys stay within that schema.
        c.execute_batch(&ddl.replace(
            "IF NOT EXISTS participant_sync_",
            &format!("IF NOT EXISTS {}.participant_sync_", storage.schema()),
        ))?;
    }
    Ok(())
}

fn cleanup_on(
    c: &Connection,
    domain: EventDomainId,
    owner: [u8; 16],
    limit: usize,
) -> Result<bool, LogError> {
    cleanup_storage_on(c, SelectionStorage::Runtime, domain, owner, limit)
}

fn cleanup_pending_on(
    c: &Connection,
    domain: EventDomainId,
    owner: [u8; 16],
    limit: usize,
) -> Result<bool, LogError> {
    // Legacy and live scratch retirement share one allowance. A large old
    // index yields between slices instead of blocking startup or being orphaned.
    if cleanup_storage_on(c, SelectionStorage::LegacyMain, domain, owner, limit)? {
        return Ok(true);
    }
    cleanup_on(c, domain, owner, limit)
}

fn cleanup_storage_on(
    c: &Connection,
    storage: SelectionStorage,
    domain: EventDomainId,
    owner: [u8; 16],
    limit: usize,
) -> Result<bool, LogError> {
    crate::eventlog::sync::network::scan_limit(limit)?;
    let schema = storage.schema();
    let id: Option<Vec<u8>> = c.query_row(&format!("SELECT id FROM {schema}.participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2) LIMIT 1"), params![domain.0.as_slice(),owner.as_slice()], |r|r.get(0)).optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    let mut remaining = limit;
    for (table, key) in [
        ("participant_sync_rows", "id"),
        ("participant_sync_have", "id"),
        ("participant_sync_receipts", "id"),
        ("participant_sync_ancestors", "id"),
        ("participant_sync_departures", "pass,member,event"),
        ("participant_sync_authors", "author"),
    ] {
        let removed = c.execute(&format!("DELETE FROM {schema}.{table} WHERE view=?1 AND ({key}) IN (SELECT {key} FROM {schema}.{table} WHERE view=?1 LIMIT ?2)"), params![&id, remaining as i64])?;
        remaining -= removed;
        if remaining == 0 {
            return Ok(true);
        }
    }
    for (table, key) in [
        ("participant_sync_export_work", "view"),
        ("participant_sync_progress", "view"),
        ("participant_sync_views", "id"),
    ] {
        remaining -= c.execute(
            &format!("DELETE FROM {schema}.{table} WHERE {key}=?1"),
            [&id],
        )?;
        if remaining == 0 {
            return Ok(true);
        }
    }
    Ok(true)
}

impl EventRepository {
    #[cfg(test)]
    pub(crate) fn active_selection_count(&self) -> usize {
        self.selection_admission
            .load(std::sync::atomic::Ordering::Acquire)
    }
    #[cfg(test)]
    pub(crate) fn selection_counts(&self) -> Result<(usize, usize, usize), LogError> {
        self.database.read(|c| {
            Ok((
                c.query_row(
                    "SELECT count(*) FROM participant_work.participant_sync_views",
                    [],
                    |r| r.get::<_, i64>(0).map(|n| n as usize),
                )?,
                c.query_row(
                    "SELECT count(*) FROM participant_work.participant_sync_rows",
                    [],
                    |r| r.get::<_, i64>(0).map(|n| n as usize),
                )?,
                c.query_row(
                    "SELECT count(*) FROM participant_work.participant_sync_have",
                    [],
                    |r| r.get::<_, i64>(0).map(|n| n as usize),
                )?,
            ))
        })
    }
    pub(crate) fn initialize_selections(&self) -> Result<(), LogError> {
        self.database.transaction(|tx| {
            // Preserve the old schema for compatible readers; only its bounded
            // retirement path mutates these legacy MAIN tables now.
            initialize_selection_schema(tx, SelectionStorage::LegacyMain)?;
            tx.execute_batch(metadata::DDL)?;
            initialize_selection_schema(tx, SelectionStorage::Runtime)?;
            tx.execute(
                "INSERT OR IGNORE INTO participant_kind_scan VALUES(?1,-1)",
                [self.domain.0.as_slice()],
            )?;
            cleanup_pending_on(tx, self.domain, self.selection_owner, STEP).map(|_| ())
        })
    }
    pub(crate) fn cleanup_selections(&self) -> Result<bool, LogError> {
        // Empty maintenance is read-only. Installing the write guard would
        // expire cached SQL plans even when no selection needs reclamation.
        // A retirement after this observation is safe to defer to its owner or
        // the next bounded cleanup pass; this is not an authority decision.
        let pending = self.selection_cleanup_pending()?;
        if !pending {
            return Ok(false);
        }
        self.database.selection_work_transaction(|tx| {
            cleanup_pending_on(tx, self.domain, self.selection_owner, STEP)
        })
    }
    pub(crate) fn selection_cleanup_pending(&self) -> Result<bool, LogError> {
        self.database.read(|c| {
            Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2) UNION ALL SELECT 1 FROM main.participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2))", params![self.domain.0.as_slice(),self.selection_owner.as_slice()], |r|r.get::<_,bool>(0))?)
        })
    }
    pub(crate) fn activate_cached_selection(
        &self,
        selection: &mut Selection,
    ) -> Result<(), LogError> {
        if selection.owner != self.selection_owner
            || selection.domain != self.domain
            || !std::sync::Arc::ptr_eq(&selection.admission_counter, &self.selection_admission)
        {
            return Err(LogError::StaleCursor);
        }
        selection.activate()
    }

    /// Charge disposable IDs/scalar work, including variable checkpoint bytes.
    /// This indexed accounting runs only after preparation/export state changes.
    pub(crate) fn selection_cache_bytes(&self, selection: &Selection) -> Result<usize, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            // Conservative fixed page/index overhead per cached projection.
            let mut bytes = 64_usize * 1024;
            for table in ["participant_work.participant_sync_rows", "participant_work.participant_sync_have", "participant_work.participant_sync_receipts",
                "participant_work.participant_sync_ancestors", "participant_work.participant_sync_departures", "participant_work.participant_sync_authors",
                "participant_work.participant_sync_export_work", "participant_work.participant_sync_progress"] {
                let rows: i64 = c.query_row(&format!("SELECT count(*) FROM {table} WHERE view=?1"),
                    [selection.id.as_slice()], |r| r.get(0))?;
                let charge = usize::try_from(rows).ok().and_then(|rows| rows.checked_mul(256))
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                bytes = bytes.checked_add(charge).ok_or(LogError::RepositoryCounterExhausted)?;
            }
            let context: i64 = c.query_row("SELECT coalesce(sum(length(context)),0) FROM participant_work.participant_sync_progress WHERE view=?1",
                [selection.id.as_slice()], |r| r.get(0))?;
            bytes.checked_add(usize::try_from(context).map_err(|_| LogError::RepositoryCounterExhausted)?)
                .ok_or(LogError::RepositoryCounterExhausted)
        })
    }

    pub(crate) fn source_revision(&self) -> Result<i64, LogError> {
        self.database.read(|c| {
            nonnegative(c.query_row(
                "SELECT source_revision FROM event_domains WHERE domain=?1",
                [self.domain.0.as_slice()],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn selected_have_page(
        &self,
        selection: &Selection,
        view: &NetworkView,
        after: Option<&HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, HaveCursor>, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            let mut page = self.network_have_page_on(c, view, after, budget)?;
            let mut stmt = c.prepare(
                "SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_rows WHERE view=?1 AND id=?2)",
            )?;
            let mut allowed = Vec::new();
            for id in page.items {
                if stmt.query_row(
                    params![selection.id.as_slice(), id.as_bytes().as_slice()],
                    |r| r.get::<_, bool>(0),
                )? {
                    allowed.push(id);
                }
            }
            page.items = allowed;
            Ok(page)
        })
    }
    fn require_selection_on(&self, c: &Connection, selection: &Selection) -> Result<(), LogError> {
        if selection.admission.is_none() {
            return Err(LogError::StaleCursor);
        }
        let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_views WHERE id=?1 AND owner=?2 AND domain=?3 AND retired=0)",params![selection.id.as_slice(),self.selection_owner.as_slice(),self.domain.0.as_slice()],|r|r.get(0))?;
        if !valid {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn selected_export_page(
        &self,
        selection: &Selection,
        view: &NetworkView,
        have: &HashSet<EventId>,
        after: Option<&ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<ExportCursor>, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            self.network_export_selected_on(c, view, have, after, budget, Some(&selection.id))
        })
    }

    /// Read only the finite selected rank suffix after the existing export
    /// closure worker has committed its outgoing flags. Gaps and rejected rows
    /// advance the cursor without loading bodies or replaying the old prefix.
    pub(crate) fn selected_upload_id_rank_page(
        &self,
        selection: &Selection,
        view: &NetworkView,
        after_rank: i64,
        upper: u64,
        budget: IdPageBudget,
    ) -> Result<(Vec<EventId>, i64, bool), LogError> {
        budget.validate()?;
        let upper = i64::try_from(upper).map_err(|_| LogError::RepositoryCounterExhausted)?;
        if after_rank < -1 || after_rank >= upper.max(0) {
            return Err(LogError::StaleCursor);
        }
        self.database.read(|c| {
            self.require_selection_on(c,selection)?;
            self.validate_network_on(c,view)?;
            let mut stmt=c.prepare_cached("SELECT id,rank,outgoing FROM participant_work.participant_sync_rows WHERE view=?1 AND rank>?2 AND rank<?3 ORDER BY rank LIMIT ?4")?;
            let mut rows=stmt.query(params![selection.id.as_slice(),after_rank,upper,crate::eventlog::sync::network::scan_limit(budget.examined)?])?;
            let mut ids=Vec::new();let mut examined=0;let mut cursor=after_rank;
            while let Some(row)=rows.next()? {
                if examined==budget.examined || ids.len()==budget.ids { return Ok((ids,cursor,false)); }
                if row.get::<_,bool>(2)? {ids.push(EventId::new(blob(row,0)?));}
                cursor=nonnegative(row.get(1)?)?;examined+=1;
            }
            Ok((ids,upper-1,true))
        })
    }

    pub(crate) fn selected_upload_contains(
        &self,
        selection: &Selection,
        view: &NetworkView,
        id: EventId,
        upper: u64,
    ) -> Result<bool, LogError> {
        let upper = i64::try_from(upper).map_err(|_| LogError::RepositoryCounterExhausted)?;
        self.database.read(|c| {
            self.require_selection_on(c,selection)?;
            self.validate_network_on(c,view)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_rows WHERE view=?1 AND id=?2 AND rank<?3 AND outgoing=1)",params![selection.id.as_slice(),id.as_bytes().as_slice(),upper],|r|r.get(0)).map_err(Into::into)
        })
    }

    pub(crate) fn selected_upload_event(
        &self,
        selection: &Selection,
        view: &NetworkView,
        id: EventId,
        upper: u64,
        max_bytes: usize,
    ) -> Result<Option<Event>, LogError> {
        if max_bytes == 0 {
            return Err(LogError::InvalidPageBudget);
        }
        let upper = i64::try_from(upper).map_err(|_| LogError::RepositoryCounterExhausted)?;
        self.database.read(|c| {
            self.require_selection_on(c,selection)?;
            self.validate_network_on(c,view)?;
            let allowed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_rows WHERE view=?1 AND id=?2 AND rank<?3 AND outgoing=1)",params![selection.id.as_slice(),id.as_bytes().as_slice(),upper],|r|r.get(0))?;
            if !allowed {return Ok(None);}
            let header=self.network_header_on(c,&id)?;
            if header.conversation!=view.binding.conversation {return Err(corrupt("selected upload conversation differs"));}
            if header.encoded_bytes>max_bytes {return Err(LogError::PageBudgetTooSmall{required:header.encoded_bytes});}
            self.get_on(c,&id)?.map(Some).ok_or_else(||corrupt("selected upload body disappeared"))
        })
    }

    #[cfg(test)]
    pub(crate) fn selected_upload_rank_page(
        &self,
        selection: &Selection,
        view: &NetworkView,
        after_rank: i64,
        upper: u64,
        budget: ExportPageBudget,
    ) -> Result<(Vec<Event>, i64, bool), LogError> {
        budget.validate()?;
        let upper = i64::try_from(upper).map_err(|_| LogError::RepositoryCounterExhausted)?;
        if after_rank < -1 || after_rank >= upper.max(0) {
            return Err(LogError::StaleCursor);
        }
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            let mut stmt = c.prepare_cached("SELECT id,rank,outgoing FROM participant_work.participant_sync_rows WHERE view=?1 AND rank>?2 AND rank<?3 ORDER BY rank LIMIT ?4")?;
            let mut rows = stmt.query(params![selection.id.as_slice(),after_rank,upper,crate::eventlog::sync::network::scan_limit(budget.examined)?])?;
            let mut events = Vec::new();
            let mut examined = 0;
            let mut cursor = after_rank;
            let mut encoded_bytes = 0usize;
            let mut allocated_bytes = 0usize;
            while let Some(row) = rows.next()? {
                if examined == budget.examined || events.len() == budget.events {
                    return Ok((events, cursor, false));
                }
                let id = EventId::new(blob(row, 0)?);
                let rank = nonnegative(row.get(1)?)?;
                if row.get::<_, bool>(2)? {
                    let header = self.network_header_on(c, &id)?;
                    if header.conversation != view.binding.conversation {
                        return Err(corrupt("selected upload conversation differs"));
                    }
                    let allocation = header.stored_bytes.checked_mul(3)
                        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<Event>()))
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    let encoded = encoded_bytes.checked_add(header.encoded_bytes)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    let allocated = allocated_bytes.checked_add(allocation)
                        .ok_or(LogError::RepositoryCounterExhausted)?;
                    if encoded > budget.encoded_bytes || allocated > budget.allocated_bytes {
                        if events.is_empty() {
                            return Err(LogError::PageBudgetTooSmall { required:
                                if encoded > budget.encoded_bytes { header.encoded_bytes } else { allocation }
                            });
                        }
                        return Ok((events, cursor, false));
                    }
                    let event = self.get_on(c, &id)?
                        .ok_or_else(|| corrupt("selected upload body disappeared"))?;
                    events.reserve_exact(1);
                    events.push(event);
                    encoded_bytes = encoded;
                    allocated_bytes = allocated;
                }
                examined += 1;
                cursor = rank;
            }
            Ok((events, upper - 1, true))
        })
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;

    #[test]
    fn selection_retirement_wakes_local_cleanup_without_network_source_change() {
        use super::super::tests::{database, event};
        use crate::eventlog::sync::network::NetworkSyncStore;
        let directory = tempfile::tempdir().unwrap();
        let db = database(&directory);
        let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        repository.initialize_selections().unwrap();
        let message = event(
            &crate::identity::device::DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repository.append(&message).unwrap();
        let view = NetworkSyncStore::open_view(&repository, &message.conversation_id).unwrap();
        let selection = repository.begin_selection(&view, 1, 512).unwrap().unwrap();
        db.selection_work_transaction(|c| {
            for index in 0..STEP+1 {
                let mut id=[0xffu8;32];id[..8].copy_from_slice(&(index as u64).to_le_bytes());
                c.execute("INSERT INTO participant_work.participant_sync_rows(view,id,rank,candidate,selected,own) VALUES(?1,?2,?3,0,0,0)",params![selection.id.as_slice(),id.as_slice(),(index+10000) as i64])?;
            }
            Ok(())
        }).unwrap();
        let maintenance = db.subscribe_maintenance_changes();
        let source = db.subscribe_source_changes();
        drop(selection);
        assert!(maintenance.has_changed().unwrap());
        assert!(!source.has_changed().unwrap());
        assert!(repository.selection_cleanup_pending().unwrap());
        for _ in 0..8 {
            if !repository.cleanup_selections().unwrap() {
                break;
            }
        }
        assert!(!repository.selection_cleanup_pending().unwrap());
    }

    #[test]
    fn empty_selection_cleanup_preserves_cached_read_plans() {
        use super::super::tests::database;
        let directory = tempfile::tempdir().unwrap();
        let db = database(&directory);
        let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        repository.initialize_selections().unwrap();
        let reprepare = |reset| {
            db.read(|c| {
                let mut statement = c.prepare_cached(
                    "SELECT count(*) FROM participant_work.participant_sync_views",
                )?;
                let _: i64 = statement.query_row([], |r| r.get(0))?;
                Ok(if reset {
                    statement.reset_status(rusqlite::StatementStatus::RePrepare)
                } else {
                    statement.get_status(rusqlite::StatementStatus::RePrepare)
                })
            })
            .unwrap()
        };
        assert_eq!(reprepare(false), 0);
        // Positive control: authorizer changes really expire the retained VM.
        db.selection_work_transaction(|_| Ok(())).unwrap();
        assert!(reprepare(true) > 0);
        for _ in 0..8 {
            assert!(!repository.cleanup_selections().unwrap());
        }
        assert_eq!(
            reprepare(false),
            0,
            "empty recovery must not force fresh SQL compilation"
        );
    }

    #[test]
    fn parked_selection_preserves_active_capacity_owner_and_quiet_checkout() {
        use super::super::tests::{database, event};
        use crate::eventlog::sync::network::NetworkSyncStore;
        let directory = tempfile::tempdir().unwrap();
        let db = database(&directory);
        let repository = EventRepository::new(db.clone(), EventDomainId([1; 16])).unwrap();
        repository.initialize_selections().unwrap();
        let message = event(
            &crate::identity::device::DeviceIdentity::generate(),
            1,
            vec![],
            1,
            EventKind::Message,
        );
        repository.append(&message).unwrap();
        let view = NetworkSyncStore::open_view(&repository, &message.conversation_id).unwrap();
        let mut first = repository.begin_selection(&view, 1, 512).unwrap().unwrap();
        assert_eq!(repository.active_selection_count(), 1);
        assert!(repository.begin_selection(&view, 1, 512).is_err());
        let charge = repository.selection_cache_bytes(&first).unwrap();
        assert!(charge >= 64 * 1024);
        first.park();
        assert_eq!(repository.active_selection_count(), 0);
        assert!(matches!(
            repository.selected_contains(&first, message.id),
            Err(LogError::StaleCursor)
        ));
        let second = repository.begin_selection(&view, 1, 512).unwrap().unwrap();
        assert!(matches!(
            repository.activate_cached_selection(&mut first),
            Err(LogError::AdmissionRejected(_))
        ));
        drop(second);
        let changes = db.read(|c| Ok(c.total_changes())).unwrap();
        let commits = db.successful_write_commits();
        for _ in 0..8 {
            repository.activate_cached_selection(&mut first).unwrap();
            assert_eq!(repository.active_selection_count(), 1);
            assert!(!repository.selected_contains(&first, message.id).unwrap());
            first.park();
        }
        assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), changes);
        assert_eq!(db.successful_write_commits(), commits);
        let other = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
        assert!(matches!(
            other.activate_cached_selection(&mut first),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(other.active_selection_count(), 0);
        drop(first);
        assert_eq!(repository.active_selection_count(), 0);
    }

    #[test]
    fn legacy_and_runtime_selection_retirement_share_one_allowance() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON; ATTACH DATABASE ':memory:' AS participant_work")
            .unwrap();
        initialize_selection_schema(&c, SelectionStorage::LegacyMain).unwrap();
        initialize_selection_schema(&c, SelectionStorage::Runtime).unwrap();
        let domain = EventDomainId([13; 16]);
        let current = [14u8; 16];
        for (schema, id, owner) in [
            ("main", [15u8; 16], [16u8; 16]),
            ("participant_work", [17u8; 16], current),
        ] {
            c.execute(
                &format!("INSERT INTO {schema}.participant_sync_views VALUES(?1,?2,?3,1)"),
                params![id.as_slice(), owner.as_slice(), domain.0.as_slice()],
            )
            .unwrap();
            for rank in 0..2u8 {
                c.execute(
                    &format!(
                        "INSERT INTO {schema}.participant_sync_rows VALUES(?1,?2,?3,1,1,0,0,0)"
                    ),
                    params![id.as_slice(), [rank; 32].as_slice(), rank],
                )
                .unwrap();
            }
        }
        let mut retired = 0;
        while retired < 6 {
            let before = c.total_changes();
            assert!(cleanup_pending_on(&c, domain, current, 2).unwrap());
            let removed = c.total_changes() - before;
            assert!(
                (1..=2).contains(&removed),
                "the two schemas share one row allowance"
            );
            retired += removed;
        }
        assert_eq!(retired, 6);
        let before = c.total_changes();
        assert!(!cleanup_pending_on(&c, domain, current, 2).unwrap());
        assert_eq!(
            c.total_changes(),
            before,
            "completed migration and retirement are read-only"
        );
    }

    #[test]
    fn participant_selection_retirement_shares_one_limit_across_all_tables() {
        for limit in [1usize, 2, 5, 64] {
            let c = Connection::open_in_memory().unwrap();
            c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            c.execute_batch("ATTACH DATABASE ':memory:' AS participant_work")
                .unwrap();
            initialize_selection_schema(&c, SelectionStorage::Runtime).unwrap();
            let domain = EventDomainId([3; 16]);
            let owner = [4; 16];
            let retired = [1u8; 16];
            let active = [2u8; 16];
            for (id, is_retired) in [(retired, true), (active, false)] {
                c.execute(
                    "INSERT INTO participant_work.participant_sync_views VALUES(?1,?2,?3,?4)",
                    params![
                        id.as_slice(),
                        owner.as_slice(),
                        domain.0.as_slice(),
                        is_retired
                    ],
                )
                .unwrap();
                for ordinal in 0..3i64 {
                    let event = [ordinal as u8; 32];
                    c.execute(
                        "INSERT INTO participant_work.participant_sync_rows VALUES(?1,?2,?3,1,1,0,0,0)",
                        params![id.as_slice(), event.as_slice(), ordinal],
                    )
                    .unwrap();
                    for table in [
                        "participant_work.participant_sync_have",
                        "participant_work.participant_sync_receipts",
                        "participant_work.participant_sync_ancestors",
                    ] {
                        c.execute(
                            &format!("INSERT INTO {table} VALUES(?1,?2)"),
                            params![id.as_slice(), event.as_slice()],
                        )
                        .unwrap();
                    }
                    c.execute(
                        "INSERT INTO participant_work.participant_sync_departures VALUES(?1,0,'member',?2)",
                        params![id.as_slice(), event.as_slice()],
                    )
                    .unwrap();
                    c.execute(
                        "INSERT INTO participant_work.participant_sync_authors VALUES(?1,?2)",
                        params![id.as_slice(), event.as_slice()],
                    )
                    .unwrap();
                }
                c.execute(
                    "INSERT INTO participant_work.participant_sync_export_work VALUES(?1,-1,NULL,0,0)",
                    [id.as_slice()],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO participant_work.participant_sync_progress VALUES(?1,x'00')",
                    [id.as_slice()],
                )
                .unwrap();
            }
            // 18 child rows plus export, progress, and the parent: every
            // physical row, including final singleton cleanup, is charged.
            let mut remaining = 21u64;
            let mut turns = 0;
            while remaining > 0 {
                let before = c.total_changes();
                assert!(cleanup_on(&c, domain, owner, limit).unwrap());
                let removed = c.total_changes() - before;
                assert_eq!(removed, remaining.min(limit as u64));
                remaining -= removed;
                turns += 1;
                let exists: bool = c
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_views WHERE id=?1)",
                        [retired.as_slice()],
                        |r| r.get(0),
                    )
                    .unwrap();
                assert_eq!(exists, remaining != 0);
            }
            assert_eq!(turns, 21usize.div_ceil(limit));
            assert!(!cleanup_on(&c, domain, owner, limit).unwrap());
            let mut retained = 0i64;
            for (table, key) in [
                ("participant_work.participant_sync_rows", "view"),
                ("participant_work.participant_sync_have", "view"),
                ("participant_work.participant_sync_receipts", "view"),
                ("participant_work.participant_sync_ancestors", "view"),
                ("participant_work.participant_sync_departures", "view"),
                ("participant_work.participant_sync_authors", "view"),
                ("participant_work.participant_sync_export_work", "view"),
                ("participant_work.participant_sync_progress", "view"),
                ("participant_work.participant_sync_views", "id"),
            ] {
                retained += c
                    .query_row(
                        &format!("SELECT count(*) FROM {table} WHERE {key}=?1"),
                        [active.as_slice()],
                        |r| r.get::<_, i64>(0),
                    )
                    .unwrap();
            }
            assert_eq!(retained, 21, "an active view must never be retired");
        }
    }
}

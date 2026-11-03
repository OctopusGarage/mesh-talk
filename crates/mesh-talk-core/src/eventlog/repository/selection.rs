//! Disposable participant authorization selections in the encrypted profile DB.
//! Rows contain identities and scalar policy facts, never event bodies. The
//! exchange owner supplies its existing connection limit; one view occupies one
//! slot. Retired/crashed work is drained in bounded transactions before another
//! selection is admitted, so repeated cancellation cannot accumulate history.
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

pub(crate) struct Selection {
    id: [u8; 16],
    domain: EventDomainId,
    owner: [u8; 16],
    database: RepositoryOwner,
}
impl Drop for Selection {
    fn drop(&mut self) {
        let result = self.database.transaction(|tx| {
            tx.execute(
                "UPDATE participant_sync_views SET retired=1 WHERE id=?1 AND owner=?2",
                params![self.id.as_slice(), self.owner.as_slice()],
            )?;
            cleanup_on(tx, self.domain, self.owner, STEP).map(|_| ())
        });
        if let Err(error) = result {
            log::warn!("participant sync selection cleanup deferred: {error}");
        }
    }
}
fn cleanup_on(
    c: &Connection,
    domain: EventDomainId,
    owner: [u8; 16],
    limit: usize,
) -> Result<bool, LogError> {
    crate::eventlog::sync::network::scan_limit(limit)?;
    let id: Option<Vec<u8>> = c.query_row("SELECT id FROM participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2) LIMIT 1", params![domain.0.as_slice(),owner.as_slice()], |r|r.get(0)).optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    // A small retired view may occupy several child tables. They share one
    // allowance; none gets a fresh limit, and even the three singleton rows
    // count toward it. The fixed table list bounds empty-table probes.
    let mut remaining = limit;
    for (table, key) in [
        ("participant_sync_rows", "id"),
        ("participant_sync_have", "id"),
        ("participant_sync_receipts", "id"),
        ("participant_sync_ancestors", "id"),
        ("participant_sync_departures", "pass,member,event"),
        ("participant_sync_authors", "author"),
    ] {
        let removed = c.execute(
            &format!("DELETE FROM {table} WHERE view=?1 AND ({key}) IN (SELECT {key} FROM {table} WHERE view=?1 LIMIT ?2)"),
            params![&id, remaining as i64],
        )?;
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
        remaining -= c.execute(&format!("DELETE FROM {table} WHERE {key}=?1"), [&id])?;
        if remaining == 0 {
            return Ok(true);
        }
    }
    Ok(true)
}
impl EventRepository {
    #[cfg(test)]
    pub(crate) fn selection_counts(&self) -> Result<(usize, usize, usize), LogError> {
        self.database.read(|c| {
            Ok((
                c.query_row("SELECT count(*) FROM participant_sync_views", [], |r| {
                    r.get::<_, i64>(0).map(|n| n as usize)
                })?,
                c.query_row("SELECT count(*) FROM participant_sync_rows", [], |r| {
                    r.get::<_, i64>(0).map(|n| n as usize)
                })?,
                c.query_row("SELECT count(*) FROM participant_sync_have", [], |r| {
                    r.get::<_, i64>(0).map(|n| n as usize)
                })?,
            ))
        })
    }
    pub(crate) fn initialize_selections(&self) -> Result<(), LogError> {
        self.database.transaction(|tx| {
            tx.execute_batch(DDL)?;
            tx.execute_batch(metadata::DDL)?;
            tx.execute_batch(export::DDL)?;
            tx.execute(
                "INSERT OR IGNORE INTO participant_kind_scan VALUES(?1,-1)",
                [self.domain.0.as_slice()],
            )?;
            cleanup_on(tx, self.domain, self.selection_owner, STEP).map(|_| ())
        })
    }
    pub(crate) fn cleanup_selections(&self) -> Result<bool, LogError> {
        self.database
            .transaction(|tx| cleanup_on(tx, self.domain, self.selection_owner, STEP))
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
                "SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2)",
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
        let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_sync_views WHERE id=?1 AND owner=?2 AND domain=?3 AND retired=0)",params![selection.id.as_slice(),self.selection_owner.as_slice(),self.domain.0.as_slice()],|r|r.get(0))?;
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
            let mut stmt=c.prepare_cached("SELECT id,rank,outgoing FROM participant_sync_rows WHERE view=?1 AND rank>?2 AND rank<?3 ORDER BY rank LIMIT ?4")?;
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
            c.query_row("SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2 AND rank<?3 AND outgoing=1)",params![selection.id.as_slice(),id.as_bytes().as_slice(),upper],|r|r.get(0)).map_err(Into::into)
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
            let allowed:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2 AND rank<?3 AND outgoing=1)",params![selection.id.as_slice(),id.as_bytes().as_slice(),upper],|r|r.get(0))?;
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
            let mut stmt = c.prepare_cached("SELECT id,rank,outgoing FROM participant_sync_rows WHERE view=?1 AND rank>?2 AND rank<?3 ORDER BY rank LIMIT ?4")?;
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
    fn participant_selection_retirement_shares_one_limit_across_all_tables() {
        for limit in [1usize, 2, 5, 64] {
            let c = Connection::open_in_memory().unwrap();
            c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            c.execute_batch(DDL).unwrap();
            c.execute_batch(export::DDL).unwrap();
            let domain = EventDomainId([3; 16]);
            let owner = [4; 16];
            let retired = [1u8; 16];
            let active = [2u8; 16];
            for (id, is_retired) in [(retired, true), (active, false)] {
                c.execute(
                    "INSERT INTO participant_sync_views VALUES(?1,?2,?3,?4)",
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
                        "INSERT INTO participant_sync_rows VALUES(?1,?2,?3,1,1,0,0,0)",
                        params![id.as_slice(), event.as_slice(), ordinal],
                    )
                    .unwrap();
                    for table in [
                        "participant_sync_have",
                        "participant_sync_receipts",
                        "participant_sync_ancestors",
                    ] {
                        c.execute(
                            &format!("INSERT INTO {table} VALUES(?1,?2)"),
                            params![id.as_slice(), event.as_slice()],
                        )
                        .unwrap();
                    }
                    c.execute(
                        "INSERT INTO participant_sync_departures VALUES(?1,0,'member',?2)",
                        params![id.as_slice(), event.as_slice()],
                    )
                    .unwrap();
                    c.execute(
                        "INSERT INTO participant_sync_authors VALUES(?1,?2)",
                        params![id.as_slice(), event.as_slice()],
                    )
                    .unwrap();
                }
                c.execute(
                    "INSERT INTO participant_sync_export_work VALUES(?1,-1,NULL,0,0)",
                    [id.as_slice()],
                )
                .unwrap();
                c.execute(
                    "INSERT INTO participant_sync_progress VALUES(?1,x'00')",
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
                        "SELECT EXISTS(SELECT 1 FROM participant_sync_views WHERE id=?1)",
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
                ("participant_sync_rows", "view"),
                ("participant_sync_have", "view"),
                ("participant_sync_receipts", "view"),
                ("participant_sync_ancestors", "view"),
                ("participant_sync_departures", "view"),
                ("participant_sync_authors", "view"),
                ("participant_sync_export_work", "view"),
                ("participant_sync_progress", "view"),
                ("participant_sync_views", "id"),
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

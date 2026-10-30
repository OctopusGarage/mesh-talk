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
    let id: Option<Vec<u8>> = c.query_row("SELECT id FROM participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2) LIMIT 1", params![domain.0.as_slice(),owner.as_slice()], |r|r.get(0)).optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    for (table, key) in [
        ("participant_sync_rows", "id"),
        ("participant_sync_have", "id"),
        ("participant_sync_receipts", "id"),
        ("participant_sync_ancestors", "id"),
        ("participant_sync_departures", "pass,member,event"),
        ("participant_sync_authors", "author"),
    ] {
        if c.execute(&format!("DELETE FROM {table} WHERE view=?1 AND ({key}) IN (SELECT {key} FROM {table} WHERE view=?1 LIMIT ?2)"),params![&id,limit as i64])? != 0 {return Ok(true);}
    }
    c.execute(
        "DELETE FROM participant_sync_export_work WHERE view=?1",
        [&id],
    )?;
    c.execute("DELETE FROM participant_sync_progress WHERE view=?1", [&id])?;
    c.execute("DELETE FROM participant_sync_views WHERE id=?1", [&id])?;
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
}

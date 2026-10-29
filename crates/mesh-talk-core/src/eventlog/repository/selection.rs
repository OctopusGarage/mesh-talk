//! Disposable participant authorization selections in the encrypted profile DB.
//! Rows contain identities and scalar policy facts, never event bodies. The
//! exchange owner supplies its existing connection limit; one view occupies one
//! slot. Retired/crashed work is drained in bounded transactions before another
//! selection is admitted, so repeated cancellation cannot accumulate history.
use super::network::{ExportCursor, HaveCursor, NetworkView};
use super::*;
use crate::eventlog::sync::network::{ExportPage, ExportPageBudget, IdPageBudget, SyncPage};
use std::collections::HashSet;

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
            cleanup_on(tx, self.domain, self.owner).map(|_| ())
        });
        if let Err(error) = result {
            log::warn!("participant sync selection cleanup deferred: {error}");
        }
    }
}
fn cleanup_on(c: &Connection, domain: EventDomainId, owner: [u8; 16]) -> Result<bool, LogError> {
    let id: Option<Vec<u8>> = c.query_row("SELECT id FROM participant_sync_views WHERE domain=?1 AND (retired=1 OR owner<>?2) LIMIT 1", params![domain.0.as_slice(),owner.as_slice()], |r|r.get(0)).optional()?;
    let Some(id) = id else {
        return Ok(false);
    };
    c.execute("DELETE FROM participant_sync_rows WHERE view=?1 AND id IN (SELECT id FROM participant_sync_rows WHERE view=?1 LIMIT ?2)", params![&id, STEP as i64])?;
    c.execute("DELETE FROM participant_sync_have WHERE view=?1 AND id IN (SELECT id FROM participant_sync_have WHERE view=?1 LIMIT ?2)", params![&id, STEP as i64])?;
    c.execute("DELETE FROM participant_sync_views WHERE id=?1 AND NOT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1) AND NOT EXISTS(SELECT 1 FROM participant_sync_have WHERE view=?1)", [&id])?;
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
            cleanup_on(tx, self.domain, self.selection_owner).map(|_| ())
        })
    }
    pub(crate) fn cleanup_selections(&self) -> Result<bool, LogError> {
        self.database
            .transaction(|tx| cleanup_on(tx, self.domain, self.selection_owner))
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
    /// The caller's temporary reducer output is in exact causal order. Capture
    /// fixed policy facts now; all later Have-dependent closure uses these rows.
    pub(crate) fn create_selection(
        &self,
        view: &NetworkView,
        events: &[Event],
        max_views: usize,
        mut flags: impl FnMut(&Event) -> (bool, bool, bool),
    ) -> Result<Selection, LogError> {
        while self.cleanup_selections()? {}
        let selection = Selection {
            id: *uuid::Uuid::new_v4().as_bytes(),
            domain: self.domain,
            owner: self.selection_owner,
            database: self.database.clone(),
        };
        self.database.transaction(|tx| {
            self.validate_network_on(tx,view)?;
            let active: i64 = tx.query_row("SELECT count(*) FROM participant_sync_views WHERE domain=?1 AND owner=?2 AND retired=0",params![self.domain.0.as_slice(),self.selection_owner.as_slice()],|r|r.get(0))?;
            if active >= max_views as i64 { return Err(LogError::AdmissionRejected("participant sync connection capacity exhausted".into())); }
            tx.execute("INSERT INTO participant_sync_views(id,owner,domain) VALUES(?1,?2,?3)",params![selection.id.as_slice(),self.selection_owner.as_slice(),self.domain.0.as_slice()])?;
            Ok(())
        })?;
        for (chunk, events) in events.chunks(STEP).enumerate() {
            self.database.transaction(|tx| {
                self.validate_network_on(tx,view)?;
                let mut insert = tx.prepare("INSERT INTO participant_sync_rows(view,id,rank,candidate,selected,own) SELECT ?1,id,?2,?3,?4,?5 FROM retained_events WHERE domain=?6 AND id=?7 AND commit_ordinal<?8")?;
                for (offset,event) in events.iter().enumerate() {
                    let (candidate, selected, own) = flags(event);
                    insert.execute(params![selection.id.as_slice(),(chunk*STEP+offset) as i64,candidate,selected,own,self.domain.0.as_slice(),event.id.as_bytes().as_slice(),view.binding.cutoff])?;
                }
                Ok(())
            })?;
        }
        Ok(selection)
    }
    /// On a source/proof change only, compare the current reducer's original
    /// cutoff membership. No resident retained-ID set is required.
    pub(crate) fn finite_ids(
        &self,
        view: &NetworkView,
        events: &[Event],
    ) -> Result<Vec<EventId>, LogError> {
        self.database.read(|c| {
            self.validate_network_on(c,view)?;
            let mut stmt=c.prepare("SELECT EXISTS(SELECT 1 FROM retained_events WHERE domain=?1 AND id=?2 AND commit_ordinal<?3)")?;
            let mut ids=Vec::new();
            for e in events {
                if stmt.query_row(params![self.domain.0.as_slice(),e.id.as_bytes().as_slice(),view.binding.cutoff],|r|r.get::<_,bool>(0))? { ids.push(e.id); }
            }
            Ok(ids)
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
    /// Rebuild only when the peer's exact Have set changes. Parents are checked
    /// against retained edges and already visited causal rows. Two separate
    /// flags preserve sibling selection followed by share-off own-only closure.
    pub(crate) fn prepare_selected_export(
        &self,
        selection: &Selection,
        view: &NetworkView,
        have: &HashSet<EventId>,
        sibling_closure: bool,
        own_closure: bool,
    ) -> Result<(), LogError> {
        loop {
            let changed=self.database.transaction(|tx| {
                self.require_selection_on(tx,selection)?;
                self.validate_network_on(tx,view)?;
                Ok(tx.execute("DELETE FROM participant_sync_have WHERE view=?1 AND id IN (SELECT id FROM participant_sync_have WHERE view=?1 LIMIT ?2)",params![selection.id.as_slice(),STEP as i64])?)
            })?;
            if changed == 0 {
                break;
            }
        }
        // Have is already admitted by the exchange's shared RemoteHaveBudget.
        let mut batch = Vec::with_capacity(STEP);
        for id in have {
            batch.push(*id);
            if batch.len() == STEP {
                self.insert_selected_have(selection, &batch)?;
                batch.clear();
            }
        }
        self.insert_selected_have(selection, &batch)?;
        let mut after = -1_i64;
        loop {
            let next=self.database.transaction(|tx| {
                self.require_selection_on(tx,selection)?;
                self.validate_network_on(tx,view)?;
                let mut stmt=tx.prepare("SELECT id,rank,candidate,selected,own FROM participant_sync_rows WHERE view=?1 AND rank>?2 ORDER BY rank LIMIT ?3")?;
                let rows=stmt.query_map(params![selection.id.as_slice(),after,STEP as i64],|r| Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,i64>(1)?,r.get::<_,bool>(2)?,r.get::<_,bool>(3)?,r.get::<_,bool>(4)?)))?.collect::<Result<Vec<_>,_>>()?;
                let mut last=None;
                for (id,rank,candidate,selected,own) in rows {
                    let closed=|column:&str|->Result<bool,LogError>{
                        Ok(!tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM retained_event_parents p WHERE p.domain=?1 AND p.child=?2 AND NOT EXISTS(SELECT 1 FROM participant_sync_have h WHERE h.view=?3 AND h.id=p.parent) AND NOT EXISTS(SELECT 1 FROM participant_sync_rows r WHERE r.view=?3 AND r.id=p.parent AND r.rank<?4 AND r.{column}=1))"),params![self.domain.0.as_slice(),&id,selection.id.as_slice(),rank],|r|r.get::<_,bool>(0))?)
                    };
                    let sibling=candidate && (!sibling_closure || (selected && closed("sibling")?));
                    let outgoing=sibling && (!own_closure || (own && closed("outgoing")?));
                    tx.execute("UPDATE participant_sync_rows SET sibling=?3,outgoing=?4 WHERE view=?1 AND id=?2",params![selection.id.as_slice(),&id,sibling,outgoing])?;
                    last=Some(rank);
                }
                Ok(last)
            })?;
            match next {
                Some(rank) => after = rank,
                None => break,
            }
        }
        Ok(())
    }
    fn insert_selected_have(&self, selection: &Selection, ids: &[EventId]) -> Result<(), LogError> {
        self.database.transaction(|tx| {
            self.require_selection_on(tx, selection)?;
            let mut stmt =
                tx.prepare("INSERT INTO participant_sync_have(view,id) VALUES(?1,?2)")?;
            for id in ids {
                stmt.execute(params![selection.id.as_slice(), id.as_bytes().as_slice()])?;
            }
            Ok(())
        })
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

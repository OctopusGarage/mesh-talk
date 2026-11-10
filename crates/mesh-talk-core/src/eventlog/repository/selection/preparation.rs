//! Resumable participant selection writes. Every call has a finite row/edge
//! budget; current policy state belongs to the caller, selected identities here.
use super::*;

pub(crate) struct SelectionRow {
    pub header: EventHeader,
    pub rank: i64,
    pub candidate: bool,
    pub selected: bool,
    pub own: bool,
    pub receipt: bool,
}
#[derive(Default)]
pub(crate) struct ParentStep {
    pub after: Option<EventId>,
    pub examined: usize,
    pub closed: bool,
    pub complete: bool,
}
impl EventRepository {
    /// `None` means one retirement page was drained. The asynchronous session
    /// yields before retrying; cancelled history cannot grow beside a new view.
    pub(crate) fn begin_selection(
        &self,
        view: &NetworkView,
        max_views: usize,
        rows: usize,
    ) -> Result<Option<Selection>, LogError> {
        crate::eventlog::sync::network::scan_limit(rows)?;
        if self
            .database
            .transaction(|c| cleanup_on(c, self.domain, self.selection_owner, rows))?
        {
            return Ok(None);
        }
        let selection = Selection {
            id: *uuid::Uuid::new_v4().as_bytes(),
            domain: self.domain,
            owner: self.selection_owner,
            database: self.database.clone(),
        };
        self.database.transaction(|c| {
            self.validate_network_on(c, view)?;
            let active: i64 = c.query_row("SELECT count(*) FROM participant_sync_views WHERE domain=?1 AND owner=?2 AND retired=0",params![self.domain.0.as_slice(),self.selection_owner.as_slice()],|r|r.get(0))?;
            if active >= i64::try_from(max_views).map_err(|_| LogError::InvalidPageBudget)? {
                return Err(LogError::AdmissionRejected("participant sync connection capacity exhausted".into()));
            }
            c.execute("INSERT INTO participant_sync_views(id,owner,domain) VALUES(?1,?2,?3)",params![selection.id.as_slice(),self.selection_owner.as_slice(),self.domain.0.as_slice()])?;
            Ok(())
        })?;
        Ok(Some(selection))
    }
    pub(crate) fn insert_selection_row(
        &self,
        selection: &Selection,
        view: &NetworkView,
        row: SelectionRow,
    ) -> Result<bool, LogError> {
        self.database.transaction(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            let inserted=c.execute("INSERT OR IGNORE INTO participant_sync_rows(view,id,rank,candidate,selected,own) SELECT ?1,id,?2,?3,?4,?5 FROM retained_events WHERE domain=?6 AND conversation=?7 AND id=?8 AND commit_ordinal<?9",params![selection.id.as_slice(),row.rank,row.candidate,row.selected,row.own,self.domain.0.as_slice(),row.header.conversation.as_bytes().as_slice(),row.header.id.as_bytes().as_slice(),view.binding.cutoff])?;
            if inserted != 0 && row.receipt {
                c.execute("INSERT INTO participant_sync_receipts VALUES(?1,?2)",params![selection.id.as_slice(),row.header.id.as_bytes().as_slice()])?;
            }
            // Retrying the same durable cursor may revisit a committed row.
            // Membership remains true while its caller replays the scalar count.
            let retained:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2)",params![selection.id.as_slice(),row.header.id.as_bytes().as_slice()],|r|r.get(0))?;
            Ok(retained)
        })
    }
    pub(crate) fn selected_contains(
        &self,
        selection: &Selection,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2)",
                params![selection.id.as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
    }
    /// Parent count is not an admission limit. A legal event with more parents
    /// resumes from this edge cursor, without allocating its complete parent Vec.
    pub(crate) fn selection_parents_step(
        &self,
        selection: &Selection,
        view: &NetworkView,
        event: EventId,
        after: Option<EventId>,
        receipts_only: bool,
        edges: usize,
    ) -> Result<ParentStep, LogError> {
        let limit = crate::eventlog::sync::network::scan_limit(edges)?;
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            let table=if receipts_only {"participant_sync_receipts"} else {"participant_sync_rows"};
            let mut stmt=c.prepare(&format!("SELECT p.parent,EXISTS(SELECT 1 FROM {table} a WHERE a.view=?3 AND a.id=p.parent) FROM retained_event_parents p WHERE p.domain=?1 AND p.child=?2 AND p.parent>COALESCE(?4,x'') ORDER BY p.parent LIMIT ?5"))?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),event.as_bytes().as_slice(),selection.id.as_slice(),after.map(|id|*id.as_bytes()),limit])?;
            let mut result=ParentStep { closed:true, ..Default::default() };
            while let Some(row)=rows.next()? {
                if result.examined == edges { return Ok(result); }
                result.after=Some(EventId::new(blob(row,0)?));
                result.examined+=1;
                if !row.get::<_,bool>(1)? {result.closed=false; result.complete=true;return Ok(result);}
            }
            result.complete=true;
            Ok(result)
        })
    }
    pub(crate) fn selected_ids_page(
        &self,
        selection: &Selection,
        after: Option<EventId>,
        rows: usize,
    ) -> Result<Vec<EventId>, LogError> {
        let limit = i64::try_from(rows)
            .ok()
            .filter(|n| *n > 0)
            .ok_or(LogError::InvalidPageBudget)?;
        self.database.read(|c| {
            self.require_selection_on(c,selection)?;
            let mut stmt=c.prepare("SELECT id FROM participant_sync_rows WHERE view=?1 AND id>COALESCE(?2,x'') ORDER BY id LIMIT ?3")?;
            let mut result=Vec::new();let mut rows=stmt.query(params![selection.id.as_slice(),after.map(|id|*id.as_bytes()),limit])?;
            while let Some(row)=rows.next()? {result.push(EventId::new(blob(row,0)?));}
            Ok(result)
        })
    }
}

#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct AncestryProgress {
    pub rank: i64,
    pub parent_after: Option<EventId>,
}
impl EventRepository {
    pub(crate) fn selection_rank_header(
        &self,
        view: &NetworkView,
        job: [u8; 16],
        rank: i64,
    ) -> Result<Option<EventHeader>, LogError> {
        self.database.read(|c| {
            self.validate_network_on(c,view)?;
            self.require_selection_order_on(c,view,job)?;
            let columns=HEADER_COLUMNS.split(',').map(|name|format!("e.{name}")).collect::<Vec<_>>().join(",");
            let mut stmt=c.prepare(&format!("SELECT {columns} FROM event_order_rows r JOIN retained_events e ON e.domain=?1 AND e.id=r.id WHERE r.job=?2 AND r.rank=?3"))?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),job.as_slice(),rank])?;
            rows.next()?.map(read_header).transpose()
        })
    }
    fn require_selection_order_on(
        &self,
        c: &Connection,
        view: &NetworkView,
        job: [u8; 16],
    ) -> Result<(), LogError> {
        let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_order_current o JOIN event_conversations e ON e.domain=o.domain AND e.conversation=o.conversation WHERE o.domain=?1 AND o.conversation=?2 AND o.order_kind=0 AND o.job=?3 AND o.source_revision=e.revision AND e.lifecycle=0)",params![self.domain.0.as_slice(),view.binding.conversation.as_bytes().as_slice(),job.as_slice()],|r|r.get(0))?;
        if !valid {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn clear_selection_marks_step(
        &self,
        selection: &Selection,
        rows: usize,
    ) -> Result<usize, LogError> {
        let rows = crate::eventlog::sync::network::scan_limit(rows)? - 1;
        self.database.transaction(|c|{self.require_selection_on(c,selection)?;Ok(c.execute("DELETE FROM participant_sync_ancestors WHERE view=?1 AND id IN (SELECT id FROM participant_sync_ancestors WHERE view=?1 LIMIT ?2)",params![selection.id.as_slice(),rows])?)})
    }
    pub(crate) fn mark_selection_ancestors(
        &self,
        selection: &Selection,
        ids: &[EventId],
    ) -> Result<(), LogError> {
        self.database.transaction(|c| {
            self.require_selection_on(c, selection)?;
            let mut stmt =
                c.prepare("INSERT OR IGNORE INTO participant_sync_ancestors VALUES(?1,?2)")?;
            for id in ids {
                stmt.execute(params![selection.id.as_slice(), id.as_bytes().as_slice()])?;
            }
            Ok(())
        })
    }
    pub(crate) fn selection_is_ancestor(
        &self,
        selection: &Selection,
        id: EventId,
    ) -> Result<bool, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM participant_sync_ancestors WHERE view=?1 AND id=?2)",
                params![selection.id.as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
    }
    /// Reverse canonical order visits a marked child before every parent. The
    /// cursor includes the current parent's key so arbitrarily wide vertices
    /// cannot exceed a turn's edge budget.
    pub(crate) fn selection_ancestors_step(
        &self,
        selection: &Selection,
        view: &NetworkView,
        job: [u8; 16],
        progress: &mut AncestryProgress,
        budget: crate::eventlog::sync::network::PreparationBudget,
    ) -> Result<crate::eventlog::sync::network::PreparationStep, LogError> {
        budget.validate()?;
        budget.rows_for_bytes(64)?;
        let (next,step)=self.database.transaction(|c| {
            self.require_selection_on(c,selection)?;self.validate_network_on(c,view)?;self.require_selection_order_on(c,view,job)?;
            let mut next=*progress;let mut step=crate::eventlog::sync::network::PreparationStep::default();
            while next.rank>=0 && step.rows<budget.rows && step.edges<budget.edges && step.bytes+64<=budget.bytes {
                let (id,marked):(Vec<u8>,bool)=c.query_row("SELECT id,EXISTS(SELECT 1 FROM participant_sync_ancestors a WHERE a.view=?3 AND a.id=r.id) FROM event_order_rows r WHERE job=?1 AND rank=?2",params![job.as_slice(),next.rank,selection.id.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
                if marked {
                    let limit=(budget.edges-step.edges).min((budget.bytes-step.bytes)/64);
                    if limit==0 {break;}
                    let mut stmt=c.prepare("SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent>COALESCE(?3,x'') ORDER BY parent LIMIT ?4")?;
                    let parents=stmt.query_map(params![self.domain.0.as_slice(),id,next.parent_after.map(|id|*id.as_bytes()),limit as i64],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?;
                    for parent in &parents {c.execute("INSERT OR IGNORE INTO participant_sync_ancestors VALUES(?1,?2)",params![selection.id.as_slice(),parent])?;}
                    step.edges+=parents.len();step.bytes+=parents.len()*64;
                    if parents.len()==limit {next.parent_after=parents.last().map(|id|id.as_slice().try_into().map(EventId::new).map_err(|_|corrupt("invalid ancestor id"))).transpose()?;continue;}
                }
                next.rank-=1;next.parent_after=None;step.rows+=1;step.bytes+=32;
            }
            step.ready=next.rank<0;
            Ok((next,step))
        })?;
        *progress = next;
        Ok(step)
    }
    pub(crate) fn selection_departure(
        &self,
        selection: &Selection,
        pass: i64,
        member: &str,
        event: EventId,
    ) -> Result<(), LogError> {
        self.database.transaction(|c| {
            self.require_selection_on(c, selection)?;
            c.execute(
                "INSERT OR IGNORE INTO participant_sync_departures VALUES(?1,?2,?3,?4)",
                params![
                    selection.id.as_slice(),
                    pass,
                    member,
                    event.as_bytes().as_slice()
                ],
            )?;
            Ok(())
        })
    }
    pub(crate) fn selection_departure_page(
        &self,
        selection: &Selection,
        pass: i64,
        member: &str,
        after: Option<EventId>,
        limit: usize,
    ) -> Result<Vec<(EventId, bool)>, LogError> {
        let limit = crate::eventlog::sync::network::scan_limit(limit)? - 1;
        self.database.read(|c| {
            self.require_selection_on(c,selection)?;
            let mut stmt=c.prepare("SELECT event,EXISTS(SELECT 1 FROM participant_sync_ancestors a WHERE a.view=d.view AND a.id=d.event) FROM participant_sync_departures d WHERE view=?1 AND pass=?2 AND member=?3 AND event>COALESCE(?4,x'') ORDER BY event LIMIT ?5")?;
            let mut rows=stmt.query(params![selection.id.as_slice(),pass,member,after.map(|id|*id.as_bytes()),limit])?;let mut result=Vec::new();
            while let Some(row)=rows.next()?{result.push((EventId::new(blob(row,0)?),row.get(1)?));}Ok(result)
        })
    }
    pub(crate) fn selection_author(
        &self,
        selection: &Selection,
        author: Author,
        insert: bool,
    ) -> Result<bool, LogError> {
        self.database.transaction(|c| {
            self.require_selection_on(c, selection)?;
            if insert {
                c.execute(
                    "INSERT OR IGNORE INTO participant_sync_authors VALUES(?1,?2)",
                    params![selection.id.as_slice(), author.ed25519_pub().as_slice()],
                )?;
            }
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM participant_sync_authors WHERE view=?1 AND author=?2)",
                params![selection.id.as_slice(), author.ed25519_pub().as_slice()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
    }
    pub(crate) fn clear_selected_rows_step(
        &self,
        selection: &Selection,
        limit: usize,
    ) -> Result<usize, LogError> {
        let limit = crate::eventlog::sync::network::scan_limit(limit)? - 1;
        self.database.transaction(|c|{self.require_selection_on(c,selection)?;Ok(c.execute("DELETE FROM participant_sync_rows WHERE view=?1 AND id IN (SELECT id FROM participant_sync_rows WHERE view=?1 LIMIT ?2)",params![selection.id.as_slice(),limit])?)})
    }
}

impl EventRepository {
    pub(crate) fn save_selection_progress(
        &self,
        selection: &Selection,
        view: &NetworkView,
        context: &[u8],
    ) -> Result<(), LogError> {
        self.database.transaction(|c|{
            self.require_selection_on(c,selection)?;self.validate_network_on(c,view)?;
            c.execute("INSERT INTO participant_sync_progress VALUES(?1,?2) ON CONFLICT(view) DO UPDATE SET context=excluded.context",params![selection.id.as_slice(),context])?;
            Ok(())
        })
    }
    pub(crate) fn selection_progress(
        &self,
        selection: &Selection,
        view: &NetworkView,
    ) -> Result<Option<Vec<u8>>, LogError> {
        self.database.read(|c| {
            self.require_selection_on(c, selection)?;
            self.validate_network_on(c, view)?;
            c.query_row(
                "SELECT context FROM participant_sync_progress WHERE view=?1",
                [selection.id.as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
    }
}

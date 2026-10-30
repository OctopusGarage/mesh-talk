//! Exact ancestry over canonical ranks. SQLite temporary storage is in-memory,
//! so a recursive CTE would retain an unbounded frontier. These short-lived marks
//! live in the encrypted database and advance in fixed header-only steps.
use super::order::{CausalOrder, OrderReadiness, OrderWorkBudget};
use super::*;

const STEP: i64 = 128;
const SCHEMA: &str = "
CREATE TABLE event_ancestry_queries(
 query BLOB PRIMARY KEY CHECK(length(query)=16), retired INTEGER NOT NULL CHECK(retired IN (0,1))
) WITHOUT ROWID;
CREATE TABLE event_ancestry_marks(
 query BLOB NOT NULL REFERENCES event_ancestry_queries(query), id BLOB NOT NULL CHECK(length(id)=32),
 PRIMARY KEY(query,id)
) WITHOUT ROWID;
CREATE TABLE event_ancestry_selection(
 query BLOB PRIMARY KEY REFERENCES event_ancestry_queries(query),store_uuid BLOB NOT NULL,domain BLOB NOT NULL,
 conversation BLOB NOT NULL,job BLOB NOT NULL,revision INTEGER NOT NULL,child_rank INTEGER NOT NULL,
 next_rank INTEGER NOT NULL,parent_after BLOB
) WITHOUT ROWID;";

struct Marks {
    database: RepositoryOwner,
    query: [u8; 16],
    retire_on_drop: bool,
}
impl Drop for Marks {
    fn drop(&mut self) {
        if !self.retire_on_drop {
            return;
        }
        if let Err(error) = self.database.transaction(|c| {
            c.execute(
                "UPDATE event_ancestry_queries SET retired=1 WHERE query=?1",
                [self.query.as_slice()],
            )?;
            cleanup_on(c).map(|_| ())
        }) {
            log::warn!("channel ancestry cleanup deferred: {error}");
        }
    }
}
fn cleanup_on(c: &Connection) -> Result<bool, LogError> {
    let query: Option<Vec<u8>> = c
        .query_row(
            "SELECT query FROM event_ancestry_queries WHERE retired=1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(query) = query else {
        return Ok(false);
    };
    c.execute(
        "DELETE FROM event_ancestry_selection WHERE query=?1",
        [&query],
    )?;
    c.execute("DELETE FROM event_ancestry_marks WHERE query=?1 AND id IN (SELECT id FROM event_ancestry_marks WHERE query=?1 LIMIT 512)",[&query])?;
    c.execute("DELETE FROM event_ancestry_queries WHERE query=?1 AND NOT EXISTS(SELECT 1 FROM event_ancestry_marks WHERE query=?1)",[&query])?;
    Ok(true)
}
impl EventRepository {
    /// The Node invokes this once before exposing graph consumers under its
    /// profile lease. Crash leftovers become incremental cleanup work.
    pub(crate) fn initialize_ancestry(&self) -> Result<(), LogError> {
        self.database.transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='event_ancestry_queries')",
                [],
                |row| row.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query NOT IN (SELECT query FROM event_ancestry_selection)", [])?;
            cleanup_on(c)?;
            Ok(())
        })
    }

    pub(crate) fn descends_from(
        &self,
        conversation: ConversationId,
        child: EventId,
        ancestor: EventId,
    ) -> Result<bool, LogError> {
        while self
            .order_step(
                &conversation,
                CausalOrder::Logical,
                OrderWorkBudget::default(),
            )?
            .readiness
            != OrderReadiness::Ready
        {}
        let Some((job, ancestor_rank, child_rank))=self.database.read(|c| {
            self.require_active_on(c,&conversation)?;
            let row:Option<(Vec<u8>,i64,i64)>=c.query_row(
                "SELECT j.job,a.rank,b.rank FROM event_order_current j JOIN event_order_rows a ON a.job=j.job AND a.id=?3 JOIN event_order_rows b ON b.job=j.job AND b.id=?4 WHERE j.domain=?1 AND j.conversation=?2 AND j.order_kind=0",
                params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),ancestor.as_bytes().as_slice(),child.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            Ok(row)
        })? else { return Ok(false); };
        if ancestor_rank > child_rank {
            return Ok(false);
        }
        if ancestor_rank == child_rank {
            return Ok(true);
        }
        // Cancelled walks cannot accumulate a second history before retirement.
        while self.database.transaction(|c| cleanup_on(c))? {}
        let marks = Marks {
            database: self.database.clone(),
            query: *uuid::Uuid::new_v4().as_bytes(),
            retire_on_drop: true,
        };
        self.database.transaction(|c| {
            c.execute(
                "INSERT INTO event_ancestry_queries VALUES(?1,0)",
                [marks.query.as_slice()],
            )?;
            c.execute(
                "INSERT INTO event_ancestry_marks VALUES(?1,?2)",
                params![marks.query.as_slice(), ancestor.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        let mut after = ancestor_rank;
        let mut parent_after: Option<Vec<u8>> = None;
        while after < child_rank {
            (after,parent_after)=self.database.transaction(|c| {
                self.require_active_on(c,&conversation)?;
                let current:Option<Vec<u8>>=c.query_row("SELECT job FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=0",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()],|r|r.get(0)).optional()?;
                if current.as_ref()!=Some(&job){return Err(LogError::StaleCursor);}
                let mut next=after;let mut edge_after=parent_after.clone();let mut vertices=0;let mut edges=0;
                while next<child_rank && vertices<STEP && edges<512 {
                    let id:Vec<u8>=c.query_row("SELECT id FROM event_order_rows WHERE job=?1 AND rank=?2",params![&job,next+1],|r|r.get(0))?;
                    let mut statement=c.prepare("SELECT p.parent,EXISTS(SELECT 1 FROM event_ancestry_marks m WHERE m.query=?1 AND m.id=p.parent) FROM retained_event_parents p WHERE p.domain=?2 AND p.child=?3 AND p.parent>COALESCE(?4,x'') ORDER BY p.parent LIMIT ?5")?;
                    let parents:Vec<(Vec<u8>,bool)>=statement.query_map(params![marks.query.as_slice(),self.domain.0.as_slice(),&id,edge_after,512-edges],|r|Ok((r.get(0)?,r.get(1)?)))?.collect::<Result<_,_>>()?;
                    let examined=parents.len() as i64;
                    let reachable=parents.iter().any(|(_,marked)|*marked);
                    if reachable {
                        c.execute("INSERT OR IGNORE INTO event_ancestry_marks VALUES(?1,?2)",params![marks.query.as_slice(),&id])?;
                    }
                    let exhausted=examined<512-edges;
                    edges+=examined;
                    if reachable||exhausted {next+=1;vertices+=1;edge_after=None;}
                    else {edge_after=parents.last().map(|(id,_)|id.clone());}
                }
                Ok((next,edge_after))
            })?;
        }
        self.database.read(|c| {
            self.require_active_on(c,&conversation)?;
            let current:Option<Vec<u8>>=c.query_row("SELECT job FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=0",params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            if current.as_ref()!=Some(&job){return Err(LogError::StaleCursor);}
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM event_ancestry_marks WHERE query=?1 AND id=?2)",
                params![marks.query.as_slice(), child.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
    }
}

/// A bounded disk selection of one signed event's strict ancestors. The
/// canonical prefix also contains concurrent events and cannot substitute for it.
pub(crate) struct AncestorSelection {
    marks: Marks,
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: ConversationId,
    job: [u8; 16],
    revision: i64,
    child_rank: i64,
}
pub(crate) struct AncestorHeaderPage {
    pub(crate) items: Vec<(i64, EventHeader)>,
    /// Metadata work can produce an empty page while advancing over concurrent
    /// vertices. None means the strict ancestor prefix is fully examined.
    pub(crate) next: Option<i64>,
}
impl AncestorSelection {
    pub(crate) fn generation(&self) -> ([u8; 16], i64) {
        (self.job, self.revision)
    }
}
impl EventRepository {
    fn require_ancestor_selection_on(
        &self,
        c: &Connection,
        selection: &AncestorSelection,
    ) -> Result<(), LogError> {
        if selection.store_uuid != self.store_uuid || selection.domain != self.domain {
            return Err(LogError::StaleCursor);
        }
        self.require_active_on(c, &selection.conversation)?;
        let current: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM event_order_current o JOIN event_conversations s ON s.domain=o.domain AND s.conversation=o.conversation AND s.revision=o.source_revision WHERE o.domain=?1 AND o.conversation=?2 AND o.order_kind=0 AND o.job=?3 AND o.source_revision=?4 AND s.lifecycle=0)",
            params![self.domain.0.as_slice(),selection.conversation.as_bytes().as_slice(),selection.job.as_slice(),selection.revision],|r|r.get(0))?;
        if !current {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }

    pub(crate) fn select_ancestors(
        &self,
        conversation: ConversationId,
        child: EventId,
    ) -> Result<Option<AncestorSelection>, LogError> {
        while self
            .order_step(
                &conversation,
                CausalOrder::Logical,
                OrderWorkBudget::default(),
            )?
            .readiness
            != OrderReadiness::Ready
        {}
        let Some(generation) = self.order_generation(&conversation, CausalOrder::Logical)? else {
            return Ok(None);
        };
        let child_rank: Option<i64> = self.database.read(|c| {
            c.query_row(
                "SELECT rank FROM event_order_rows WHERE job=?1 AND id=?2",
                params![generation.job.as_slice(), child.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })?;
        let Some(child_rank) = child_rank else {
            return Ok(None);
        };
        while self.database.transaction(|c| cleanup_on(c))? {}
        let selection = AncestorSelection {
            marks: Marks {
                database: self.database.clone(),
                query: *uuid::Uuid::new_v4().as_bytes(),
                retire_on_drop: true,
            },
            store_uuid: self.store_uuid,
            domain: self.domain,
            conversation,
            job: generation.job,
            revision: generation.source_revision,
            child_rank,
        };
        self.database.transaction(|c| {
            self.require_ancestor_selection_on(c, &selection)?;
            c.execute(
                "INSERT INTO event_ancestry_queries VALUES(?1,0)",
                [selection.marks.query.as_slice()],
            )?;
            c.execute(
                "INSERT INTO event_ancestry_marks VALUES(?1,?2)",
                params![
                    selection.marks.query.as_slice(),
                    child.as_bytes().as_slice()
                ],
            )?;
            Ok(())
        })?;
        let mut rank = child_rank;
        let mut after_parent: Option<Vec<u8>> = None;
        while rank >= 0 {
            (rank, after_parent) = self.database.transaction(|c| {
                self.require_ancestor_selection_on(c, &selection)?;
                let mut next = rank;
                let mut parent_after = after_parent.clone();
                let mut vertices = 0;
                let mut edges = 0;
                while next >= 0 && vertices < STEP && edges < 512 {
                    let (id, marked):(Vec<u8>,bool) = c.query_row(
                        "SELECT r.id,EXISTS(SELECT 1 FROM event_ancestry_marks m WHERE m.query=?1 AND m.id=r.id) FROM event_order_rows r WHERE r.job=?2 AND r.rank=?3",
                        params![selection.marks.query.as_slice(),selection.job.as_slice(),next],|r|Ok((r.get(0)?,r.get(1)?)))?;
                    if !marked { next -= 1; vertices += 1; continue; }
                    let remaining = 512 - edges;
                    let mut statement = c.prepare("SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent>COALESCE(?3,x'') ORDER BY parent LIMIT ?4")?;
                    let parents: Vec<Vec<u8>> = statement.query_map(params![self.domain.0.as_slice(),id,parent_after,remaining],|r|r.get(0))?.collect::<Result<_,_>>()?;
                    for parent in &parents {
                        c.execute("INSERT OR IGNORE INTO event_ancestry_marks VALUES(?1,?2)",params![selection.marks.query.as_slice(),parent])?;
                    }
                    let count = parents.len() as i64;
                    edges += count;
                    if count < remaining { next -= 1; vertices += 1; parent_after = None; }
                    else { parent_after = parents.last().cloned(); }
                }
                Ok((next,parent_after))
            })?;
        }
        Ok(Some(selection))
    }

    pub(crate) fn ancestor_headers_page(
        &self,
        selection: &AncestorSelection,
        after_rank: i64,
        rows: usize,
    ) -> Result<AncestorHeaderPage, LogError> {
        if rows == 0 || rows > 128 || after_rank < -1 {
            return Err(LogError::InvalidPageBudget);
        }
        self.database.read(|c| {
            self.require_ancestor_selection_on(c, selection)?;
            let pending:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM event_ancestry_selection WHERE query=?1 AND next_rank>=0)",[selection.marks.query.as_slice()],|r|r.get(0))?;
            if pending {return Err(LogError::StaleCursor);}
            let columns = HEADER_COLUMNS.split(',').map(|column|format!("e.{column}")).collect::<Vec<_>>().join(",");
            let through = after_rank.saturating_add(integer(rows)?).min(selection.child_rank - 1);
            let mut statement = c.prepare(&format!("SELECT {columns},r.rank FROM event_order_rows r CROSS JOIN event_ancestry_marks m ON m.query=?1 AND m.id=r.id CROSS JOIN retained_events e ON e.domain=?2 AND e.id=r.id WHERE r.job=?3 AND r.rank>?4 AND r.rank<=?5 ORDER BY r.rank"))?;
            let mut values = statement.query(params![selection.marks.query.as_slice(),self.domain.0.as_slice(),selection.job.as_slice(),after_rank,through])?;
            let mut page = Vec::new();
            while let Some(row) = values.next()? { page.push((nonnegative(row.get(12)?)?,read_header(row)?)); }
            Ok(AncestorHeaderPage { items: page, next: (through < selection.child_rank - 1).then_some(through) })
        })
    }
}

impl AncestorSelection {
    pub(crate) fn id(&self) -> [u8; 16] {
        self.marks.query
    }
    pub(crate) fn retire_with_owner(&mut self) {
        self.marks.retire_on_drop = true;
    }
}
impl EventRepository {
    /// Begin a persisted single-root reverse-rank selection. The caller already
    /// owns a ready canonical generation; this never drains order or cleanup.
    pub(crate) fn start_ancestor_selection(
        &self,
        conversation: ConversationId,
        child: EventId,
    ) -> Result<Option<AncestorSelection>, LogError> {
        self.start_ancestor_selection_roots(conversation, &[child], false)
    }
    pub(crate) fn start_ancestor_selection_roots(
        &self,
        conversation: ConversationId,
        roots: &[EventId],
        include_roots: bool,
    ) -> Result<Option<AncestorSelection>, LogError> {
        if roots.is_empty() || roots.len() > 256 || (!include_roots && roots.len() != 1) {
            return Err(LogError::InvalidPageBudget);
        }
        let Some(generation) = self.order_generation(&conversation, CausalOrder::Logical)? else {
            return Ok(None);
        };
        let maximum: Option<i64> = self.database.read(|c| {
            let mut maximum = -1;
            for root in roots {
                let rank: Option<i64> = c
                    .query_row(
                        "SELECT rank FROM event_order_rows WHERE job=?1 AND id=?2",
                        params![generation.job.as_slice(), root.as_bytes().as_slice()],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(rank) = rank else {
                    return Ok(None);
                };
                maximum = maximum.max(rank);
            }
            Ok(Some(maximum))
        })?;
        let Some(maximum) = maximum else {
            return Ok(None);
        };
        let child_rank = maximum
            .checked_add(i64::from(include_roots))
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let selection = AncestorSelection {
            marks: Marks {
                database: self.database.clone(),
                query: *uuid::Uuid::new_v4().as_bytes(),
                retire_on_drop: false,
            },
            store_uuid: self.store_uuid,
            domain: self.domain,
            conversation,
            job: generation.job,
            revision: generation.source_revision,
            child_rank,
        };
        self.database.transaction(|c| {
            self.require_ancestor_selection_on(c, &selection)?;
            cleanup_on(c)?;
            c.execute(
                "INSERT INTO event_ancestry_queries VALUES(?1,0)",
                [selection.marks.query.as_slice()],
            )?;
            c.execute(
                "INSERT INTO event_ancestry_selection VALUES(?1,?2,?3,?4,?5,?6,?7,?8,NULL)",
                params![
                    selection.marks.query.as_slice(),
                    self.store_uuid.as_slice(),
                    self.domain.0.as_slice(),
                    conversation.as_bytes().as_slice(),
                    selection.job.as_slice(),
                    selection.revision,
                    child_rank,
                    maximum
                ],
            )?;
            for root in roots {
                c.execute(
                    "INSERT OR IGNORE INTO event_ancestry_marks VALUES(?1,?2)",
                    params![selection.marks.query.as_slice(), root.as_bytes().as_slice()],
                )?;
            }
            Ok(())
        })?;
        Ok(Some(selection))
    }
    pub(crate) fn resume_ancestor_selection(
        &self,
        id: [u8; 16],
    ) -> Result<Option<AncestorSelection>, LogError> {
        self.database.read(|c| {
            let row:Option<(Vec<u8>,Vec<u8>,Vec<u8>,Vec<u8>,i64,i64)>=c.query_row("SELECT s.store_uuid,s.domain,s.conversation,s.job,s.revision,s.child_rank FROM event_ancestry_selection s JOIN event_ancestry_queries q ON q.query=s.query AND q.retired=0 WHERE s.query=?1",[id.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
            let Some((uuid,domain,conversation,job,revision,child_rank))=row else{return Ok(None);};
            if uuid!=self.store_uuid || domain!=self.domain.0 {return Err(LogError::StaleCursor);}
            let selection=AncestorSelection {marks:Marks {database:self.database.clone(),query:id,retire_on_drop:false},store_uuid:self.store_uuid,domain:self.domain,conversation:ConversationId::new(conversation.try_into().map_err(|_|LogError::StaleCursor)?),job:job.try_into().map_err(|_|LogError::StaleCursor)?,revision,child_rank};
            self.require_ancestor_selection_on(c,&selection)?;
            Ok(Some(selection))
        })
    }
    /// At most128 ranked vertices and512 parent identities, including a partial
    /// large parent set. True authorizes paging the completed strict ancestors.
    pub(crate) fn ancestor_selection_step(
        &self,
        selection: &AncestorSelection,
    ) -> Result<bool, LogError> {
        self.database.transaction(|c| {
            self.require_ancestor_selection_on(c,selection)?;
            let (mut next,mut parent_after):(i64,Option<Vec<u8>>)=c.query_row("SELECT next_rank,parent_after FROM event_ancestry_selection WHERE query=?1",[selection.marks.query.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let mut vertices=0;let mut edges=0;
            while next>=0 && vertices<STEP && edges<512 {
                let (id,marked):(Vec<u8>,bool)=c.query_row("SELECT r.id,EXISTS(SELECT 1 FROM event_ancestry_marks m WHERE m.query=?1 AND m.id=r.id) FROM event_order_rows r WHERE r.job=?2 AND r.rank=?3",params![selection.marks.query.as_slice(),selection.job.as_slice(),next],|r|Ok((r.get(0)?,r.get(1)?)))?;
                if !marked {next-=1;vertices+=1;continue;}
                let remaining=512-edges;
                let mut statement=c.prepare("SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent>COALESCE(?3,x'') ORDER BY parent LIMIT ?4")?;
                let parents:Vec<Vec<u8>>=statement.query_map(params![self.domain.0.as_slice(),id,parent_after,remaining],|r|r.get(0))?.collect::<Result<_,_>>()?;
                for parent in &parents {c.execute("INSERT OR IGNORE INTO event_ancestry_marks VALUES(?1,?2)",params![selection.marks.query.as_slice(),parent])?;}
                let count=parents.len() as i64;edges+=count;
                if count<remaining {next-=1;vertices+=1;parent_after=None;}else{parent_after=parents.last().cloned();}
            }
            c.execute("UPDATE event_ancestry_selection SET next_rank=?2,parent_after=?3 WHERE query=?1",params![selection.marks.query.as_slice(),next,parent_after])?;
            Ok(next<0)
        })
    }
    pub(crate) fn retire_ancestor_selection(&self, id: [u8; 16]) -> Result<(), LogError> {
        self.database.transaction(|c| {
            c.execute(
                "UPDATE event_ancestry_queries SET retired=1 WHERE query=?1",
                [id.as_slice()],
            )?;
            c.execute(
                "DELETE FROM event_ancestry_selection WHERE query=?1",
                [id.as_slice()],
            )?;
            cleanup_on(c)?;
            Ok(())
        })
    }
}

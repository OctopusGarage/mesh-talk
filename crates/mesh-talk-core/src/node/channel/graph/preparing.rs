//! One signed reduction decision owns resumable graph answers and quantified
//! fact cursors. A missing answer suspends the private reducer; no partial
//! decision may reach a checkpoint transaction.
use super::{AuthorityGraph, PredicateProgress, require_source_on};
use crate::{
    eventlog::{
        ConversationId, Event, EventId, LogError,
        repository::{EventRepository, order::OrderWorkBudget},
        sync::network::{PreparationBudget, PreparationStep},
    },
    storage::profile::context::ProfileStorage,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::{borrow::Cow, cell::Cell, sync::Arc};

const SCHEMA: &str = "
CREATE TABLE channel_decision_work(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),event BLOB NOT NULL CHECK(length(event)=32),source_job BLOB NOT NULL CHECK(length(source_job)=16),revision INTEGER NOT NULL CHECK(revision>=0),selection BLOB CHECK(selection IS NULL OR length(selection)=16),child BLOB CHECK(child IS NULL OR length(child)=32),ancestor BLOB CHECK(ancestor IS NULL OR length(ancestor)=32),retired INTEGER NOT NULL CHECK(retired IN(0,1)),PRIMARY KEY(job,channel)) WITHOUT ROWID;
CREATE INDEX channel_decision_retired ON channel_decision_work(retired,job,channel);
CREATE TABLE channel_decision_answers(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),key BLOB NOT NULL,after_key BLOB,answer INTEGER CHECK(answer IS NULL OR answer IN(0,1)),PRIMARY KEY(job,channel,key)) WITHOUT ROWID;
";
pub(in crate::node) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_decision_work')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}

#[derive(Clone, Copy)]
pub(in crate::node) struct DecisionBinding {
    pub(in crate::node) job: [u8; 16],
    pub(in crate::node) source_job: [u8; 16],
    pub(in crate::node) revision: i64,
    pub(in crate::node) channel: ConversationId,
    pub(in crate::node) event: EventId,
}
#[derive(PartialEq)]
struct DecisionRow {
    event: Vec<u8>,
    source_job: Vec<u8>,
    revision: i64,
    selection: Option<Vec<u8>>,
    child: Option<Vec<u8>>,
    ancestor: Option<Vec<u8>>,
    retired: bool,
}
type PredicateAnswer = Option<(Option<Vec<u8>>, Option<bool>)>;
pub(in crate::node) struct PreparingGraph<'a> {
    profile: Arc<ProfileStorage>,
    repository: &'a EventRepository,
    job: [u8; 16],
    source_job: [u8; 16],
    revision: i64,
    channel: ConversationId,
    event: EventId,
    budget: PreparationBudget,
    work: Cell<PreparationStep>,
    pending: Cell<bool>,
}
impl<'a> PreparingGraph<'a> {
    pub(in crate::node) fn open(
        profile: Arc<ProfileStorage>,
        repository: &'a EventRepository,
        binding: DecisionBinding,
        budget: PreparationBudget,
    ) -> Result<Self, LogError> {
        let DecisionBinding {
            job,
            source_job,
            revision,
            channel,
            event,
        } = binding;
        budget.validate()?;
        if budget.bytes < 256 {
            return Err(LogError::PageBudgetTooSmall { required: 256 });
        }
        Ok(Self {
            profile,
            repository,
            job,
            source_job,
            revision,
            channel,
            event,
            budget,
            work: Cell::new(PreparationStep::default()),
            pending: Cell::new(false),
        })
    }
    fn source_guard(&self, c: &Connection) -> Result<(), LogError> {
        require_source_on(c, self.channel, self.source_job, self.revision)
    }
    fn guard(&self, c: &Connection) -> Result<(), LogError> {
        self.source_guard(c)?;
        let valid:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM channel_decision_work WHERE job=?1 AND channel=?2 AND event=?3 AND source_job=?4 AND revision=?5 AND retired=0)",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),self.event.as_bytes().as_slice(),self.source_job.as_slice(),self.revision],|r|r.get(0))?;
        if !valid {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    fn decision_on(&self, c: &Connection) -> Result<Option<DecisionRow>, LogError> {
        c.query_row("SELECT event,source_job,revision,selection,child,ancestor,retired FROM channel_decision_work WHERE job=?1 AND channel=?2",params![self.job.as_slice(),self.channel.as_bytes().as_slice()],|r|Ok(DecisionRow {event:r.get(0)?,source_job:r.get(1)?,revision:r.get(2)?,selection:r.get(3)?,child:r.get(4)?,ancestor:r.get(5)?,retired:r.get(6)?})).optional().map_err(Into::into)
    }
    pub(in crate::node) fn work(&self) -> PreparationStep {
        self.work.get()
    }
    /// Resume graph work before decoding the decision again. Old answers and
    /// ancestry marks retire in bounded transactions before a new decision.
    pub(in crate::node) fn prepare(&self) -> Result<bool, LogError> {
        let old = self.profile.database().read(|c| {
            self.source_guard(c)?;
            self.decision_on(c)
        })?;
        if let Some(old) = old {
            if old.event != self.event.as_bytes()
                || old.source_job != self.source_job
                || old.revision != self.revision
            {
                let limit = self.budget.rows.min(self.budget.bytes / 512);
                if limit == 0 {
                    return Err(LogError::PageBudgetTooSmall { required: 512 });
                }
                let (removed,replaced)=self.profile.database().transaction(|c| {
                    self.source_guard(c)?;
                    if self.decision_on(c)?.as_ref()!=Some(&old) {return Err(LogError::StaleCursor);}
                    if let Some(selection)=&old.selection {
                        c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query=?1",[selection])?;
                    }
                    let n=c.execute("DELETE FROM channel_decision_answers WHERE job=?1 AND channel=?2 AND key IN(SELECT key FROM channel_decision_answers WHERE job=?1 AND channel=?2 LIMIT ?3)",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),i64::try_from(limit).map_err(|_|LogError::InvalidPageBudget)?])?;
                    let replaced=n<limit;
                    if replaced {
                        c.execute("DELETE FROM channel_decision_work WHERE job=?1 AND channel=?2",params![self.job.as_slice(),self.channel.as_bytes().as_slice()])?;
                        c.execute("INSERT INTO channel_decision_work VALUES(?1,?2,?3,?4,?5,NULL,NULL,NULL,0)",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),self.event.as_bytes().as_slice(),self.source_job.as_slice(),self.revision])?;
                    }
                    Ok((n+usize::from(replaced),replaced))
                })?;
                self.work.set(PreparationStep {
                    rows: removed,
                    bytes: removed * 512,
                    ..Default::default()
                });
                return Ok(replaced);
            }
            if old.retired {
                return Err(LogError::StaleCursor);
            }
            let DecisionRow {
                selection,
                child,
                ancestor,
                ..
            } = old;
            if let Some(selection) = selection {
                let id = identity(&selection)?;
                let selection = self
                    .repository
                    .resume_ancestor_selection(id)?
                    .ok_or(LogError::StaleCursor)?;
                let step = self.repository.ancestor_selection_step_with_budget(
                    &selection,
                    OrderWorkBudget {
                        vertices: self.budget.rows,
                        edges: self.budget.edges,
                        bytes: self.budget.bytes,
                    },
                )?;
                self.work.set(PreparationStep {
                    rows: step.vertices,
                    edges: step.edges,
                    bytes: step.charged_bytes,
                    ..Default::default()
                });
                if step.complete {
                    let child = EventId::new(identity(
                        &child.ok_or_else(|| invalid("missing decision child"))?,
                    )?);
                    let ancestor = EventId::new(identity(
                        &ancestor.ok_or_else(|| invalid("missing decision ancestor"))?,
                    )?);
                    let answer = self
                        .repository
                        .ancestor_selection_contains(&selection, ancestor)?;
                    let key = relation_key(child, ancestor);
                    self.profile.database().transaction(|c| {
                        self.guard(c)?;
                        c.execute("INSERT OR IGNORE INTO channel_decision_answers VALUES(?1,?2,?3,NULL,?4)",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),key,answer])?;
                        let current:Option<Vec<u8>>=c.query_row("SELECT selection FROM channel_decision_work WHERE job=?1 AND channel=?2",params![self.job.as_slice(),self.channel.as_bytes().as_slice()],|r|r.get(0))?;
                        if current.as_deref()!=Some(id.as_slice()) {return Err(LogError::StaleCursor);}
                        c.execute("UPDATE channel_decision_work SET selection=NULL,child=NULL,ancestor=NULL WHERE job=?1 AND channel=?2",params![self.job.as_slice(),self.channel.as_bytes().as_slice()])?;
                        c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query=?1",[id.as_slice()])?;
                        Ok(())
                    })?;
                }
                return Ok(false);
            }
        } else {
            self.profile.database().transaction(|c| {
                self.source_guard(c)?;
                c.execute(
                    "INSERT OR IGNORE INTO channel_decision_work VALUES(?1,?2,?3,?4,?5,NULL,NULL,NULL,0)",
                    params![
                        self.job.as_slice(),
                        self.channel.as_bytes().as_slice(),
                        self.event.as_bytes().as_slice(),
                        self.source_job.as_slice(),
                        self.revision
                    ],
                )?;
                self.guard(c)

            })?;
        }
        let cleaned = self
            .repository
            .ancestor_cleanup_step(self.budget.rows.min(self.budget.bytes / 64))?;
        if cleaned > 0 {
            self.work.set(PreparationStep {
                rows: cleaned,
                bytes: cleaned * 64,
                ..Default::default()
            });
            return Ok(false);
        }
        Ok(true)
    }
    pub(in crate::node) fn complete_on(&self, c: &Connection) -> Result<(), LogError> {
        self.guard(c)?;
        c.execute("UPDATE channel_decision_work SET retired=1 WHERE job=?1 AND channel=?2 AND event=?3 AND source_job=?4 AND revision=?5",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),self.event.as_bytes().as_slice(),self.source_job.as_slice(),self.revision])?;
        Ok(())
    }
    fn answer(&self, key: &[u8]) -> Result<PredicateAnswer, LogError> {
        self.profile.database().read(|c| {self.guard(c)?;c.query_row("SELECT after_key,answer FROM channel_decision_answers WHERE job=?1 AND channel=?2 AND key=?3",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),key],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into)})
    }
}
impl AuthorityGraph for PreparingGraph<'_> {
    fn event(&self, id: EventId) -> Result<Option<Cow<'_, Event>>, LogError> {
        if self.pending() {
            return Ok(None);
        }
        let Some(header) = self.repository.header(&id)? else {
            return Ok(None);
        };
        if header.conversation != self.channel {
            return Ok(None);
        }
        let bytes = header
            .stored_bytes
            .checked_mul(3)
            .and_then(|v| v.checked_add(std::mem::size_of::<Event>()))
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if !self.admit_fact(bytes)? {
            return Ok(None);
        }
        Ok(self.repository.get(&id)?.map(Cow::Owned))
    }
    fn descends(&self, child: EventId, ancestor: EventId) -> Result<bool, LogError> {
        if self.pending() {
            return Ok(false);
        }
        let key = relation_key(child, ancestor);
        if let Some((_, Some(answer))) = self.answer(&key)? {
            return Ok(answer);
        }
        // The initial exact rank lookup is source work too. Charge it before
        // creating the persisted walk, just as later vertices are charged.
        if !self.admit_fact(64)? {
            return Ok(false);
        }
        let selection = self.repository.begin_ancestor_selection(self.channel)?;
        if let Some(mut selection) = selection {
            if !self
                .repository
                .seed_ancestor_selection(&mut selection, &[child], true, true)?
                .0
            {
                self.save_predicate(&key, None, Some(false))?;
                return Ok(false);
            }
            let changed=self.profile.database().transaction(|c| {
                self.guard(c)?;
                let changed=c.execute("UPDATE channel_decision_work SET selection=?3,child=?4,ancestor=?5 WHERE job=?1 AND channel=?2 AND selection IS NULL AND retired=0",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),selection.id().as_slice(),child.as_bytes().as_slice(),ancestor.as_bytes().as_slice()])?;
                if changed==1 {self.repository.persist_ancestor_selection_on(c,&selection)?;}
                Ok(changed)
            })?;
            if changed == 1 {
                selection.persist_with_owner();
            }
            self.pending.set(true);
            Ok(false)
        } else {
            self.save_predicate(&key, None, Some(false))?;
            Ok(false)
        }
    }
    fn pending(&self) -> bool {
        self.pending.get()
    }
    fn predicate(&self, key: &[u8]) -> Result<PredicateProgress, LogError> {
        if self.pending() {
            return Ok(PredicateProgress::Pending);
        }
        Ok(match self.answer(key)? {
            Some((_, Some(answer))) => PredicateProgress::Complete(answer),
            Some((after, None)) => PredicateProgress::Scan(after),
            None => PredicateProgress::Scan(None),
        })
    }
    fn save_predicate(
        &self,
        key: &[u8],
        after: Option<&[u8]>,
        answer: Option<bool>,
    ) -> Result<(), LogError> {
        if self.pending() {
            return Ok(());
        }
        self.profile.database().transaction(|c| {self.guard(c)?;c.execute("INSERT INTO channel_decision_answers VALUES(?1,?2,?3,?4,?5) ON CONFLICT(job,channel,key) DO UPDATE SET after_key=excluded.after_key,answer=excluded.answer",params![self.job.as_slice(),self.channel.as_bytes().as_slice(),key,after,answer])?;Ok(())})
    }
    fn admit_fact(&self, bytes: usize) -> Result<bool, LogError> {
        if self.pending() {
            return Ok(false);
        }
        if bytes > self.budget.bytes {
            return Err(LogError::PageBudgetTooSmall { required: bytes });
        }
        let mut work = self.work.get();
        if work.rows >= self.budget.rows || bytes > self.budget.bytes - work.bytes {
            self.pending.set(true);
            return Ok(false);
        }
        work.rows += 1;
        work.bytes += bytes;
        self.work.set(work);
        Ok(true)
    }
}
pub(in crate::node) fn retire_selection_on(
    c: &Connection,
    job: &[u8],
    channel: &[u8],
) -> Result<(), LogError> {
    c.execute("UPDATE event_ancestry_queries SET retired=1 WHERE query=(SELECT selection FROM channel_decision_work WHERE job=?1 AND channel=?2)",params![job,channel])?;
    Ok(())
}
fn relation_key(child: EventId, ancestor: EventId) -> Vec<u8> {
    let mut key = Vec::with_capacity(65);
    key.push(0);
    key.extend_from_slice(child.as_bytes());
    key.extend_from_slice(ancestor.as_bytes());
    key
}
fn identity<const N: usize>(bytes: &[u8]) -> Result<[u8; N], LogError> {
    bytes
        .try_into()
        .map_err(|_| invalid("invalid decision identity"))
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}

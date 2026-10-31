//! Bounded directory observations reuse the retained raw-control selector and
//! runtime-owned ancestry marks. Incomplete observations cannot be published.
use super::*;
use crate::eventlog::repository::{AncestorSelection, EventRepository};
use std::collections::VecDeque;

const SELECTOR: i64 = 1;
const ROWS: usize = 128;
const EDGES: usize = 512;
const BYTES: usize = 8 * 1024 * 1024;
const WIRE_IDS: usize = crate::node::account_documents::MAX_DIRECTORY_IDS;
const DIRECTORY_KINDS: [EventKind; 9] = [
    EventKind::MembershipChange,
    EventKind::ChannelOwnerTransfer,
    EventKind::ChannelLeave,
    EventKind::ChannelAccept,
    EventKind::ChannelDissolve,
    EventKind::ChannelAccountAuthority,
    EventKind::ChannelDeviceInheritance,
    EventKind::ChannelRename,
    EventKind::ChannelHistoryPolicy,
];
type StoredDirectory = (Vec<u8>, i64, i64, Vec<u8>);
const SQL: &str = "
CREATE TABLE group_raw_selected_views(selector INTEGER NOT NULL CHECK(selector IN(0,1)),channel BLOB NOT NULL CHECK(length(channel)=32),source_key BLOB NOT NULL CHECK(length(source_key) IN(9,33)),fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),event_count INTEGER NOT NULL CHECK(event_count>=0),frontier_count INTEGER NOT NULL CHECK(frontier_count>=0),controls BLOB NOT NULL CHECK(length(controls)<=16384 AND length(controls)%32=0),PRIMARY KEY(selector,channel)) WITHOUT ROWID;
CREATE TABLE group_raw_selected_frontiers(selector INTEGER NOT NULL CHECK(selector IN(0,1)),channel BLOB NOT NULL CHECK(length(channel)=32),event BLOB NOT NULL CHECK(length(event)=32),PRIMARY KEY(selector,channel,event)) WITHOUT ROWID;";

#[derive(Default)]
pub(in crate::node) struct RawViewCache {
    initialized: bool,
    active: VecDeque<Preparation>,
}
pub(in crate::node) struct DirectoryView {
    pub fingerprint: [u8; 32],
    pub count: usize,
    pub controls: Vec<String>,
    pub frontier: Vec<EventId>,
    group: ConversationId,
    source: Vec<u8>,
}
impl DirectoryView {
    pub fn contains(&self, node: &Node, hex: &str) -> Result<bool, crate::eventlog::LogError> {
        let Ok(bytes) = hex::decode(hex) else {
            return Ok(false);
        };
        if bytes.len() != 32 || hex::encode(&bytes) != hex {
            return Ok(false);
        }
        node.storage.database().read(|c| {
            c.query_row("SELECT EXISTS(SELECT 1 FROM retained_events e JOIN event_conversations v ON v.domain=e.domain AND v.conversation=e.conversation AND v.lifecycle=0 WHERE e.domain=?1 AND e.conversation=?2 AND e.id=?3 AND e.kind IN (?4,?5,?6,?7,?8,?9,?10,?11,?12))",params![DOMAIN.as_slice(),self.group.as_bytes().as_slice(),bytes,DIRECTORY_KINDS[0] as i64,DIRECTORY_KINDS[1] as i64,DIRECTORY_KINDS[2] as i64,DIRECTORY_KINDS[3] as i64,DIRECTORY_KINDS[4] as i64,DIRECTORY_KINDS[5] as i64,DIRECTORY_KINDS[6] as i64,DIRECTORY_KINDS[7] as i64,DIRECTORY_KINDS[8] as i64],|r|r.get(0)).map_err(Into::into)
        })
    }
    pub fn check(&self, repository: &EventRepository) -> Result<(), crate::eventlog::LogError> {
        if source_key(repository, self.group)? != self.source {
            return Err(crate::eventlog::LogError::StaleCursor);
        }
        Ok(())
    }
}
struct Preparation {
    group: ConversationId,
    source: Vec<u8>,
    phase: Phase,
    digest: Sha256,
    count: usize,
    prefix: Vec<EventId>,
    frontier_count: usize,
    after: Option<EventId>,
    current: Option<(EventId, Option<EventId>)>,
    last_seed: Option<EventId>,
    selection: Option<AncestorSelection>,
}
#[derive(Clone, Copy)]
enum Phase {
    Clear,
    Order,
    Start,
    Controls,
    FinishSeeds,
    Ancestry,
    Frontier,
    Publish,
}
fn pending() -> crate::eventlog::LogError {
    std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        "group directory observation is preparing",
    )
    .into()
}
fn source_key(
    repository: &EventRepository,
    group: ConversationId,
) -> Result<Vec<u8>, crate::eventlog::LogError> {
    super::selected_cache_key(
        repository,
        group,
        repository.metadata(&group)?.map_or(0, |m| m.revision),
        &DIRECTORY_KINDS,
    )
}
fn initialize(node: &Node) -> Result<(), crate::eventlog::LogError> {
    node.storage.database().transaction(|c| {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='group_raw_selected_views')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            c.execute_batch(SQL)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, SQL)
    })
}
fn headers(
    node: &Node,
    group: ConversationId,
    after: Option<EventId>,
    limit: usize,
) -> Result<Vec<(EventId, i64)>, crate::eventlog::LogError> {
    node.storage.database().read(|c|{
        let mut q=c.prepare("SELECT id,kind FROM retained_events INDEXED BY retained_events_ids WHERE domain=?1 AND conversation=?2 AND id>COALESCE(?3,x'') ORDER BY id LIMIT ?4")?;
        let values=q.query_map(params![DOMAIN.as_slice(),group.as_bytes().as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()),limit as i64],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,i64>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
        values.into_iter().map(|(bytes,kind)|Ok((id(bytes)?,kind))).collect()
    })
}
fn relevant(tag: i64) -> bool {
    let Some(kind) = DIRECTORY_KINDS.iter().find(|kind| **kind as i64 == tag) else {
        return false;
    };
    crate::node::account_group::wire::authorization_control(*kind)
        || matches!(
            kind,
            EventKind::ChannelRename | EventKind::ChannelHistoryPolicy
        )
}

fn cached(
    node: &Node,
    repository: &EventRepository,
    group: ConversationId,
    source: &[u8],
) -> Result<Option<DirectoryView>, crate::eventlog::LogError> {
    let row:Option<StoredDirectory>=node.storage.database().read(|c|c.query_row("SELECT fingerprint,event_count,frontier_count,controls FROM group_raw_selected_views WHERE selector=?1 AND channel=?2 AND source_key=?3",params![SELECTOR,group.as_bytes().as_slice(),source],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(Into::into))?;
    let Some((fingerprint, count, frontier_count, control_bytes)) = row else {
        return Ok(None);
    };
    // This explicit final output keeps the pre-existing local receipt bound;
    // wire witnesses below remain limited to 512 identities.
    let frontier=node.storage.database().read(|c|{
        let count=frontier_count;
        if count<0 || count as usize>crate::node::account_documents::MAX_LOCAL_FRONTIER {return Err(invalid());}
        let required=(count as usize).checked_mul(32).and_then(|n|n.checked_add(WIRE_IDS*64)).ok_or_else(invalid)?;
        if required>BYTES {return Err(crate::eventlog::LogError::PageBudgetTooSmall{required});}
        let mut q=c.prepare("SELECT event FROM group_raw_selected_frontiers WHERE selector=?1 AND channel=?2 ORDER BY event LIMIT ?3")?;
        let mut rows=q.query(params![SELECTOR,group.as_bytes().as_slice(),crate::node::account_documents::MAX_LOCAL_FRONTIER as i64])?;
        let mut frontier=Vec::with_capacity(count as usize);
        while let Some(row)=rows.next()? {frontier.push(id(row.get(0)?)?);}
        if frontier.len()!=count as usize {return Err(invalid());}
        Ok(frontier)
    })?;
    let (chunks, remainder) = control_bytes.as_chunks::<32>();
    if !remainder.is_empty()
        || chunks.len() != usize::try_from(count).map_err(|_| invalid())?.min(WIRE_IDS)
        || frontier_count > count
    {
        return Err(invalid());
    }
    let controls = chunks.iter().map(hex::encode).collect();
    let view = DirectoryView {
        fingerprint: fingerprint.try_into().map_err(|_| invalid())?,
        count: usize::try_from(count).map_err(|_| invalid())?,
        controls,
        frontier,
        group,
        source: source.to_vec(),
    };
    view.check(repository)?;
    Ok(Some(view))
}
pub(in crate::node::account_group) fn read(
    node: &Node,
    log: &PersistentEventLog,
    group: ConversationId,
) -> Result<DirectoryView, crate::eventlog::LogError> {
    let repository = log.repository()?;
    let source = source_key(repository, group)?;
    let mut cache = node
        .raw_group_views
        .lock()
        .expect("raw group view work mutex");
    if !cache.initialized {
        initialize(node)?;
        cache.initialized = true;
    }
    if let Some(view) = cached(node, repository, group, &source)? {
        return Ok(view);
    }
    if let Some(index) = cache
        .active
        .iter()
        .position(|p| p.group == group && p.source != source)
    {
        cache.active.remove(index);
    }
    let index = if let Some(index) = cache.active.iter().position(|p| p.group == group) {
        index
    } else {
        if cache.active.len() == 64 {
            // Advance an abandoned request instead of evicting active work or
            // permanently reserving every slot for callers that never return.
            let mut oldest = cache.active.pop_front().expect("full preparation cache");
            match oldest.step(node, repository) {
                Ok(false) => cache.active.push_back(oldest),
                Ok(true) | Err(crate::eventlog::LogError::StaleCursor) => {}
                Err(error) => {
                    cache.active.push_back(oldest);
                    return Err(error);
                }
            }
            node.account_sync_notify.notify_one();
            return Err(pending());
        }
        let mut digest = Sha256::new();
        digest.update(b"mesh-talk-group-directory-v1");
        cache.active.push_back(Preparation {
            group,
            source: source.clone(),
            phase: Phase::Clear,
            digest,
            count: 0,
            prefix: Vec::new(),
            frontier_count: 0,
            after: None,
            current: None,
            last_seed: None,
            selection: None,
        });
        cache.active.len() - 1
    };
    let mut preparation = cache.active.remove(index).expect("selected preparation");
    match preparation.step(node, repository) {
        Ok(true) => cached(node, repository, group, &source)?.ok_or_else(invalid),
        Ok(false) => {
            cache.active.push_back(preparation);
            node.account_sync_notify.notify_one();
            Err(pending())
        }
        Err(crate::eventlog::LogError::StaleCursor) => Err(crate::eventlog::LogError::StaleCursor),
        Err(error) => {
            cache.active.push_back(preparation);
            Err(error)
        }
    }
}
impl Preparation {
    fn step(
        &mut self,
        node: &Node,
        repository: &EventRepository,
    ) -> Result<bool, crate::eventlog::LogError> {
        if source_key(repository, self.group)? != self.source {
            return Err(crate::eventlog::LogError::StaleCursor);
        }
        match self.phase {
            Phase::Clear => {
                let removed=node.storage.database().transaction(|c|{
                    let owner=c.execute("DELETE FROM group_raw_selected_views WHERE selector=?1 AND channel=?2",params![SELECTOR,self.group.as_bytes().as_slice()])?;
                    let removed=c.execute("DELETE FROM group_raw_selected_frontiers WHERE selector=?1 AND channel=?2 AND event IN(SELECT event FROM group_raw_selected_frontiers WHERE selector=?1 AND channel=?2 ORDER BY event LIMIT ?3)",params![SELECTOR,self.group.as_bytes().as_slice(),(ROWS-owner) as i64])?;
                    Ok(owner+removed)
                })?;
                if removed == 0 {
                    self.phase = Phase::Order;
                }
            }
            Phase::Order => {
                let work = repository.order_step(
                    &self.group,
                    CausalOrder::Logical,
                    OrderWorkBudget {
                        vertices: ROWS,
                        edges: EDGES,
                        bytes: BYTES,
                    },
                )?;
                if work.readiness == OrderReadiness::Ready {
                    self.source = source_key(repository, self.group)?;
                    self.phase = Phase::Start;
                }
            }
            Phase::Start => {
                self.selection = repository.begin_ancestor_selection(self.group)?;
                self.phase = Phase::Controls;
            }
            Phase::Controls => {
                let mut rows = 0;
                while rows < ROWS {
                    if self.current.is_none() {
                        let next = headers(node, self.group, self.after, 1)?;
                        rows += 1;
                        let Some((event, kind)) = next.first().copied() else {
                            self.phase = Phase::FinishSeeds;
                            self.after = None;
                            break;
                        };
                        if !relevant(kind) {
                            self.after = Some(event);
                            continue;
                        }
                        if self.prefix.len() < WIRE_IDS {
                            self.prefix.push(event);
                        }
                        self.digest.update(event.to_hex().as_bytes());
                        self.count = self.count.checked_add(1).ok_or_else(invalid)?;
                        self.current = Some((event, None));
                    }
                    let room = (ROWS - rows) / 2;
                    if room == 0 {
                        break;
                    }
                    let (event, after) = self.current.expect("selected control");
                    let parents=node.storage.database().read(|c|{
                        let mut q=c.prepare("SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent>COALESCE(?3,x'') ORDER BY parent LIMIT ?4")?;
                        let bytes=q.query_map(params![DOMAIN.as_slice(),event.as_bytes().as_slice(),after.as_ref().map(|id|id.as_bytes().as_slice()),room as i64],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?;
                        bytes.into_iter().map(id).collect::<Result<Vec<_>,_>>()
                    })?;
                    rows += parents.len() * 2;
                    if !parents.is_empty() {
                        let selection = self.selection.as_mut().ok_or_else(invalid)?;
                        if !repository
                            .seed_ancestor_selection(selection, &parents, false, true)?
                            .0
                        {
                            return Err(crate::eventlog::LogError::StaleCursor);
                        }
                        self.last_seed = parents.last().copied();
                    }
                    if parents.len() < room {
                        self.after = Some(event);
                        self.current = None;
                    } else {
                        self.current = Some((event, parents.last().copied()));
                    }
                }
            }
            Phase::FinishSeeds => {
                if let (Some(selection), Some(last)) = (&mut self.selection, self.last_seed) {
                    repository.seed_ancestor_selection(selection, &[last], true, true)?;
                    self.phase = Phase::Ancestry;
                } else {
                    self.selection = None;
                    self.phase = Phase::Frontier;
                }
            }
            Phase::Ancestry => {
                let work = repository.ancestor_selection_step_with_budget(
                    self.selection.as_ref().ok_or_else(invalid)?,
                    OrderWorkBudget {
                        vertices: ROWS,
                        edges: EDGES,
                        bytes: BYTES,
                    },
                )?;
                if work.complete {
                    self.phase = Phase::Frontier;
                }
            }
            Phase::Frontier => {
                let page = headers(node, self.group, self.after, ROWS / 3)?;
                for (event, kind) in &page {
                    if !relevant(*kind) {
                        self.after = Some(*event);
                        continue;
                    }
                    let ancestor = self
                        .selection
                        .as_ref()
                        .map(|selection| repository.ancestor_selection_contains(selection, *event))
                        .transpose()?
                        .unwrap_or(false);
                    if !ancestor {
                        let inserted=node.storage.database().transaction(|c|c.execute("INSERT OR IGNORE INTO group_raw_selected_frontiers VALUES(?1,?2,?3)",params![SELECTOR,self.group.as_bytes().as_slice(),event.as_bytes().as_slice()]).map_err(Into::into))?;
                        self.frontier_count = self
                            .frontier_count
                            .checked_add(inserted)
                            .ok_or_else(invalid)?;
                    }
                    self.after = Some(*event);
                }
                if page.is_empty() {
                    self.phase = Phase::Publish;
                }
            }
            Phase::Publish => {
                let fingerprint: [u8; 32] = self.digest.clone().finalize().into();
                let controls: Vec<u8> = self
                    .prefix
                    .iter()
                    .flat_map(|id| id.as_bytes().iter().copied())
                    .collect();
                node.storage.database().transaction(|c| {
                    c.execute(
                        "INSERT INTO group_raw_selected_views VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![
                            SELECTOR,
                            self.group.as_bytes().as_slice(),
                            &self.source,
                            fingerprint.as_slice(),
                            i64::try_from(self.count).map_err(|_| invalid())?,
                            i64::try_from(self.frontier_count).map_err(|_| invalid())?,
                            controls
                        ],
                    )?;
                    Ok(())
                })?;
                return Ok(true);
            }
        }
        Ok(false)
    }
}

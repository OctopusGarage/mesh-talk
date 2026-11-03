//! Raw control observations include rejected controls. They are not the allowed
//! channel projection. The caller holds the public log mutex for this complete
//! observation; one disk mark set replaces history-sized ancestry collections.
use super::*;
use crate::eventlog::repository::order::{CausalOrder, OrderReadiness, OrderWorkBudget};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;
#[path = "view/directory.rs"]
mod directory;
pub(super) use directory::read as directory_view;
pub(in crate::node) use directory::{DirectoryView, RawViewCache};
type CachedAuthorization = (Vec<u8>, i64, Vec<u8>);

#[cfg(test)]
thread_local! { static COLD_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(super) fn cold_reads() -> usize {
    COLD_READS.with(std::cell::Cell::get)
}

const DOMAIN: &[u8; 16] = b"MeshPartEventsV1";
const SCHEMA: &str = "
CREATE TABLE group_raw_control_views(channel BLOB PRIMARY KEY CHECK(length(channel)=32),revision INTEGER NOT NULL,fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),event_count INTEGER NOT NULL CHECK(event_count>=0),frontier BLOB NOT NULL CHECK(length(frontier)<=131104 AND length(frontier)%32=0)) WITHOUT ROWID;
CREATE TABLE group_raw_control_marks(event BLOB PRIMARY KEY CHECK(length(event)=32)) WITHOUT ROWID;";

const INPUT_SCHEMA: &str = "CREATE TABLE group_raw_control_inputs(channel BLOB PRIMARY KEY CHECK(length(channel)=32),source_key BLOB NOT NULL CHECK(length(source_key) IN(9,33))) WITHOUT ROWID;";
// The wire cache remains limited to 4096 heads plus its overflow sentinel.
// A local publication receipt retains the separately capped physical frontier
// on disk; it is never kept in the 64-slot directory preparation cache.
const RECEIPT_SCHEMA: &str = "
CREATE TABLE group_raw_control_receipts(channel BLOB PRIMARY KEY CHECK(length(channel)=32),source_key BLOB NOT NULL CHECK(length(source_key) IN(9,33)),event_count INTEGER NOT NULL CHECK(event_count>=0 AND event_count<=131072)) WITHOUT ROWID;
CREATE TABLE group_raw_control_receipt_rows(channel BLOB NOT NULL CHECK(length(channel)=32),event BLOB NOT NULL CHECK(length(event)=32),PRIMARY KEY(channel,event)) WITHOUT ROWID;";
fn receipt_frontier(
    node: &Node,
    group: ConversationId,
    count: usize,
) -> Result<Vec<EventId>, crate::eventlog::LogError> {
    if count > crate::node::account_documents::MAX_LOCAL_FRONTIER {
        return Err(crate::eventlog::LogError::PageBudgetTooSmall {
            required: count.saturating_mul(32),
        });
    }
    node.storage.database().read(|c| {
        let mut query = c.prepare("SELECT event FROM group_raw_control_receipt_rows WHERE channel=?1 ORDER BY event LIMIT ?2")?;
        let mut rows = query.query(params![group.as_bytes().as_slice(),count as i64])?;
        let mut frontier = Vec::with_capacity(count);
        while let Some(row) = rows.next()? { frontier.push(id(row.get(0)?)?); }
        if frontier.len() != count { return Err(invalid()); }
        Ok(frontier)
    })
}
const CONTROL_KINDS: [EventKind; 7] = [
    EventKind::MembershipChange,
    EventKind::ChannelOwnerTransfer,
    EventKind::ChannelLeave,
    EventKind::ChannelAccept,
    EventKind::ChannelDissolve,
    EventKind::ChannelAccountAuthority,
    EventKind::ChannelDeviceInheritance,
];
/// A published causal generation changes for late parents and reordered sources.
/// Its safe tail keeps the same job: ordinary message appends cannot change a
/// frontier of retained controls. The tag prevents fallback revision values from
/// ever colliding with the optimized key (including installed older caches).
fn cache_key(
    repository: &crate::eventlog::repository::EventRepository,
    group: ConversationId,
    revision: i64,
) -> Result<Vec<u8>, crate::eventlog::LogError> {
    selected_cache_key(repository, group, revision, &CONTROL_KINDS)
}
fn selected_cache_key(
    repository: &crate::eventlog::repository::EventRepository,
    group: ConversationId,
    revision: i64,
    kinds: &[EventKind],
) -> Result<Vec<u8>, crate::eventlog::LogError> {
    if let Some(revisions) = repository.raw_kind_revisions(group)?
        && let Some(generation) = repository.order_generation(&group, CausalOrder::Logical)?
    {
        let control = kinds
            .iter()
            .map(|kind| revisions[*kind as usize])
            .max()
            .unwrap_or(-1);
        let destructive = repository.destructive_policy_revision()?;
        let mut key = Vec::with_capacity(33);
        key.push(1);
        key.extend_from_slice(&generation.job);
        key.extend_from_slice(&control.to_be_bytes());
        key.extend_from_slice(&destructive.to_be_bytes());
        return Ok(key);
    }
    let mut key = Vec::with_capacity(9);
    key.push(0);
    key.extend_from_slice(&revision.to_be_bytes());
    Ok(key)
}

fn invalid() -> crate::eventlog::LogError {
    crate::eventlog::LogError::CorruptFile("invalid raw group control view".into())
}
fn id(bytes: Vec<u8>) -> Result<EventId, crate::eventlog::LogError> {
    bytes.try_into().map(EventId::new).map_err(|_| invalid())
}
fn clear_marks(node: &Node) -> Result<(), crate::eventlog::LogError> {
    while node.storage.database().transaction(|c| {
        c.execute("DELETE FROM group_raw_control_marks WHERE event IN (SELECT event FROM group_raw_control_marks LIMIT 512)", []).map_err(Into::into)
    })? != 0 {}
    Ok(())
}

pub(super) fn read(
    node: &Node,
    log: &PersistentEventLog,
    group: ConversationId,
    scope: Option<&crate::node::channel::ChannelMembershipProjection>,
) -> Result<RawAuthorizationView, crate::eventlog::LogError> {
    let repository = log.repository()?;
    let revision = repository.metadata(&group)?.map_or(0, |meta| meta.revision);
    node.storage.database().transaction(|c| {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='group_raw_control_views')",
            [],
            |r| r.get(0),
        )?;
        if !exists {
            c.execute_batch(SCHEMA)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
        let inputs: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='group_raw_control_inputs')",
            [],
            |r| r.get(0),
        )?;
        if !inputs {
            c.execute_batch(INPUT_SCHEMA)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, INPUT_SCHEMA)?;
        let receipts: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='group_raw_control_receipts')",
            [],
            |r| r.get(0),
        )?;
        if !receipts {
            c.execute_batch(RECEIPT_SCHEMA)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, RECEIPT_SCHEMA)
    })?;
    if scope.is_none() {
        let source_key = cache_key(repository, group, revision)?;
        let cached: Option<CachedAuthorization> = node.storage.database().read(|c| {
            c.query_row("SELECT v.fingerprint,v.event_count,v.frontier FROM group_raw_control_views v JOIN group_raw_control_inputs i ON i.channel=v.channel WHERE v.channel=?1 AND i.source_key=?2", params![group.as_bytes().as_slice(),source_key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into)
        })?;
        if let Some((fingerprint, count, bytes)) = cached {
            if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_ACCESS_FRONTIER + 1 {
                return Err(invalid());
            }
            let receipt_count: Option<i64> = node.storage.database().read(|c| {
                c.query_row("SELECT event_count FROM group_raw_control_receipts WHERE channel=?1 AND source_key=?2",params![group.as_bytes().as_slice(),source_key], |r| r.get(0)).optional().map_err(Into::into)
            })?;
            let frontier = if bytes.len() / 32 <= MAX_ACCESS_FRONTIER {
                Some(
                    bytes
                        .as_chunks::<32>()
                        .0
                        .iter()
                        .copied()
                        .map(EventId::new)
                        .collect(),
                )
            } else {
                // An older truncated cache cannot stand in for the complete
                // physical receipt. Rebuild it from retained metadata once.
                receipt_count
                    .map(|count| {
                        receipt_frontier(
                            node,
                            group,
                            usize::try_from(count).map_err(|_| invalid())?,
                        )
                    })
                    .transpose()?
            };
            if let Some(frontier) = frontier {
                return Ok((
                    frontier,
                    (
                        fingerprint.try_into().map_err(|_| invalid())?,
                        usize::try_from(count).map_err(|_| invalid())?,
                    ),
                ));
            }
        }
    }
    #[cfg(test)]
    COLD_READS.with(|value| value.set(value.get() + 1));
    // Compute the unchanged wire fingerprint in identifier order. Selection
    // happens before any event body is accessed; at most128 IDs are retained.
    let mut digest = Sha256::new();
    digest.update(b"mesh-talk-group-authorization-view-v1");
    let mut count = 0_usize;
    let mut after: Option<Vec<u8>> = None;
    let kinds = CONTROL_KINDS.map(|kind| kind as i64);
    loop {
        let page: Vec<Vec<u8>> = node.storage.database().read(|c| {
            let mut query = c.prepare("SELECT id FROM retained_events INDEXED BY retained_events_ids WHERE domain=?1 AND conversation=?2 AND id>COALESCE(?3,x'') AND kind IN (?4,?5,?6,?7,?8,?9,?10) ORDER BY id LIMIT 128")?;
            query.query_map(params![DOMAIN.as_slice(),group.as_bytes().as_slice(),after,kinds[0],kinds[1],kinds[2],kinds[3],kinds[4],kinds[5],kinds[6]],|r|r.get(0))?.collect::<Result<_,_>>().map_err(Into::into)
        })?;
        if page.is_empty() {
            break;
        }
        for bytes in &page {
            if scope
                .map(|scope| scope.contains_event(id(bytes.clone())?))
                .transpose()?
                .unwrap_or(true)
            {
                digest.update(bytes);
                count = count.checked_add(1).ok_or_else(invalid)?;
            }
        }
        after = page.last().cloned();
    }
    let stamp: ([u8; 32], usize) = (digest.finalize().into(), count);
    clear_marks(node)?;
    node.storage.database().transaction(|c| {
        c.execute(
            "DELETE FROM group_raw_control_receipts WHERE channel=?1",
            [group.as_bytes().as_slice()],
        )?;
        Ok(())
    })?;
    while node.storage.database().transaction(|c| {
        c.execute("DELETE FROM group_raw_control_receipt_rows WHERE channel=?1 AND event IN(SELECT event FROM group_raw_control_receipt_rows WHERE channel=?1 LIMIT 128)",[group.as_bytes().as_slice()]).map_err(Into::into)
    })? != 0 {}
    let mut receipt_count = 0usize;
    while repository
        .order_step(&group, CausalOrder::Logical, OrderWorkBudget::default())?
        .readiness
        != OrderReadiness::Ready
    {}
    let mut frontier = BTreeSet::new();
    if let Some(generation) = repository.order_generation(&group, CausalOrder::Logical)? {
        let mut before = i64::try_from(generation.count).map_err(|_| invalid())?;
        loop {
            let page: Vec<(i64,Vec<u8>,i64)> = node.storage.database().read(|c| {
                let mut query = c.prepare("SELECT r.rank,r.id,e.kind FROM event_order_rows r JOIN retained_events e ON e.domain=?1 AND e.id=r.id WHERE r.job=?2 AND r.rank<?3 ORDER BY r.rank DESC LIMIT 128")?;
                query.query_map(params![DOMAIN.as_slice(),generation.job.as_slice(),before],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<Result<_,_>>().map_err(Into::into)
            })?;
            if page.is_empty() {
                break;
            }
            for (rank, bytes, kind) in page {
                before = rank;
                let event = id(bytes.clone())?;
                let relevant = kinds.contains(&kind)
                    && scope
                        .map(|scope| scope.contains_event(event))
                        .transpose()?
                        .unwrap_or(true);
                let marked: bool = node.storage.database().read(|c| {
                    c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM group_raw_control_marks WHERE event=?1)",
                        [&bytes],
                        |r| r.get(0),
                    )
                    .map_err(Into::into)
                })?;
                if relevant && !marked {
                    receipt_count += node.storage.database().transaction(|c| {
                        c.execute(
                            "INSERT OR IGNORE INTO group_raw_control_receipt_rows VALUES(?1,?2)",
                            params![group.as_bytes().as_slice(), event.as_bytes().as_slice()],
                        )
                        .map_err(Into::into)
                    })?;
                    if receipt_count > crate::node::account_documents::MAX_LOCAL_FRONTIER {
                        return Err(crate::eventlog::LogError::PageBudgetTooSmall {
                            required: receipt_count.saturating_mul(32),
                        });
                    }
                    frontier.insert(event);
                    if frontier.len() > MAX_ACCESS_FRONTIER + 1 {
                        frontier.pop_last();
                    }
                }
                if !relevant && !marked {
                    continue;
                }
                let mut parent_after: Option<Vec<u8>> = None;
                loop {
                    let parents: Vec<Vec<u8>> = node.storage.database().transaction(|c| {
                        let mut query = c.prepare("SELECT parent FROM retained_event_parents WHERE domain=?1 AND child=?2 AND parent>COALESCE(?3,x'') ORDER BY parent LIMIT 512")?;
                        let values = query.query_map(params![DOMAIN.as_slice(),&bytes,parent_after],|r|r.get(0))?.collect::<Result<Vec<Vec<u8>>,_>>()?;
                        for parent in &values { c.execute("INSERT OR IGNORE INTO group_raw_control_marks VALUES(?1)",[parent])?; }
                        Ok(values)
                    })?;
                    if parents.len() < 512 {
                        break;
                    }
                    parent_after = parents.last().cloned();
                }
            }
        }
    }
    clear_marks(node)?;
    if repository.metadata(&group)?.map_or(0, |meta| meta.revision) != revision {
        return Err(crate::eventlog::LogError::StaleCursor);
    }
    let frontier: Vec<_> = frontier.into_iter().collect();
    if scope.is_none() {
        let bytes: Vec<_> = frontier
            .iter()
            .flat_map(|id| id.as_bytes().iter().copied())
            .collect();
        let source_key = cache_key(repository, group, revision)?;
        node.storage.database().transaction(|c| {
            c.execute("INSERT INTO group_raw_control_receipts VALUES(?1,?2,?3) ON CONFLICT(channel) DO UPDATE SET source_key=excluded.source_key,event_count=excluded.event_count",params![group.as_bytes().as_slice(),&source_key,receipt_count as i64])?;
            c.execute("INSERT INTO group_raw_control_inputs VALUES(?1,?2) ON CONFLICT(channel) DO UPDATE SET source_key=excluded.source_key",params![group.as_bytes().as_slice(),source_key])?;
            c.execute("INSERT INTO group_raw_control_views VALUES(?1,?2,?3,?4,?5) ON CONFLICT(channel) DO UPDATE SET revision=excluded.revision,fingerprint=excluded.fingerprint,event_count=excluded.event_count,frontier=excluded.frontier",params![group.as_bytes().as_slice(),revision,stamp.0.as_slice(),i64::try_from(stamp.1).map_err(|_|invalid())?,bytes])?;
            Ok(())
        })?;
    }
    Ok((receipt_frontier(node, group, receipt_count)?, stamp))
}

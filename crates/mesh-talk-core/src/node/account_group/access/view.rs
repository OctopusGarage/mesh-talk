//! Raw control observations include rejected controls. They are not the allowed
//! channel projection. The caller holds the public log mutex for this complete
//! observation; one disk mark set replaces history-sized ancestry collections.
use super::*;
use crate::eventlog::repository::order::{CausalOrder, OrderReadiness, OrderWorkBudget};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeSet;

const DOMAIN: &[u8; 16] = b"MeshPartEventsV1";
const SCHEMA: &str = "
CREATE TABLE group_raw_control_views(channel BLOB PRIMARY KEY CHECK(length(channel)=32),revision INTEGER NOT NULL,fingerprint BLOB NOT NULL CHECK(length(fingerprint)=32),event_count INTEGER NOT NULL CHECK(event_count>=0),frontier BLOB NOT NULL CHECK(length(frontier)<=131104 AND length(frontier)%32=0)) WITHOUT ROWID;
CREATE TABLE group_raw_control_marks(event BLOB PRIMARY KEY CHECK(length(event)=32)) WITHOUT ROWID;";

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
) -> Result<(Vec<EventId>, ([u8; 32], usize)), crate::eventlog::LogError> {
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
        crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
    })?;
    if scope.is_none() {
        let cached: Option<(Vec<u8>, i64, Vec<u8>)> = node.storage.database().read(|c| {
            c.query_row("SELECT fingerprint,event_count,frontier FROM group_raw_control_views WHERE channel=?1 AND revision=?2", params![group.as_bytes().as_slice(),revision], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(Into::into)
        })?;
        if let Some((fingerprint, count, bytes)) = cached {
            if bytes.len() % 32 != 0 || bytes.len() / 32 > MAX_ACCESS_FRONTIER + 1 {
                return Err(invalid());
            }
            let frontier = bytes
                .chunks_exact(32)
                .map(|bytes| EventId::new(bytes.try_into().expect("fixed chunk")))
                .collect();
            return Ok((
                frontier,
                (
                    fingerprint.try_into().map_err(|_| invalid())?,
                    usize::try_from(count).map_err(|_| invalid())?,
                ),
            ));
        }
    }
    // Compute the unchanged wire fingerprint in identifier order. Selection
    // happens before any event body is accessed; at most128 IDs are retained.
    let mut digest = Sha256::new();
    digest.update(b"mesh-talk-group-authorization-view-v1");
    let mut count = 0_usize;
    let mut after: Option<Vec<u8>> = None;
    let kinds = [
        EventKind::MembershipChange,
        EventKind::ChannelOwnerTransfer,
        EventKind::ChannelLeave,
        EventKind::ChannelAccept,
        EventKind::ChannelDissolve,
        EventKind::ChannelAccountAuthority,
        EventKind::ChannelDeviceInheritance,
    ]
    .map(|kind| kind as i64);
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
        node.storage.database().transaction(|c| {
            c.execute("INSERT INTO group_raw_control_views VALUES(?1,?2,?3,?4,?5) ON CONFLICT(channel) DO UPDATE SET revision=excluded.revision,fingerprint=excluded.fingerprint,event_count=excluded.event_count,frontier=excluded.frontier",params![group.as_bytes().as_slice(),revision,stamp.0.as_slice(),i64::try_from(stamp.1).map_err(|_|invalid())?,bytes])?;
            Ok(())
        })?;
    }
    Ok((frontier, stamp))
}

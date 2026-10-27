//! Demand-driven exact federation digests and finite metadata inventories.
//! Preparation is derived work: its durable cold cursor becomes an append tail;
//! it is never reset by connection failure, cache publication, or retirement.
use super::*;
use sha2::{Digest, Sha256};
pub(crate) const SCHEMA: &str = "
CREATE TABLE relay_digest_preparation(singleton INTEGER PRIMARY KEY CHECK(singleton=1), generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(generation)='integer' AND generation>=0), ready_generation INTEGER NOT NULL DEFAULT 0 CHECK(typeof(ready_generation)='integer' AND ready_generation>=0 AND ready_generation<=generation), phase INTEGER NOT NULL DEFAULT 0 CHECK(typeof(phase)='integer' AND phase BETWEEN 0 AND 2), scan_after BLOB CHECK(scan_after IS NULL OR (typeof(scan_after)='blob' AND length(scan_after)=32)), scan_upper BLOB CHECK(scan_upper IS NULL OR (typeof(scan_upper)='blob' AND length(scan_upper)=32)), tail_after INTEGER NOT NULL DEFAULT 0 CHECK(typeof(tail_after)='integer' AND tail_after>=0), queued INTEGER NOT NULL DEFAULT 0 CHECK(typeof(queued)='integer' AND queued BETWEEN 0 AND 1024), next_ticket INTEGER NOT NULL DEFAULT 1 CHECK(typeof(next_ticket)='integer' AND next_ticket>0));
CREATE TABLE relay_digest_demands(conversation BLOB PRIMARY KEY CHECK(typeof(conversation)='blob' AND length(conversation)=32), ticket INTEGER NOT NULL UNIQUE CHECK(typeof(ticket)='integer' AND ticket>0)) WITHOUT ROWID;
CREATE TABLE relay_federation_digests(conversation BLOB PRIMARY KEY CHECK(typeof(conversation)='blob' AND length(conversation)=32), revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0), destructive_epoch INTEGER NOT NULL CHECK(typeof(destructive_epoch)='integer' AND destructive_epoch>=0), digest BLOB NOT NULL CHECK(typeof(digest)='blob' AND length(digest)=32), wire_fingerprint BLOB NOT NULL CHECK(typeof(wire_fingerprint)='blob' AND length(wire_fingerprint)=32)) WITHOUT ROWID;
";
const PAGE: usize = 512;
const MAX_DEMANDS: i64 = 1024;
// Fixed 256-byte logical reservation per demand, <=256KiB in total. No duplicate
// retained-event counters; SQLite/file/WAL/RSS accounting is a separate boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Binding {
    revision: i64,
    epoch: i64,
    count: u64,
    first: u64,
}
#[derive(Clone, Copy)]
struct CachedDigest {
    federation: [u8; 32],
    #[cfg(test)]
    wire: [u8; 32],
}
fn id(bytes: Vec<u8>) -> Result<ConversationId, LogError> {
    Ok(ConversationId::new(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid inventory conversation"))?,
    ))
}
fn next(value: i64) -> Result<i64, LogError> {
    value
        .checked_add(1)
        .ok_or(LogError::RepositoryCounterExhausted)
}
pub(crate) fn validate_preparation_on(c: &Connection) -> Result<(), LogError> {
    type PreparationRow = (
        i64,
        i64,
        i64,
        Option<Vec<u8>>,
        Option<Vec<u8>>,
        i64,
        i64,
        i64,
    );
    let row: Option<PreparationRow>=c.query_row("SELECT generation,ready_generation,phase,scan_after,scan_upper,tail_after,queued,next_ticket FROM relay_digest_preparation WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
    let Some((generation, ready, phase, after, upper, tail, queued, ticket)) = row else {
        return Err(invalid("missing digest preparation authority"));
    };
    if generation < 0
        || ready < 0
        || ready > generation
        || !(0..=2).contains(&phase)
        || tail < 0
        || !(0..=MAX_DEMANDS).contains(&queued)
        || ticket <= 0
        || after.as_ref().is_some_and(|v| v.len() != 32)
        || upper.as_ref().is_some_and(|v| v.len() != 32)
        || (phase == 0
            && (generation != 0 || ready != 0 || after.is_some() || upper.is_some() || tail != 0))
        || (phase != 0 && generation == 0)
        || (after
            .as_ref()
            .zip(upper.as_ref())
            .is_some_and(|(a, u)| a > u))
    {
        return Err(invalid("invalid digest preparation authority"));
    }
    let commit: i64 = c.query_row(
        "SELECT next_commit FROM event_domains WHERE domain=?1",
        [RELAY_EVENTS.0.as_slice()],
        |r| r.get(0),
    )?;
    if tail > commit {
        return Err(invalid("digest preparation cursor exceeds repository"));
    }
    Ok(())
}
/// Transaction-owned visibility invalidation: checked overflow rolls back the
/// authoritative role operation, never permits stale inventory validation.
pub(super) fn invalidate_visibility_on(c: &Connection) -> Result<(), LogError> {
    let epoch: i64 = c.query_row(
        "SELECT inventory_visibility_epoch FROM relay_runtime WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    c.execute(
        "UPDATE relay_runtime SET inventory_visibility_epoch=?1 WHERE singleton=1",
        [next(epoch)?],
    )?;
    Ok(())
}
pub(super) fn remove_demand_on(c: &Connection, conv: &ConversationId) -> Result<(), LogError> {
    let removed = c.execute(
        "DELETE FROM relay_digest_demands WHERE conversation=?1",
        [conv.as_bytes().as_slice()],
    )?;
    if removed != 0 {
        c.execute(
            "UPDATE relay_digest_preparation SET queued=queued-1 WHERE singleton=1",
            [],
        )?;
    }
    Ok(())
}
pub(super) fn retire_on(c: &Connection, conv: &ConversationId) -> Result<(), LogError> {
    invalidate_visibility_on(c)?;
    remove_demand_on(c, conv)?;
    c.execute(
        "DELETE FROM relay_federation_digests WHERE conversation=?1",
        [conv.as_bytes().as_slice()],
    )?;
    Ok(())
}
impl IndexedRelayState {
    fn digest_binding_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        cutoff: Option<u64>,
    ) -> Result<Option<Binding>, LogError> {
        let m = match self.eligible_on(c, conv, cutoff) {
            Ok(Some(m)) => m,
            Ok(None) | Err(LogError::RelayMailboxUnavailable | LogError::ConversationRetiring) => {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let (revision,epoch):(i64,i64)=c.query_row("SELECT revision,destructive_epoch FROM event_conversations WHERE domain=?1 AND conversation=?2",params![RELAY_EVENTS.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        if revision < 0 || epoch < 0 {
            return Err(invalid("invalid digest repository binding"));
        }
        Ok(Some(Binding {
            revision,
            epoch,
            count: m.retained.count,
            first: m.first_seen,
        }))
    }
    fn cached_digest_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        binding: Binding,
    ) -> Result<Option<CachedDigest>, LogError> {
        let row:Option<(i64,i64,Vec<u8>,Vec<u8>)>=c.query_row("SELECT revision,destructive_epoch,digest,wire_fingerprint FROM relay_federation_digests WHERE conversation=?1",[conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        let Some((revision, epoch, digest, wire)) = row else {
            return Ok(None);
        };
        if revision < 0 || epoch < 0 || revision > binding.revision || epoch > binding.epoch {
            return Err(invalid("digest cache exceeds repository binding"));
        }
        let federation = digest
            .try_into()
            .map_err(|_| invalid("malformed federation digest"))?;
        let _wire: [u8; 32] = wire
            .try_into()
            .map_err(|_| invalid("malformed wire fingerprint"))?;
        Ok(
            (revision == binding.revision && epoch == binding.epoch).then_some(CachedDigest {
                federation,
                #[cfg(test)]
                wire: _wire,
            }),
        )
    }
}
pub(crate) mod preparation;
#[cfg(test)]
mod tests;
pub(crate) mod view;

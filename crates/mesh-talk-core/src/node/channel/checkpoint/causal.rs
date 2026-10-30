//! Causal membership queries share the channel reducer and its exact indexed
//! facts. A serialized, disposable query never retains an ancestor ID set in RAM.
use super::*;
use crate::{
    eventlog::repository::EventRepository,
    identity::device::DeviceIdentity,
    node::channel::{graph::IndexedGraph, projection_facts::ProjectionFacts},
};

const SCHEMA: &str = "
CREATE TABLE channel_projection_queries(job BLOB PRIMARY KEY CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),retired INTEGER NOT NULL CHECK(retired IN(0,1))) WITHOUT ROWID;
CREATE TABLE channel_causal_states(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),event BLOB NOT NULL CHECK(length(event)=32),state BLOB,PRIMARY KEY(job,channel,event)) WITHOUT ROWID;
";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    // At most one serialized query can be active. Its potentially large facts
    // remain deferred cleanup, never an all-row warm-start replay.
    c.execute("UPDATE channel_projection_queries SET retired=1", [])?;
    Ok(())
}

struct Query<'a> {
    store: &'a ChannelCheckpointStore,
    job: [u8; 16],
}
impl Drop for Query<'_> {
    fn drop(&mut self) {
        if let Err(error) = self.store.profile.database().transaction(|c| {
            c.execute(
                "UPDATE channel_projection_queries SET retired=1 WHERE job=?1",
                [self.job.as_slice()],
            )?;
            cleanup_on(c).map(|_| ())
        }) {
            log::warn!("channel causal query cleanup deferred: {error}");
        }
    }
}
fn cleanup_on(c: &Connection) -> Result<bool, LogError> {
    let row: Option<(Vec<u8>, Vec<u8>)> = c
        .query_row(
            "SELECT job,channel FROM channel_projection_queries WHERE retired=1 LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((job, channel)) = row else {
        return Ok(false);
    };
    for (table, key) in [
        ("channel_event_authority", "event"),
        ("channel_departure_facts", "member,event"),
        ("channel_membership_snapshots", "rank"),
        ("channel_authority_facts", "kind,key"),
        ("channel_authority_current", "channel"),
        ("channel_projection_work", "channel"),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE job=?1 AND channel=?2 AND ({key}) IN (SELECT {key} FROM {table} WHERE job=?1 AND channel=?2 LIMIT 128)"
        );
        if c.execute(&sql, params![&job, &channel])? != 0 {
            return Ok(true);
        }
    }
    c.execute("DELETE FROM channel_projection_queries WHERE job=?1", [job])?;
    Ok(true)
}

impl ChannelCheckpointStore {
    /// Membership established by strict signed ancestors, excluding concurrent
    /// controls that merely appear earlier in canonical order. Safe tail appends
    /// reuse the immutable result; changed canonical generations do not.
    pub(in crate::node) fn causal_state(
        &self,
        repository: &EventRepository,
        me: &DeviceIdentity,
        channel: ConversationId,
        event: EventId,
    ) -> Result<Option<ChannelState>, LogError> {
        let _serial = self.causal_query.lock().expect("channel causal query lock");
        let binding = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row: Option<(Vec<u8>,i64)> = c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((job,revision)) = row else { return Ok(None); };
            let job: [u8;16] = job.try_into().map_err(|_|invalid("invalid channel current job"))?;
            replay::require_source_on(c,channel,job,revision)?;
            let cached: Option<Option<Vec<u8>>> = c.query_row("SELECT state FROM channel_causal_states WHERE job=?1 AND channel=?2 AND event=?3",params![job.as_slice(),channel.as_bytes().as_slice(),event.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            Ok(Some((job,revision,cached)))
        })?;
        let Some((source_job, revision, cached)) = binding else {
            return Ok(None);
        };
        if let Some(cached) = cached {
            return cached
                .map(|bytes| {
                    ChannelState::decode_current(&bytes)
                        .map_err(|error| invalid(&error.to_string()))
                })
                .transpose();
        }
        while self.profile.database().transaction(|c| cleanup_on(c))? {}
        let Some(selection) = repository.select_ancestors(channel, event)? else {
            return Ok(None);
        };
        if selection.generation() != (source_job, revision) {
            return Err(LogError::StaleCursor);
        }
        let query = Query {
            store: self,
            job: *uuid::Uuid::new_v4().as_bytes(),
        };
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, source_job, revision)?;
            c.execute(
                "INSERT INTO channel_projection_queries VALUES(?1,?2,0)",
                params![query.job.as_slice(), channel.as_bytes().as_slice()],
            )?;
            Ok(())
        })?;
        let (mut reducer, _) =
            ProjectionFacts::load_bound(self.profile.clone(), query.job, source_job, channel)?;
        let graph = IndexedGraph {
            repository,
            conversation: channel,
        };
        let own = me.user_id();
        let mut after = -1;
        loop {
            let page = repository.ancestor_headers_page(&selection, after, 64)?;
            for (rank, header) in page.items {
                let allowed = if super::super::projection_facts::is_membership_control(header.kind)
                {
                    let event = repository
                        .get(&header.id)?
                        .ok_or_else(|| invalid("causal channel control disappeared"))?;
                    reducer.step(&event, &graph)?
                } else {
                    reducer.step_header(&header)?
                };
                let facts = reducer
                    .history
                    .as_ref()
                    .ok_or_else(|| invalid("missing causal channel fact owner"))?
                    .clone();
                self.profile.database().transaction(|c| {
                    self.check_account_on(c)?;
                    replay::require_source_on(c, channel, source_job, revision)?;
                    facts.checkpoint_on(c, &mut reducer, &header, rank, allowed, &own)?;
                    Ok(())
                })?;
            }
            let Some(next) = page.next else {
                break;
            };
            after = next;
        }
        let state = reducer.prior;
        let bytes = state
            .as_ref()
            .map(ChannelState::encode_current)
            .transpose()
            .map_err(|error| invalid(&error.to_string()))?;
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c, channel, source_job, revision)?;
            c.execute(
                "INSERT INTO channel_causal_states VALUES(?1,?2,?3,?4)",
                params![
                    source_job.as_slice(),
                    channel.as_bytes().as_slice(),
                    event.as_bytes().as_slice(),
                    bytes
                ],
            )?;
            c.execute(
                "UPDATE channel_projection_queries SET retired=1 WHERE job=?1",
                [query.job.as_slice()],
            )?;
            Ok(())
        })?;
        Ok(state)
    }
}

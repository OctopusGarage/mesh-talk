//! On-demand history authority over the channel's indexed canonical and causal
//! states. Only selected controls are decoded; unchanged policy is scalar state.
use super::*;
use crate::{
    eventlog::{
        LogError,
        repository::{EventHeader, EventRepository},
    },
    node::channel::checkpoint::{ChannelCheckpointStore, ChannelSource},
};
use rusqlite::{Connection, OptionalExtension, params};

mod authority;
mod continuation;
mod operations;
mod selection;
mod work;
pub(in crate::node) use work::HistoryWork;
mod preparation;
pub(in crate::node) use preparation::{OwnerAuthorizationQuery, OwnerAuthorizationStep};
mod controls;
mod receive;
mod send;
pub(super) use controls::install_on;

const SCHEMA: &str = "CREATE TABLE channel_history_policy_cache(job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),control_stamp INTEGER NOT NULL,policy BLOB NOT NULL,PRIMARY KEY(job,channel)) WITHOUT ROWID;";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    controls::initialize(c, fresh)?;
    send::initialize(c, fresh)
}
pub(in crate::node) struct IndexedView<'a> {
    checkpoints: &'a ChannelCheckpointStore,
    repository: &'a EventRepository,
    me: &'a DeviceIdentity,
    source: ChannelSource,
}
impl<'a> IndexedView<'a> {
    pub(in crate::node) fn new(
        checkpoints: &'a ChannelCheckpointStore,
        repository: &'a EventRepository,
        me: &'a DeviceIdentity,
        source: ChannelSource,
    ) -> Self {
        Self {
            checkpoints,
            repository,
            me,
            source,
        }
    }
}

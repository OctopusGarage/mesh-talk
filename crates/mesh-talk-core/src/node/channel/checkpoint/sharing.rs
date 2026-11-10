//! Current receiver snapshots and exact admitted shipment coverage. Historical
//! epoch keys and ReceiveKeys controls remain on disk, never in ChannelBook.
use super::*;
use crate::identity::device::PublicIdentity;
use crate::node::{
    channel_account_authority::AccountGrantId, channel_account_bootstrap::DeviceControl,
};
use std::collections::BTreeMap;

const SCHEMA: &str = "CREATE TABLE channel_receive_shipments(job BLOB NOT NULL,channel BLOB NOT NULL,recipient_ed BLOB NOT NULL,recipient_x BLOB NOT NULL,grant_account BLOB NOT NULL,grant_incarnation BLOB NOT NULL,epoch BLOB NOT NULL,author TEXT NOT NULL,position INTEGER NOT NULL CHECK(position>=0 AND position<=4294967295),PRIMARY KEY(job,channel,recipient_ed,recipient_x,grant_account,grant_incarnation,epoch,author)) WITHOUT ROWID;";
impl ChannelCheckpointStore {
    pub(in crate::node) fn replica_enabled_at(
        &self,
        source: &ChannelSource,
        member: &str,
    ) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            Ok(c.query_row("SELECT enabled FROM channel_replica_preferences WHERE job=?1 AND channel=?2 AND member=?3",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),member],|r|r.get(0)).optional()?.unwrap_or(true))
        })
    }
}
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
pub(super) fn install_on(
    c: &Connection,
    job: [u8; 16],
    event: &Event,
    allowed: bool,
    own: &str,
) -> Result<(), LogError> {
    if !allowed
        || event.kind != crate::eventlog::EventKind::ChannelDeviceInheritance
        || event.author.user_id() != own
    {
        return Ok(());
    }
    let Some(DeviceControl::ReceiveKeys {
        grant,
        recipient,
        epoch,
        authors,
        positions,
        ..
    }) = DeviceControl::decode(&event.ciphertext)
    else {
        return Ok(());
    };
    for (author, position) in authors.into_iter().zip(positions) {
        c.execute("INSERT INTO channel_receive_shipments VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(job,channel,recipient_ed,recipient_x,grant_account,grant_incarnation,epoch,author) DO UPDATE SET position=MAX(position,excluded.position)",params![job.as_slice(),event.conversation_id.as_bytes().as_slice(),recipient.ed25519_pub.as_slice(),recipient.x25519_pub.as_slice(),grant.account_pub.as_slice(),grant.incarnation.as_slice(),epoch.to_be_bytes().as_slice(),author.user_id(),i64::from(position)])?;
    }
    Ok(())
}
impl ChannelCheckpointStore {
    pub(in crate::node) fn receiver_distribution(
        &self,
        source: &ChannelSource,
        author: &str,
        epoch: u64,
    ) -> Result<Option<SenderKeyDistribution>, LogError> {
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            let bytes:Option<Vec<u8>>=c.query_row("SELECT checkpoint FROM channel_receivers WHERE channel=?1 AND author=?2 AND epoch=?3",params![source.channel.as_bytes().as_slice(),author,epoch.to_be_bytes().as_slice()],|r|r.get(0)).optional()?;
            bytes.map(|bytes|ChannelState::checkpoint_distribution(&Zeroizing::new(bytes)).map_err(|error|invalid(&error.to_string()))).transpose()
        })
    }
    pub(in crate::node) fn shipped_distributions(
        &self,
        source: &ChannelSource,
        state: &ChannelState,
        grant: AccountGrantId,
        recipient: &PublicIdentity,
    ) -> Result<BTreeMap<String, u32>, LogError> {
        self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            let mut shipped=BTreeMap::new();
            for member in state.members().iter().filter(|member|state.is_accepted_member(&member.user_id())) {
                let author=member.user_id();
                let position:Option<u32>=c.query_row("SELECT position FROM channel_receive_shipments WHERE job=?1 AND channel=?2 AND recipient_ed=?3 AND recipient_x=?4 AND grant_account=?5 AND grant_incarnation=?6 AND epoch=?7 AND author=?8",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),recipient.ed25519_pub.as_slice(),recipient.x25519_pub.as_slice(),grant.account_pub.as_slice(),grant.incarnation.as_slice(),state.epoch().to_be_bytes().as_slice(),&author],|r|r.get(0)).optional()?;
                if let Some(position)=position {shipped.insert(author,position);}
            }
            Ok(shipped)
        })
    }
}

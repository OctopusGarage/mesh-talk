//! Indexed presentation history. Only a selected page is materialized, while
//! ChannelBook retains current metadata rather than every past activity.
use super::*;
use crate::{
    eventlog::{event::EventKind, repository::EventHeader},
    node::channel::{ChannelActivity, projection::MembershipReducer},
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChannelActivityCursor {
    channel: ConversationId,
    account: [u8; 32],
    job: [u8; 16],
    revision: i64,
    before: i64,
}
#[derive(Clone, Copy, Debug)]
pub struct ChannelActivityPageLimit {
    pub rows: usize,
    pub bytes: usize,
}
impl Default for ChannelActivityPageLimit {
    fn default() -> Self {
        Self {
            rows: 20,
            bytes: 64 * 1024,
        }
    }
}
pub struct ChannelActivityPage {
    pub items: Vec<ChannelActivity>,
    pub next: Option<ChannelActivityCursor>,
}
const SCHEMA:&str="CREATE TABLE channel_activities(
 job BLOB NOT NULL,channel BLOB NOT NULL,event BLOB NOT NULL,rank INTEGER NOT NULL,
 kind TEXT NOT NULL,actor TEXT NOT NULL,target TEXT,wall_clock BLOB NOT NULL CHECK(length(wall_clock)=8),
 PRIMARY KEY(job,channel,event),UNIQUE(job,channel,rank)) WITHOUT ROWID;
CREATE INDEX channel_activity_actor ON channel_activities(job,channel,actor,kind);";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
pub(super) struct Before {
    known: bool,
    owner: Option<String>,
    members: Vec<String>,
    accepted: bool,
    upgraded: bool,
    dissolved: bool,
}
impl Before {
    pub(super) fn capture(reducer: &MembershipReducer, header: &EventHeader) -> Self {
        Self {
            known: reducer.known_local,
            owner: reducer.prior.as_ref().map(|s| s.owner().to_owned()),
            members: reducer
                .prior
                .as_ref()
                .map(|s| s.members().iter().map(|m| m.user_id()).collect())
                .unwrap_or_default(),
            accepted: reducer
                .prior
                .as_ref()
                .is_some_and(|s| s.is_accepted_member(&header.author.user_id())),
            upgraded: reducer
                .prior
                .as_ref()
                .is_some_and(|s| s.owner_account_id().is_some()),
            dissolved: reducer
                .prior
                .as_ref()
                .is_some_and(ChannelState::is_dissolved),
        }
    }
    pub(super) fn finish(
        self,
        reducer: &MembershipReducer,
        header: &EventHeader,
        event: Option<&Event>,
        own: &str,
    ) -> Option<ChannelActivity> {
        if self.upgraded
            || self.dissolved
            || matches!(
                header.kind,
                EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
            )
        {
            return None;
        }
        let state = reducer.prior.as_ref()?;
        let actor = header.author.user_id();
        let (kind, target) = match header.kind {
            EventKind::MembershipChange if !self.known && state.is_member(own) => {
                if state.epoch() == 0 {
                    ("created", None)
                } else {
                    ("joined", Some(own.to_owned()))
                }
            }
            EventKind::MembershipChange if self.known && self.owner.as_deref() == Some(&actor) => {
                let added = state
                    .members()
                    .iter()
                    .map(|p| p.user_id())
                    .find(|id| !self.members.contains(id));
                if let Some(id) = added {
                    ("joined", Some(id))
                } else {
                    let id = self.members.iter().find(|id| !state.is_member(id))?;
                    ("removed", Some(id.clone()))
                }
            }
            EventKind::ChannelRename
                if self.known
                    && self.owner.as_deref() == Some(&actor)
                    && event.is_some_and(|e| std::str::from_utf8(&e.ciphertext).is_ok()) =>
            {
                ("renamed", None)
            }
            EventKind::ChannelOwnerTransfer
                if self.known
                    && self.owner.as_deref() == Some(&actor)
                    && state.owner() != actor =>
            {
                ("transferred", Some(state.owner().to_owned()))
            }
            EventKind::ChannelLeave
                if self.known && self.members.contains(&actor) && !state.is_member(&actor) =>
            {
                ("left", Some(actor.clone()))
            }
            EventKind::ChannelAccept
                if self.known && !self.accepted && state.is_accepted_member(&actor) =>
            {
                ("accepted", Some(actor.clone()))
            }
            _ => return None,
        };
        Some(ChannelActivity {
            kind,
            actor,
            target,
            wall_clock: header.wall_clock,
        })
    }
}
pub(super) fn install_on(
    c: &Connection,
    job: [u8; 16],
    channel: ConversationId,
    event: EventId,
    rank: i64,
    activity: Option<&ChannelActivity>,
) -> Result<(), LogError> {
    if let Some(a) = activity {
        c.execute(
            "INSERT INTO channel_activities VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                job.as_slice(),
                channel.as_bytes().as_slice(),
                event.as_bytes().as_slice(),
                rank,
                a.kind,
                &a.actor,
                &a.target,
                a.wall_clock.to_be_bytes().as_slice()
            ],
        )?;
    }
    Ok(())
}
impl ChannelCheckpointStore {
    /// Seek newest first in SQL, return the selected page chronologically. This
    /// matches the existing latest-20 UI without allocating older activities.
    pub(in crate::node) fn activity_page(
        &self,
        channel: ConversationId,
        cursor: Option<&ChannelActivityCursor>,
        limit: ChannelActivityPageLimit,
    ) -> Result<ChannelActivityPage, LogError> {
        if !(1..=128).contains(&limit.rows) || limit.bytes == 0 || limit.bytes > 8 * 1024 * 1024 {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let row:Option<(Vec<u8>,i64)>=c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((job,revision))=row else{if cursor.is_some(){return Err(LogError::StaleCursor);}return Ok(ChannelActivityPage{items:Vec::new(),next:None});};
            let job:[u8;16]=job.try_into().map_err(|_|invalid("invalid activity job"))?;
            replay::require_source_on(c,channel,job,revision)?;
            if cursor.is_some_and(|p|p.channel!=channel||p.account!=self.account||p.job!=job||p.revision!=revision||p.before<0){return Err(LogError::StaleCursor);}
            let mut statement=c.prepare("SELECT rank,kind,actor,target,wall_clock FROM channel_activities WHERE job=?1 AND channel=?2 AND (?3 IS NULL OR rank<?3) ORDER BY rank DESC LIMIT ?4")?;
            let mut rows=statement.query(params![job.as_slice(),channel.as_bytes().as_slice(),cursor.map(|p|p.before),limit.rows as i64+1])?;
            let mut items=Vec::new();let mut bytes=0usize;let mut last=None;let mut more=false;
            while let Some(row)=rows.next()? {
                let kind:String=row.get(1)?;let actor:String=row.get(2)?;let target:Option<String>=row.get(3)?;
                let charge=128+kind.len()+actor.len()+target.as_ref().map_or(0,String::len);
                if items.len()==limit.rows||bytes+charge>limit.bytes {if items.is_empty(){return Err(LogError::PageBudgetTooSmall{required:charge});}more=true;break;}
                let kind=match kind.as_str(){"created"=>"created","joined"=>"joined","removed"=>"removed","renamed"=>"renamed","transferred"=>"transferred","left"=>"left","accepted"=>"accepted",_=>return Err(invalid("invalid channel activity"))};
                let wall:Vec<u8>=row.get(4)?;let wall_clock=u64::from_be_bytes(wall.try_into().map_err(|_|invalid("invalid activity clock"))?);
                items.push(ChannelActivity{kind,actor,target,wall_clock});bytes+=charge;last=Some(row.get(0)?);
            }
            items.reverse();
            let next=if more {Some(ChannelActivityCursor{channel,account:self.account,job,revision,before:last.ok_or_else(||invalid("activity page missing cursor"))?})}else{None};
            Ok(ChannelActivityPage{items,next})
        })
    }
}

impl crate::node::Node {
    pub fn channel_activity_page(
        &self,
        channel: ConversationId,
        cursor: Option<&ChannelActivityCursor>,
        limit: ChannelActivityPageLimit,
    ) -> Result<ChannelActivityPage, crate::node::NodeError> {
        self.account_group_projection(channel)?;
        Ok(self
            .channel_checkpoints
            .activity_page(channel, cursor, limit)?)
    }
}

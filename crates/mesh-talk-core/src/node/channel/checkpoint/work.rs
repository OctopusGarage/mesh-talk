//! Durable channel demand for the existing recovery loop. Each current channel
//! has one queue identity; no signed bodies or receiver material are retained.
use super::*;
const SCHEMA: &str = "
CREATE TABLE channel_runtime_work(channel BLOB PRIMARY KEY CHECK(length(channel)=32),opened INTEGER NOT NULL,reopening INTEGER NOT NULL CHECK(reopening IN(0,1))) WITHOUT ROWID;
CREATE TABLE channel_runtime_owner(singleton INTEGER PRIMARY KEY CHECK(singleton=1),account BLOB NOT NULL CHECK(length(account)=32),generation INTEGER NOT NULL,opened INTEGER NOT NULL,through_commit INTEGER NOT NULL,after_channel BLOB,complete INTEGER NOT NULL CHECK(complete IN(0,1)));
";
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_runtime_work')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
pub(super) fn queue_on(c: &Connection, channel: ConversationId) -> Result<(), LogError> {
    c.execute("INSERT OR IGNORE INTO channel_runtime_work VALUES(?1,COALESCE((SELECT opened FROM channel_runtime_owner WHERE singleton=1),0),0)",[channel.as_bytes().as_slice()])?;
    Ok(())
}
pub(super) fn queue_discovered_on(
    c: &Connection,
    channel: ConversationId,
    ordinal: i64,
) -> Result<(), LogError> {
    queue_on(c, channel)?;
    c.execute("UPDATE channel_runtime_work SET reopening=1 WHERE channel=?1 AND ?2<=COALESCE((SELECT through_commit FROM channel_runtime_owner WHERE singleton=1),-1)",params![channel.as_bytes().as_slice(),ordinal])?;
    Ok(())
}
impl ChannelCheckpointStore {
    /// Called once after adoption publication. A scalar epoch marks existing
    /// work as reopening without rewriting the queue. Source discovery compares
    /// its exact committed ordinals with the captured pre-open cutoff.
    pub(in crate::node) fn begin_runtime_reopen(&self) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let epoch:i64=c.query_row("SELECT COALESCE((SELECT opened FROM channel_runtime_owner WHERE singleton=1),0)",[],|r|r.get(0))?;
            let epoch=epoch.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
            let through:i64=c.query_row("SELECT next_commit-1 FROM event_domains WHERE domain=?1",[b"MeshPartEventsV1".as_slice()],|r|r.get(0))?;
            c.execute("INSERT INTO channel_runtime_owner VALUES(1,?1,?2,?3,?4,NULL,0) ON CONFLICT(singleton) DO UPDATE SET account=excluded.account,generation=excluded.generation,opened=excluded.opened,through_commit=excluded.through_commit,after_channel=NULL,complete=0",params![self.account.as_slice(),self.generation,epoch,through])?;
            Ok(())
        })
    }
    /// One fixed page of current channel identities after reopen/adoption. The
    /// old authority owner is queued for replay; its display grants no access.
    pub(in crate::node) fn resume_current_channels_step(&self) -> Result<bool, LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let owner:Option<(Option<Vec<u8>>,bool)>=c.query_row("SELECT after_channel,complete FROM channel_runtime_owner WHERE singleton=1 AND account=?1 AND generation=?2",params![self.account.as_slice(),self.generation],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((after,false))=owner else{return Ok(false);};
            let mut statement=c.prepare("SELECT channel,owner_account FROM channel_current WHERE (?1 IS NULL OR channel>?1) ORDER BY channel LIMIT 32")?;
            let rows=statement.query_map([after],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
            for (id,owner) in &rows {
                c.execute("INSERT OR IGNORE INTO channel_runtime_work SELECT ?1,opened,1 FROM channel_runtime_owner WHERE singleton=1",[id])?;
                if owner.as_slice()!=self.account {
                    c.execute("INSERT OR IGNORE INTO channel_dirty VALUES(?1)",[id])?;
                }
            }
            if let Some((id,_))=rows.last() {c.execute("UPDATE channel_runtime_owner SET after_channel=?1 WHERE singleton=1",[id])?;}
            else {c.execute("UPDATE channel_runtime_owner SET complete=1 WHERE singleton=1",[])?;}
            Ok(!rows.is_empty())
        })
    }
    pub(in crate::node) fn queue_runtime(&self, channel: ConversationId) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            queue_on(c, channel)
        })
    }
    pub(in crate::node) fn finish_runtime(
        &self,
        channel: ConversationId,
        source: Option<&ChannelSource>,
    ) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            if let Some(source) = source {
                self.check_source_on(c, source)?;
            }
            c.execute(
                "DELETE FROM channel_runtime_work WHERE channel=?1",
                [channel.as_bytes().as_slice()],
            )?;
            Ok(())
        })
    }
    pub(in crate::node) fn next_runtime(
        &self,
        after: Option<ConversationId>,
    ) -> Result<Option<(ConversationId, bool)>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let select=|after:Option<ConversationId>|->Result<Option<(Vec<u8>,bool)>,LogError> {
                c.query_row("SELECT channel,reopening OR opened<COALESCE((SELECT opened FROM channel_runtime_owner WHERE singleton=1),0) FROM channel_runtime_work WHERE (?1 IS NULL OR channel>?1) ORDER BY channel LIMIT 1",[after.as_ref().map(|id|id.as_bytes().as_slice())],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into)
            };
            let row=match select(after)? {Some(row)=>Some(row),None if after.is_some()=>select(None)?,None=>None};
            row.map(|(id,reopening)|Ok((ConversationId::new(id.try_into().map_err(|_|invalid("invalid runtime channel"))?),reopening))).transpose()
        })
    }
}

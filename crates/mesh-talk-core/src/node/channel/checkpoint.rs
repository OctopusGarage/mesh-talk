//! Durable channel projection and consuming receiver owner. Authority replay is
//! separately versioned from receiver progress: reordering controls never rolls
//! a consumed key back. All records use the already leased profile database.
use crate::storage::profile::participant::event_ownership::{
    ParticipantEventOwner, capture_event_owner_on, check_event_owner_on,
};
use crate::{
    channel::{ChannelState, model::PreparedChannelMessage, sender_key::SenderKeyDistribution},
    eventlog::{
        LogError,
        event::{ConversationId, Event, EventId},
    },
    storage::profile::context::ProfileStorage,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;
use zeroize::Zeroizing;

pub(in crate::node) mod activity;
pub(in crate::node) mod causal;
mod cleanup;
mod current;
mod file_preview;
pub(in crate::node) use current::ChannelSource;
pub(in crate::node) use file_preview::FileReceivePreview;
mod pending;
pub(in crate::node) use pending::ChannelReceiveBudget;
mod recall;
mod reception;
pub(in crate::node) use reception::ChannelReceptionRole;
mod replay;
mod sharing;
#[cfg(test)]
mod tests;

const SCHEMA: &str = "
CREATE TABLE channel_current(
 channel BLOB PRIMARY KEY CHECK(length(channel)=32), state BLOB NOT NULL,
 source_job BLOB NOT NULL CHECK(length(source_job)=16), source_revision INTEGER NOT NULL,
 next_rank INTEGER NOT NULL CHECK(next_rank>=0), owner_account BLOB NOT NULL CHECK(length(owner_account)=32),known_local INTEGER NOT NULL CHECK(known_local IN (0,1))
) WITHOUT ROWID;
CREATE TABLE channel_receivers(
 channel BLOB NOT NULL CHECK(length(channel)=32), author TEXT NOT NULL,
 epoch BLOB NOT NULL CHECK(length(epoch)=8), revision INTEGER NOT NULL CHECK(revision>=0),
 checkpoint BLOB NOT NULL,key_revision INTEGER NOT NULL CHECK(key_revision>=0),next_n INTEGER NOT NULL CHECK(next_n>=0 AND next_n<=4294967295), PRIMARY KEY(channel,author,epoch)
) WITHOUT ROWID;
CREATE TABLE channel_processed(
 channel BLOB NOT NULL CHECK(length(channel)=32), event BLOB NOT NULL CHECK(length(event)=32),
 disposition INTEGER NOT NULL, PRIMARY KEY(channel,event)
) WITHOUT ROWID;
CREATE TABLE channel_pending(
 channel BLOB NOT NULL CHECK(length(channel)=32), event BLOB NOT NULL CHECK(length(event)=32),
 ordinal INTEGER NOT NULL, reason INTEGER NOT NULL,author TEXT NOT NULL,kind INTEGER NOT NULL,admitted INTEGER NOT NULL,
 epoch BLOB CHECK(epoch IS NULL OR length(epoch)=8),attempt_job BLOB,attempt_revision INTEGER NOT NULL DEFAULT -1,wake_at INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(channel,event)
) WITHOUT ROWID;
CREATE INDEX channel_pending_new ON channel_pending(channel,kind,ordinal,event) WHERE reason=0 AND admitted=1;
CREATE INDEX channel_pending_retry ON channel_pending(channel,author,epoch,kind,attempt_revision,ordinal,event) WHERE reason=1 AND admitted=1;
CREATE INDEX channel_pending_gap ON channel_pending(channel,author,epoch,kind,wake_at,ordinal,event) WHERE reason=3 AND admitted=1;
CREATE TABLE channel_pending_schedule(
 channel BLOB NOT NULL,kind INTEGER NOT NULL,prefer_retry INTEGER NOT NULL CHECK(prefer_retry IN(0,1)),
 after_author TEXT,after_epoch BLOB,PRIMARY KEY(channel,kind)
) WITHOUT ROWID;
CREATE TABLE channel_retry_chains(
 channel BLOB NOT NULL,author TEXT NOT NULL,epoch BLOB NOT NULL,kind INTEGER NOT NULL,revision INTEGER NOT NULL,next_n INTEGER NOT NULL,
 PRIMARY KEY(channel,kind,author,epoch)
) WITHOUT ROWID;
";

pub(in crate::node) struct ChannelCheckpointStore {
    profile: Arc<ProfileStorage>,
    account: [u8; 32],
    generation: i64,
    owner: Option<ParticipantEventOwner>,
    causal_query: std::sync::Mutex<()>,
}
pub(in crate::node) struct PreparedChannelReceive {
    channel: ConversationId,
    event: EventId,
    wall_clock: u64,
    kind: crate::eventlog::event::EventKind,
    revision: i64,
    key_revision: i64,
    source_job: [u8; 16],
    source_revision: i64,
    account: [u8; 32],
    owner: ParticipantEventOwner,
    candidate: PreparedChannelMessage,
}
impl PreparedChannelReceive {
    pub(in crate::node) fn plaintext(&self) -> &[u8] {
        &self.candidate.plaintext
    }
    pub(in crate::node) fn event_id(&self) -> EventId {
        self.event
    }
    pub(in crate::node) fn author(&self) -> &str {
        &self.candidate.author
    }
    pub(in crate::node) fn wall_clock(&self) -> u64 {
        self.wall_clock
    }
    pub(in crate::node) fn conversation(&self) -> ConversationId {
        self.channel
    }
}
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}

impl ChannelCheckpointStore {
    pub(in crate::node) fn open(profile: Arc<ProfileStorage>) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_current')",
                [],
                |row| row.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            super::projection_facts::ProjectionFacts::initialize(c, !present)?;
            replay::initialize(c, !present)?;
            reception::initialize(c, !present)?;
            recall::initialize(c, !present)?;
            sharing::initialize(c, !present)?;
            activity::initialize(c, !present)?;
            causal::initialize(c, !present)?;
            file_preview::initialize(c)?;
            super::graph::preparing::initialize(c)?;
            crate::node::join_history::initialize_indexed(c, !present)
        })?;
        let (account, generation, owner) = profile.database().read(|c| {
            let account = active_account_on(c)?;
            let generation: i64 = c.query_row(
                "SELECT generation FROM participant_event_ownership WHERE singleton=1",
                [],
                |row| row.get(0),
            )?;
            if generation < 0 {
                return Err(invalid("invalid channel account generation"));
            }
            let loaded = crate::identity::account::AccountPublic {
                ed25519_pub: account,
            };
            let owner = match capture_event_owner_on(c, &loaded) {
                Ok(owner) => Some(owner),
                // Constructor migration can project signed authority before
                // adoption finishes; it cannot prepare a consuming receiver.
                Err(LogError::StaleCursor) => None,
                Err(error) => return Err(error),
            };
            Ok((account, generation, owner))
        })?;
        Ok(Self {
            profile,
            account,
            generation,
            owner,
            causal_query: std::sync::Mutex::new(()),
        })
    }
    fn check_account_on(&self, c: &Connection) -> Result<(), LogError> {
        if let Some(owner) = &self.owner {
            return check_event_owner_on(c, owner);
        }
        // A bootstrap owner is valid only while its exact pending adoption
        // remains open. Final publication requires a new owner capture.
        let current: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM participant_profile p JOIN participant_event_ownership o ON o.singleton=p.singleton WHERE p.singleton=1 AND COALESCE(p.next_account,p.account)=?1 AND o.generation=?2 AND (p.next_account IS NOT NULL OR p.bootstrap_previous IS NOT NULL))",
            params![self.account.as_slice(), self.generation], |row| row.get(0),
        )?;
        if !current {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }

    /// Callers install RAM counters only after this single transaction publishes
    /// the consuming checkpoint, exact plaintext identity and reception fact.
    #[cfg(test)]
    pub(in crate::node) fn commit_message(
        &self,
        candidate: &PreparedChannelReceive,
        received: &mut crate::node::received_store::ReceivedStore,
        entry: &crate::node::received_log::ReceivedEntry,
        reception: Option<&crate::node::reception_effects::ReceptionMetadata>,
    ) -> Result<bool, LogError> {
        received.check_write()?;
        if candidate.kind != crate::eventlog::event::EventKind::Message
            || entry.conversation != candidate.channel
            || entry.event_id != candidate.event
            || entry.wall_clock != candidate.wall_clock
            || entry.plaintext.as_slice() != candidate.plaintext()
            || entry.from != candidate.candidate.author
        {
            return Err(invalid(
                "channel transcript does not match prepared receiver",
            ));
        }
        let (consumed, inserted) = self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            if !Self::install_receive_on(c, candidate)? {
                return Ok((false, false));
            }
            let inserted = crate::node::received_store::ReceivedStore::install_channel_on(
                c, entry, reception,
            )?;
            Ok((true, inserted))
        })?;
        received.note_channel_commit(usize::from(inserted));
        Ok(consumed)
    }

    pub(in crate::node) fn profile(&self) -> &Arc<ProfileStorage> {
        &self.profile
    }

    /// A current projection page holds no historical keys or message identities.
    pub(in crate::node) fn current_page(
        &self,
        after: Option<ConversationId>,
    ) -> Result<Vec<ChannelState>, LogError> {
        self.metadata_page(after, true)
    }

    pub(in crate::node) fn metadata_page(
        &self,
        after: Option<ConversationId>,
        known_local: bool,
    ) -> Result<Vec<ChannelState>, LogError> {
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            let mut statement = c.prepare("SELECT s.channel,s.state FROM channel_current s JOIN event_conversations e ON e.domain=?3 AND e.conversation=s.channel AND e.lifecycle=0 WHERE s.owner_account=?2 AND s.known_local=?4 AND (?1 IS NULL OR s.channel>?1) ORDER BY s.channel LIMIT 32")?;
            let mut rows = statement.query(params![after.as_ref().map(|id|id.as_bytes().as_slice()),self.account.as_slice(),b"MeshPartEventsV1".as_slice(),known_local])?;
            let mut result = Vec::new();
            while let Some(row)=rows.next()? {
                let id: Vec<u8> = row.get(0)?;
                let bytes: Vec<u8> = row.get(1)?;
                let state = ChannelState::decode_current(&bytes).map_err(|e|invalid(&e.to_string()))?;
                if state.id().as_bytes().as_slice()!=id { return Err(invalid("channel checkpoint identity mismatch")); }
                result.push(state);
            }
            Ok(result)
        })
    }

    pub(in crate::node) fn install_current_on(
        c: &Connection,
        state: &ChannelState,
        job: [u8; 16],
        revision: i64,
        next_rank: i64,
        account: &[u8; 32],
        known_local: bool,
    ) -> Result<(), LogError> {
        let bytes = state
            .encode_current()
            .map_err(|e| invalid(&e.to_string()))?;
        c.execute("INSERT INTO channel_current VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(channel) DO UPDATE SET state=excluded.state,source_job=excluded.source_job,source_revision=excluded.source_revision,next_rank=excluded.next_rank,owner_account=excluded.owner_account,known_local=excluded.known_local",
            params![state.id().as_bytes().as_slice(),bytes,job.as_slice(),revision,next_rank,account.as_slice(),known_local])?;
        Ok(())
    }

    /// Ordinary key rotations are first-wins. Replaying an old distribution
    /// cannot replace a checkpoint containing consumed holes and skipped keys.
    pub(in crate::node) fn install_distribution_on(
        c: &Connection,
        channel: ConversationId,
        author: &str,
        epoch: u64,
        distribution: &SenderKeyDistribution,
    ) -> Result<(), LogError> {
        let bytes = ChannelState::initial_receiver_checkpoint(distribution)
            .map_err(|e| invalid(&e.to_string()))?;
        let inserted=c.execute("INSERT INTO channel_receivers VALUES(?1,?2,?3,0,?4,0,?5) ON CONFLICT(channel,author,epoch) DO NOTHING",
            params![channel.as_bytes().as_slice(),author,epoch.to_be_bytes().as_slice(),bytes.as_slice(),i64::from(distribution.n)])?;
        if inserted != 0 {
            Self::wake_chain_on(c, channel, author, epoch, 0, distribution.n)?;
        }
        Ok(())
    }

    pub(in crate::node) fn processed_on(
        c: &Connection,
        channel: ConversationId,
        event: EventId,
    ) -> Result<bool, LogError> {
        c.query_row(
            "SELECT EXISTS(SELECT 1 FROM channel_processed WHERE channel=?1 AND event=?2)",
            params![channel.as_bytes().as_slice(), event.as_bytes().as_slice()],
            |row| row.get(0),
        )
        .map_err(Into::into)
    }

    pub(in crate::node) fn prepare_receive(
        &self,
        event: &Event,
    ) -> Result<Option<PreparedChannelReceive>, LogError> {
        self.prepare_receive_with_prior(event, None)
    }

    /// The second preview uses its predecessor's private chain without publishing
    /// either key. Its CAS requires the first candidate to commit before it.
    pub(in crate::node) fn prepare_receive_following(
        &self,
        event: &Event,
        preceding: &PreparedChannelReceive,
    ) -> Result<Option<PreparedChannelReceive>, LogError> {
        self.prepare_receive_with_prior(event, Some(preceding))
    }

    fn prepare_receive_with_prior(
        &self,
        event: &Event,
        preceding: Option<&PreparedChannelReceive>,
    ) -> Result<Option<PreparedChannelReceive>, LogError> {
        self.prepare_receive_with_checkpoint(
            event,
            preceding.map(|prior| (prior, &prior.candidate)),
        )
    }

    fn prepare_receive_with_checkpoint(
        &self,
        event: &Event,
        preceding: Option<(&PreparedChannelReceive, &PreparedChannelMessage)>,
    ) -> Result<Option<PreparedChannelReceive>, LogError> {
        if !event.is_canonical() || !event.verify_integrity() || !event.verify_signature() {
            return Err(invalid("unauthenticated channel receiver source"));
        }
        let owner = self.owner.as_ref().ok_or(LogError::StaleCursor)?;
        let mut attempt = None;
        let prepared = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            if !matches!(event.kind, crate::eventlog::event::EventKind::Message | crate::eventlog::event::EventKind::FileManifest) { return Ok(None); }
            if Self::processed_on(c,event.conversation_id,event.id)? { return Ok(None); }
            let authority: Option<(Vec<u8>,i64)> = c.query_row("SELECT p.source_job,p.source_revision FROM channel_current p JOIN channel_event_authority a ON a.job=p.source_job AND a.channel=p.channel AND a.event=?2 WHERE p.channel=?1 AND p.owner_account=?3 AND a.receive=1",
                params![event.conversation_id.as_bytes().as_slice(),event.id.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((job,source_revision)) = authority else { return Ok(None); };
            let source_job: [u8;16]=job.try_into().map_err(|_|invalid("invalid channel authority generation"))?;
            replay::require_source_on(c,event.conversation_id,source_job,source_revision)?;
            let author=event.author.user_id();
            attempt=Some((None,-1,source_job,source_revision,None));
            let Some(epoch)=ChannelState::sender_message_epoch(&author,&event.ciphertext) else { return Ok(None); };
            attempt=Some((Some(epoch),-1,source_job,source_revision,None));
            let row: Option<(i64,i64,Vec<u8>)>=c.query_row("SELECT revision,key_revision,checkpoint FROM channel_receivers WHERE channel=?1 AND author=?2 AND epoch=?3",
                params![event.conversation_id.as_bytes().as_slice(),&author,epoch.to_be_bytes().as_slice()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
            let Some((revision,key_revision,bytes))=row else { return Ok(None); };
            attempt=Some((Some(epoch),key_revision,source_job,source_revision,None));
            let bytes=Zeroizing::new(bytes);
            let (revision,checkpoint) = if let Some((prior,private)) = preceding.filter(|(prior,_)|
                prior.channel==event.conversation_id && prior.candidate.author==author && prior.candidate.epoch==epoch
            ) {
                if prior.event==event.id || prior.account!=self.account || &prior.owner!=owner || prior.source_job!=source_job
                    || prior.source_revision!=source_revision || prior.revision!=revision {
                    return Err(LogError::StaleCursor);
                }
                (revision.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?,private.checkpoint.as_slice())
            } else { (revision,bytes.as_slice()) };
            let candidate=match ChannelState::prepare_checkpoint_message(&author,checkpoint,&event.ciphertext) {
                Ok(candidate)=>candidate,
                Err(crate::channel::model::CheckpointMessageError::Rejected)=>return Ok(None),
                Err(crate::channel::model::CheckpointMessageError::Gap{after_position})=>{
                    attempt=Some((Some(epoch),key_revision,source_job,source_revision,Some(after_position)));
                    return Ok(None);
                },
                Err(crate::channel::model::CheckpointMessageError::Malformed(error))=>return Err(invalid(&error)),
            };
            Ok(Some(PreparedChannelReceive {channel:event.conversation_id,event:event.id,wall_clock:event.wall_clock,kind:event.kind,revision,key_revision,source_job,source_revision,account:self.account,owner:owner.clone(),candidate}))
        })?;
        if prepared.is_none()
            && let Some((epoch, revision, job, source_revision, wait_until)) = attempt
        {
            self.note_attempt(event, epoch, revision, job, source_revision, wait_until)?;
        }
        Ok(prepared)
    }

    /// Must run in the transaction installing the exact transcript and receipt
    /// fact. The compare-and-swap rejects concurrent candidates on one chain.
    /// A repeated source is idempotent; failure leaves the previous chain intact.
    pub(in crate::node) fn install_receive_on(
        c: &Connection,
        prepared: &PreparedChannelReceive,
    ) -> Result<bool, LogError> {
        check_event_owner_on(c, &prepared.owner)?;
        if Self::processed_on(c, prepared.channel, prepared.event)? {
            return Ok(false);
        }
        replay::require_source_on(
            c,
            prepared.channel,
            prepared.source_job,
            prepared.source_revision,
        )?;
        let changed=c.execute("UPDATE channel_receivers SET revision=revision+1,checkpoint=?5,next_n=?6 WHERE channel=?1 AND author=?2 AND epoch=?3 AND revision=?4 AND revision<9223372036854775807",
            params![prepared.channel.as_bytes().as_slice(),&prepared.candidate.author,prepared.candidate.epoch.to_be_bytes().as_slice(),prepared.revision,prepared.candidate.checkpoint.as_slice(),i64::from(prepared.candidate.next_position)])?;
        if changed != 1 {
            return Err(LogError::StaleCursor);
        }
        Self::wake_chain_on(
            c,
            prepared.channel,
            &prepared.candidate.author,
            prepared.candidate.epoch,
            prepared.key_revision,
            prepared.candidate.next_position,
        )?;
        c.execute(
            "INSERT INTO channel_processed VALUES(?1,?2,1)",
            params![
                prepared.channel.as_bytes().as_slice(),
                prepared.event.as_bytes().as_slice()
            ],
        )?;
        c.execute(
            "DELETE FROM channel_pending WHERE channel=?1 AND event=?2",
            params![
                prepared.channel.as_bytes().as_slice(),
                prepared.event.as_bytes().as_slice()
            ],
        )?;
        Ok(true)
    }
}

fn active_account_on(c: &Connection) -> Result<[u8; 32], LogError> {
    let bytes: Vec<u8> = c.query_row(
        "SELECT COALESCE(next_account,account) FROM participant_profile WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    bytes
        .try_into()
        .map_err(|_| invalid("invalid active channel account"))
}

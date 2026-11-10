//! Last completely validated presentation is separate from fresh admission.
//! Startup may display this current metadata while signed control work resumes.
use super::*;
use bincode::Options;
use serde::{Deserialize, Serialize};
mod preparation;
pub(super) use preparation::PolicyCandidate;

const SCHEMA: &str = "
CREATE TABLE channel_book_metadata(
 channel BLOB PRIMARY KEY CHECK(length(channel)=32),owner_account BLOB NOT NULL CHECK(length(owner_account)=32),generation INTEGER NOT NULL,
 job BLOB NOT NULL CHECK(length(job)=16),stamp INTEGER NOT NULL,replica_stamp INTEGER NOT NULL,
 state BLOB NOT NULL,presentation BLOB NOT NULL
) WITHOUT ROWID;
";

#[derive(Serialize, Deserialize)]
pub(super) struct Presentation {
    pub(super) policy: crate::channel::HistoryPolicy,
    pub(super) preferences: Vec<(String, u64, EventId, bool)>,
    pub(super) departed: bool,
    pub(super) transferred: bool,
}
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_book_metadata')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    preparation::initialize(c)
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(value)
        .map_err(|e| invalid(&e.to_string()))
}
fn decode(bytes: &[u8]) -> Result<Presentation, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| invalid(&e.to_string()))
}
impl ChannelCheckpointStore {
    pub(in crate::node) fn prepared_history_policy(
        &self,
        channel: ConversationId,
    ) -> Result<crate::channel::HistoryPolicy, LogError> {
        let source = self.source_binding(channel)?.ok_or(LogError::StaleCursor)?;
        let stamp = self.control_stamp(&source)?;
        self.profile.database().read(|c| {
            self.check_source_on(c, &source)?;
            let bytes: Option<Vec<u8>> = c.query_row("SELECT policy FROM channel_history_policy_cache WHERE job=?1 AND channel=?2 AND control_stamp=?3", params![source.job.as_slice(),channel.as_bytes().as_slice(),stamp], |r| r.get(0)).optional()?;
            bytes.as_deref().and_then(crate::node::join_history::decode_policy).ok_or(LogError::StaleCursor)
        })
    }
    fn presentation_stamps(&self, source: &ChannelSource) -> Result<(i64, i64), LogError> {
        let control = self.control_stamp(source)?;
        let replica=self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            c.query_row("SELECT COALESCE((SELECT rank FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 ORDER BY rank DESC LIMIT 1),-1)",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),crate::eventlog::EventKind::ChannelReplica as i64],|r|r.get(0)).map_err(Into::into)
        })?;
        Ok((control, replica))
    }
    pub(super) fn save_presentation(
        &self,
        source: &ChannelSource,
        state: &ChannelState,
        presentation: &Presentation,
    ) -> Result<(), LogError> {
        let state = state
            .encode_current()
            .map_err(|e| invalid(&e.to_string()))?;
        let encoded = encode(presentation)?;
        let (stamp, replica) = self.presentation_stamps(source)?;
        self.profile.database().transaction(|c| {
            self.check_source_on(c,source)?;
            c.execute("INSERT INTO channel_book_metadata VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(channel) DO UPDATE SET owner_account=excluded.owner_account,generation=excluded.generation,job=excluded.job,stamp=excluded.stamp,replica_stamp=excluded.replica_stamp,state=excluded.state,presentation=excluded.presentation",
                params![source.channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamp,replica,state,encoded])?;
            c.execute("DELETE FROM channel_book_work WHERE channel=?1",[source.channel.as_bytes().as_slice()])?;
            c.execute("INSERT INTO channel_history_policy_cache VALUES(?1,?2,?3,?4) ON CONFLICT(job,channel) DO UPDATE SET control_stamp=excluded.control_stamp,policy=excluded.policy",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),stamp,crate::node::join_history::encode_policy(presentation.policy)])?;
            Ok(())
        })
    }
    /// This is presentation only. Fresh source checks continue to own mutation,
    /// sync and plaintext admission. No current metadata default grants access.
    pub(in crate::node) fn restore_presentation_page(
        &self,
        book: &mut super::super::ChannelBook,
        own: &str,
        after: Option<ConversationId>,
    ) -> Result<Option<ConversationId>, LogError> {
        let row=self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row("SELECT m.channel,m.state,m.presentation FROM channel_book_metadata m JOIN event_conversations e ON e.domain=?1 AND e.conversation=m.channel AND e.lifecycle=0 WHERE m.owner_account=?2 AND m.generation=?3 AND (?4 IS NULL OR m.channel>?4) ORDER BY m.channel LIMIT 1",params![b"MeshPartEventsV1".as_slice(),self.account.as_slice(),self.generation,after.as_ref().map(|id|id.as_bytes().as_slice())],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?))).optional().map_err(Into::into)
        })?;
        let Some((channel, state, presentation)) = row else {
            return Ok(None);
        };
        let channel = ConversationId::new(
            channel
                .try_into()
                .map_err(|_| invalid("invalid presentation channel"))?,
        );
        let state = ChannelState::decode_current(&state).map_err(|e| invalid(&e.to_string()))?;
        if state.id() != channel {
            return Err(invalid("presentation identity mismatch"));
        }
        install(book, own, state, decode(&presentation)?);
        Ok(Some(channel))
    }
    pub(in crate::node) fn install_prepared_presentation(
        &self,
        book: &mut super::super::ChannelBook,
        own: &str,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        let Some(source) = self.source_binding(channel)? else {
            return Ok(false);
        };
        let (stamp, replica) = self.presentation_stamps(&source)?;
        let row=self.profile.database().read(|c| {
            self.check_source_on(c,&source)?;
            c.query_row("SELECT state,presentation FROM channel_book_metadata WHERE channel=?1 AND owner_account=?2 AND generation=?3 AND job=?4 AND stamp=?5 AND replica_stamp=?6",params![channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamp,replica],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?))).optional().map_err(Into::into)
        })?;
        let Some((state, presentation)) = row else {
            return Ok(false);
        };
        let state = ChannelState::decode_current(&state).map_err(|e| invalid(&e.to_string()))?;
        if state.id() != channel {
            return Err(invalid("presentation identity mismatch"));
        }
        install(book, own, state, decode(&presentation)?);
        Ok(true)
    }
}
pub(super) fn install(
    book: &mut super::super::ChannelBook,
    own: &str,
    state: ChannelState,
    presentation: Presentation,
) {
    let channel = state.id();
    book.install_indexed_state(state);
    book.history_policies.insert(channel, presentation.policy);
    book.replica_preferences.retain(|(id, _), _| *id != channel);
    for (member, lamport, event, enabled) in presentation.preferences {
        book.replica_preferences
            .insert((channel, member), (lamport, event, enabled));
    }
    book.departed.retain(|(id, _)| *id != channel);
    book.transferred.retain(|(id, _)| *id != channel);
    if presentation.departed {
        book.departed.insert((channel, own.to_owned()));
    }
    if presentation.transferred {
        book.transferred.insert((channel, own.to_owned()));
    }
}

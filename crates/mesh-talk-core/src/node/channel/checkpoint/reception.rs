//! Channel batch progress is part of the consuming transaction. Independent
//! message/file terminals survive source deletion and a crash before delivery.
use super::*;
use crate::node::{
    received_log::ReceivedEntry,
    received_store::ReceivedStore,
    reception_effects::{self, ArrivalKind, ReceptionKind, ReceptionMetadata, ReceptionSource},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::node) enum ChannelReceptionRole {
    Message,
    File,
}
impl ChannelReceptionRole {
    fn code(self) -> i64 {
        match self {
            Self::Message => 0,
            Self::File => 1,
        }
    }
    fn prefix(self) -> &'static str {
        match self {
            Self::Message => "message",
            Self::File => "file",
        }
    }
    fn matches(self, candidate: &PreparedChannelReceive) -> bool {
        candidate.kind
            == match self {
                Self::Message => crate::eventlog::event::EventKind::Message,
                Self::File => crate::eventlog::event::EventKind::FileManifest,
            }
    }
}
const SCHEMA: &str = "CREATE TABLE channel_reception_batches(
 account BLOB NOT NULL CHECK(length(account)=32),channel BLOB NOT NULL CHECK(length(channel)=32),
 role INTEGER NOT NULL CHECK(role IN(0,1)),template BLOB NOT NULL CHECK(length(template)=32),metadata BLOB NOT NULL,
 PRIMARY KEY(account,channel,role)) WITHOUT ROWID;";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
fn batch_on(
    c: &Connection,
    account: &[u8; 32],
    channel: ConversationId,
    role: ChannelReceptionRole,
) -> Result<Option<(EventId, ReceptionMetadata)>, LogError> {
    let row:Option<(Vec<u8>,Vec<u8>)>=c.query_row("SELECT template,metadata FROM channel_reception_batches WHERE account=?1 AND channel=?2 AND role=?3",params![account.as_slice(),channel.as_bytes().as_slice(),role.code()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(id, metadata)| {
        Ok((
            EventId::new(
                id.try_into()
                    .map_err(|_| invalid("invalid batch template"))?,
            ),
            bincode::deserialize(&metadata)
                .map_err(|_| invalid("invalid channel batch metadata"))?,
        ))
    })
    .transpose()
}
impl ChannelCheckpointStore {
    /// The receiver CAS must be installed first in this transaction. The caller
    /// then installs the body/projection/effect with these exact metadata and
    /// saves progress only if that body was newly retained.
    pub(in crate::node) fn prepare_batch_on(
        c: &Connection,
        candidate: &PreparedChannelReceive,
        role: ChannelReceptionRole,
        mut metadata: ReceptionMetadata,
    ) -> Result<ReceptionMetadata, LogError> {
        if active_account_on(c)? != candidate.account {
            return Err(LogError::StaleCursor);
        }
        if !role.matches(candidate)
            || metadata.reception_only
            || metadata.from_device != candidate.candidate.author
        {
            return Err(invalid("channel reception role does not match receiver"));
        }
        let previous = batch_on(c, &candidate.account, candidate.channel, role)?;
        if previous.is_some() || metadata.arrival_kind == ArrivalKind::CatchUp {
            metadata.arrival_kind = ArrivalKind::CatchUp;
            metadata.batch_id = Some(
                previous
                    .as_ref()
                    .and_then(|(_, m)| m.batch_id.clone())
                    .unwrap_or_else(|| format!("{}:{}", role.prefix(), candidate.event.to_hex())),
            );
            metadata.batch_size = previous
                .as_ref()
                .map_or(0, |(_, m)| m.batch_size)
                .checked_add(1)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            metadata.batch_complete = false;
        } else {
            metadata.batch_id = None;
            metadata.batch_size = 1;
            metadata.batch_complete = true;
        }
        Ok(metadata)
    }
    /// Call last inside the receiver/body/projection/effect transaction, after
    /// the body owner confirms that the exact row was newly retained.
    pub(in crate::node) fn install_batch_on(
        c: &Connection,
        candidate: &PreparedChannelReceive,
        role: ChannelReceptionRole,
        metadata: &ReceptionMetadata,
    ) -> Result<(), LogError> {
        if active_account_on(c)? != candidate.account {
            return Err(LogError::StaleCursor);
        }
        if !role.matches(candidate)
            || metadata.from_device != candidate.candidate.author
            || metadata.reception_only
        {
            return Err(invalid("channel batch does not match receiver"));
        }
        if metadata.arrival_kind != ArrivalKind::CatchUp {
            return Ok(());
        }
        if metadata.batch_complete
            || metadata.batch_size == 0
            || !metadata
                .batch_id
                .as_deref()
                .is_some_and(|id| id.starts_with(&format!("{}:", role.prefix())))
        {
            return Err(invalid("invalid pending channel batch"));
        }
        let bytes = bincode::serialize(metadata).map_err(|error| invalid(&error.to_string()))?;
        c.execute("INSERT INTO channel_reception_batches VALUES(?1,?2,?3,?4,?5) ON CONFLICT(account,channel,role) DO UPDATE SET template=excluded.template,metadata=excluded.metadata",params![candidate.account.as_slice(),candidate.channel.as_bytes().as_slice(),role.code(),candidate.event.as_bytes().as_slice(),bytes])?;
        Ok(())
    }

    /// Live/catch-up provenance is chosen before COMMIT by the bounded preview.
    pub(in crate::node) fn commit_reception(
        &self,
        candidate: &PreparedChannelReceive,
        received: &mut ReceivedStore,
        entry: &ReceivedEntry,
        metadata: ReceptionMetadata,
        retain: bool,
    ) -> Result<Option<ReceptionMetadata>, LogError> {
        received.check_write()?;
        if !ChannelReceptionRole::Message.matches(candidate)
            || entry.conversation != candidate.channel
            || entry.event_id != candidate.event
            || entry.wall_clock != candidate.wall_clock
            || entry.plaintext.as_slice() != candidate.plaintext()
            || entry.from != candidate.candidate.author
            || metadata.reception_only
        {
            return Err(invalid(
                "channel reception does not match prepared receiver",
            ));
        }
        // Imported history was already observed. New producers install the body
        // and receipt together, so exact evidence cannot hide a partial commit.
        let existing = received.contains(entry.event_id)?;
        let installed = self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            if !Self::install_receive_on(c, candidate)? {
                return Ok(None);
            }
            if !retain {
                ReceivedStore::install_evidence_on(c, entry.conversation, entry.event_id)?;
                return Ok(None);
            }
            if existing {
                ReceivedStore::install_channel_on(c, entry, None)?;
                return Ok(None);
            }
            let metadata =
                Self::prepare_batch_on(c, candidate, ChannelReceptionRole::Message, metadata)?;
            if !ReceivedStore::install_channel_on(c, entry, Some(&metadata))? {
                return Ok(None);
            }
            Self::install_batch_on(c, candidate, ChannelReceptionRole::Message, &metadata)?;
            Ok(Some(metadata))
        })?;
        received.note_channel_commit(usize::from(installed.is_some()));
        Ok(installed)
    }
    pub(in crate::node) fn finish_batch(
        &self,
        channel: ConversationId,
    ) -> Result<Option<(EventId, ReceptionMetadata)>, LogError> {
        self.finish_role_batch(channel, ChannelReceptionRole::Message)
    }
    pub(in crate::node) fn finish_file_batch(
        &self,
        channel: ConversationId,
    ) -> Result<Option<(EventId, ReceptionMetadata)>, LogError> {
        self.finish_role_batch(channel, ChannelReceptionRole::File)
    }
    fn finish_role_batch(
        &self,
        channel: ConversationId,
        role: ChannelReceptionRole,
    ) -> Result<Option<(EventId, ReceptionMetadata)>, LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let Some((event, mut metadata)) = batch_on(c, &self.account, channel, role)? else {
                return Ok(None);
            };
            metadata.reception_only = true;
            metadata.batch_complete = true;
            reception_effects::install_on(
                c,
                ReceptionSource {
                    kind: ReceptionKind::ChannelTerminal,
                    conversation: channel,
                    event_id: event,
                },
                &metadata,
            )?;
            c.execute(
                "DELETE FROM channel_reception_batches WHERE account=?1 AND channel=?2 AND role=?3",
                params![
                    self.account.as_slice(),
                    channel.as_bytes().as_slice(),
                    role.code()
                ],
            )?;
            Ok(Some((event, metadata)))
        })
    }
}

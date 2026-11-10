//! File provenance counts eligible cards. A private receiver can cross rejected
//! plaintext while retaining one candidate; durable keys never move speculatively.
use super::*;
use crate::eventlog::EventKind;

const INDEX: &str = "CREATE INDEX channel_pending_file_preview ON channel_pending(channel,kind,ordinal,event) WHERE reason=4;";
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    c.execute_batch(&INDEX.replace("CREATE INDEX ", "CREATE INDEX IF NOT EXISTS "))?;
    crate::storage::profile::manifest::validate_ddl(c, INDEX)
}

pub(in crate::node) struct FileReceivePreview {
    first: PreparedChannelReceive,
    following: Option<PreparedChannelMessage>,
}
impl FileReceivePreview {
    pub(in crate::node) fn new(first: PreparedChannelReceive) -> Result<Self, LogError> {
        if first.kind != EventKind::FileManifest {
            return Err(invalid("file preview requires a file manifest"));
        }
        Ok(Self {
            first,
            following: None,
        })
    }
    pub(in crate::node) fn first(&self) -> &PreparedChannelReceive {
        &self.first
    }
    pub(in crate::node) fn into_first(self) -> PreparedChannelReceive {
        self.first
    }
    fn checkpoint(&self) -> &PreparedChannelMessage {
        self.following.as_ref().unwrap_or(&self.first.candidate)
    }
    pub(in crate::node) fn same_chain(&self, event: &Event) -> bool {
        event.conversation_id == self.first.channel
            && event.author.user_id() == self.first.candidate.author
            && ChannelState::sender_message_epoch(&event.author.user_id(), &event.ciphertext)
                == Some(self.first.candidate.epoch)
    }
}
impl ChannelCheckpointStore {
    fn check_file_preview_on(
        &self,
        c: &Connection,
        preview: &FileReceivePreview,
    ) -> Result<(), LogError> {
        self.check_account_on(c)?;
        let first = &preview.first;
        check_event_owner_on(c, &first.owner)?;
        replay::require_source_on(c, first.channel, first.source_job, first.source_revision)?;
        let revision: Option<i64> = c.query_row(
            "SELECT revision FROM channel_receivers WHERE channel=?1 AND author=?2 AND epoch=?3",
            params![first.channel.as_bytes().as_slice(),first.candidate.author,first.candidate.epoch.to_be_bytes().as_slice()], |r|r.get(0)).optional()?;
        if first.account != self.account || revision != Some(first.revision) {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    /// The returned plaintext cannot be passed to the consuming CAS. Only the
    /// original first candidate remains eligible for publication.
    pub(in crate::node) fn preview_file_following(
        &self,
        event: &Event,
        preview: &mut FileReceivePreview,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, LogError> {
        self.profile
            .database()
            .read(|c| self.check_file_preview_on(c, preview))?;
        if event.kind != EventKind::FileManifest || !preview.same_chain(event) {
            return Err(LogError::StaleCursor);
        }
        let Some(mut prepared) = self
            .prepare_receive_with_checkpoint(event, Some((&preview.first, preview.checkpoint())))?
        else {
            return Ok(None);
        };
        let plaintext = std::mem::take(&mut prepared.candidate.plaintext);
        preview.following = Some(prepared.candidate);
        Ok(Some(plaintext))
    }
    pub(in crate::node) fn defer_file_preview(
        &self,
        event: &Event,
        preview: &FileReceivePreview,
    ) -> Result<(), LogError> {
        if event.kind != EventKind::FileManifest || !preview.same_chain(event) {
            return Err(LogError::StaleCursor);
        }
        self.profile.database().transaction(|c| {
            self.check_file_preview_on(c, preview)?;
            c.execute(
                "UPDATE channel_pending SET reason=4 WHERE channel=?1 AND event=?2 AND kind=?3",
                params![
                    event.conversation_id.as_bytes().as_slice(),
                    event.id.as_bytes().as_slice(),
                    EventKind::FileManifest as i64
                ],
            )?;
            Ok(())
        })
    }
    /// Reset at most one bounded page after resolution, source change or restart.
    /// A true result spends a turn and requires another call before exhaustion.
    pub(in crate::node) fn reset_file_preview(
        &self,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            Ok(c.execute("UPDATE channel_pending SET reason=0 WHERE channel=?1 AND event IN(SELECT event FROM channel_pending INDEXED BY channel_pending_file_preview WHERE channel=?1 AND kind=?2 AND reason=4 ORDER BY ordinal,event LIMIT 64)",
                params![channel.as_bytes().as_slice(),EventKind::FileManifest as i64])? > 0)
        })
    }
    pub(in crate::node) fn pending_file_page(
        &self,
        channel: ConversationId,
        preview: Option<&FileReceivePreview>,
    ) -> Result<pending::PendingChannelPage, LogError> {
        if let Some(preview) = preview {
            let ids = self.profile.database().read(|c| {
                self.check_file_preview_on(c,preview)?;
                let chain=preview.checkpoint();
                pending::read_page(c,"SELECT p.event,e.stored_bytes FROM channel_pending p INDEXED BY channel_pending_gap JOIN retained_events e ON e.domain=?1 AND e.id=p.event WHERE p.channel=?2 AND p.author=?3 AND p.epoch=?4 AND p.kind=?5 AND p.reason=3 AND p.admitted=1 AND p.wake_at<=?6 AND p.event<>?7 ORDER BY p.wake_at,p.ordinal,p.event LIMIT 64",
                    params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),chain.author,chain.epoch.to_be_bytes().as_slice(),EventKind::FileManifest as i64,i64::from(chain.next_position),preview.first.event.as_bytes().as_slice()])
            })?;
            if !ids.is_empty() {
                return Ok(pending::PendingChannelPage {
                    ids,
                    more_work: true,
                });
            }
        }
        self.pending_page_excluding(
            channel,
            EventKind::FileManifest,
            preview.map(|p| p.first.event),
        )
    }
}

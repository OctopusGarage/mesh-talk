//! Recall facts and consumed controls commit together. Missing target identities
//! wake exact dependency rows when that signed header arrives, never a log scan.
use super::*;
use crate::eventlog::EventKind;
use crate::node::{
    Node, NodeError,
    recall::{ChannelRecallPreparation, IndexedRecallLog},
    received_store::ReceivedStore,
};

const SCHEMA: &str = "
CREATE TABLE channel_recall_dependencies(channel BLOB NOT NULL,event BLOB NOT NULL,target BLOB,PRIMARY KEY(channel,event)) WITHOUT ROWID;
CREATE INDEX channel_recall_target ON channel_recall_dependencies(channel,target,event);
CREATE TABLE channel_recall_wakes(channel BLOB NOT NULL,target BLOB NOT NULL,PRIMARY KEY(channel,target)) WITHOUT ROWID;
";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
pub(super) fn wake_on(
    c: &Connection,
    channel: ConversationId,
    target: EventId,
) -> Result<(), LogError> {
    c.execute("INSERT OR IGNORE INTO channel_recall_wakes SELECT ?1,?2 WHERE EXISTS(SELECT 1 FROM channel_recall_dependencies WHERE channel=?1 AND target=?2)",params![channel.as_bytes().as_slice(),target.as_bytes().as_slice()])?;
    Ok(())
}
impl ChannelCheckpointStore {
    fn recall_page(&self, source: &ChannelSource) -> Result<pending::PendingChannelPage, LogError> {
        self.profile.database().transaction(|c| {
            self.check_source_on(c,source)?;
            let target:Option<Vec<u8>>=c.query_row("SELECT target FROM channel_recall_wakes WHERE channel=?1 ORDER BY target LIMIT 1",[source.channel.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            if let Some(target)=target {
                c.execute("UPDATE channel_pending SET reason=0 WHERE channel=?1 AND event IN (SELECT event FROM channel_recall_dependencies WHERE channel=?1 AND target=?2 ORDER BY event LIMIT 64)",params![source.channel.as_bytes().as_slice(),&target])?;
                c.execute("DELETE FROM channel_recall_dependencies WHERE channel=?1 AND event IN (SELECT event FROM channel_recall_dependencies WHERE channel=?1 AND target=?2 ORDER BY event LIMIT 64)",params![source.channel.as_bytes().as_slice(),&target])?;
                c.execute("DELETE FROM channel_recall_wakes WHERE channel=?1 AND target=?2 AND NOT EXISTS(SELECT 1 FROM channel_recall_dependencies WHERE channel=?1 AND target=?2)",params![source.channel.as_bytes().as_slice(),target])?;
            }
            let ids=pending::read_page(c,"SELECT p.event,e.stored_bytes FROM channel_pending p INDEXED BY channel_pending_new JOIN retained_events e ON e.domain=?1 AND e.id=p.event WHERE p.channel=?2 AND p.kind=?3 AND p.reason=0 AND p.admitted=1 ORDER BY p.ordinal,p.event LIMIT 64",params![b"MeshPartEventsV1".as_slice(),source.channel.as_bytes().as_slice(),EventKind::Delete as i64])?;
            let queued:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM channel_recall_wakes WHERE channel=?1)",[source.channel.as_bytes().as_slice()],|r|r.get(0))?;
            Ok(pending::PendingChannelPage{more_work:!ids.is_empty() || queued,ids})
        })
    }
    fn defer_recall(
        &self,
        source: &ChannelSource,
        event: EventId,
        target: Option<EventId>,
    ) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_source_on(c,source)?;
            c.execute("INSERT INTO channel_recall_dependencies VALUES(?1,?2,?3) ON CONFLICT(channel,event) DO UPDATE SET target=excluded.target",params![source.channel.as_bytes().as_slice(),event.as_bytes().as_slice(),target.as_ref().map(|id|id.as_bytes().as_slice())])?;
            c.execute("UPDATE channel_pending SET reason=2,attempt_job=?3 WHERE channel=?1 AND event=?2",params![source.channel.as_bytes().as_slice(),event.as_bytes().as_slice(),source.job.as_slice()])?;
            Ok(())
        })
    }
}
impl Node {
    pub(in crate::node) fn process_channel_recalls(
        &self,
        channel: ConversationId,
    ) -> Result<(), NodeError> {
        loop {
            let Some(source) = self.channel_checkpoints.source_binding(channel)? else {
                return Ok(());
            };
            let page = self.channel_checkpoints.recall_page(&source)?;
            for id in page.ids {
                let (event, author) = {
                    let log = self.log.lock().expect("log lock");
                    let event = log.repository()?.get(&id)?.ok_or(LogError::StaleCursor)?;
                    let state = self
                        .channel_checkpoints
                        .canonical_state(channel, id, false)?;
                    let author = state.and_then(|state| {
                        state
                            .members()
                            .iter()
                            .find(|member| member.user_id() == event.author.user_id())
                            .map(|member| member.x25519_pub)
                    });
                    (event, author)
                };
                let mark = match self.prepare_channel_recall(&event, author, true)? {
                    ChannelRecallPreparation::Retry(target) => {
                        self.channel_checkpoints.defer_recall(&source, id, target)?;
                        continue;
                    }
                    ChannelRecallPreparation::Mark(mark) => Some(mark),
                    ChannelRecallPreparation::Ignored => None,
                };
                let received = self.received.lock().expect("received lock");
                received.check_write()?;
                let mut recalls = self.recalls.lock().expect("recall lock");
                recalls.check_write()?;
                let inserted = self.storage.database().transaction(|c| {
                    self.channel_checkpoints.check_source_on(c, &source)?;
                    if ChannelCheckpointStore::processed_on(c, channel, id)? {
                        return Ok(false);
                    }
                    let inserted = match &mark {
                        Some(mark) => IndexedRecallLog::install_on(c, mark)?,
                        None => false,
                    };
                    ReceivedStore::install_evidence_on(c, channel, id)?;
                    c.execute(
                        "INSERT INTO channel_processed VALUES(?1,?2,5)",
                        params![channel.as_bytes().as_slice(), id.as_bytes().as_slice()],
                    )?;
                    c.execute(
                        "DELETE FROM channel_pending WHERE channel=?1 AND event=?2",
                        params![channel.as_bytes().as_slice(), id.as_bytes().as_slice()],
                    )?;
                    c.execute(
                        "DELETE FROM channel_recall_dependencies WHERE channel=?1 AND event=?2",
                        params![channel.as_bytes().as_slice(), id.as_bytes().as_slice()],
                    )?;
                    Ok(inserted)
                })?;
                recalls.note_committed(usize::from(inserted));
                drop(recalls);
                drop(received);
                if inserted {
                    let _ = self.channel_changes.send(channel);
                    self.note_reception_change();
                }
            }
            if !page.more_work {
                return Ok(());
            }
        }
    }
}

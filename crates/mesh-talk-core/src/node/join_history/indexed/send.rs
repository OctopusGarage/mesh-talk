//! Current membership roots select live invitations. Signed bodies are read only
//! for those permissions, and an unchanged control prefix reuses its scalar result.
use super::*;
use super::{
    continuation::{HistoryQuery, Step},
    selection::SendSelection,
};
use crate::eventlog::sync::network::PreparationBudget;
use crate::node::channel::ChannelMembershipProjection;
use std::sync::Arc;

const SCHEMA: &str = "CREATE TABLE channel_history_send_scan(job BLOB NOT NULL,channel BLOB NOT NULL,stamp INTEGER NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1)),PRIMARY KEY(job,channel)) WITHOUT ROWID;";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
struct ReleasePlan {
    source: ChannelSource,
    payload: Option<(EventKind, Vec<u8>)>,
    ready: bool,
}
impl IndexedView<'_> {
    fn cached_send(&self, stamp: i64) -> Result<Option<bool>, LogError> {
        self.checkpoints.profile().database().read(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            c.query_row("SELECT ready FROM channel_history_send_scan WHERE job=?1 AND channel=?2 AND stamp=?3",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),stamp],|r|r.get(0)).optional().map_err(Into::into)
        })
    }
    fn finish_send(&self, stamp: i64, ready: bool) -> Result<(), LogError> {
        self.checkpoints.profile().database().transaction(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            c.execute("INSERT INTO channel_history_send_scan VALUES(?1,?2,?3,?4) ON CONFLICT(job,channel) DO UPDATE SET stamp=excluded.stamp,ready=excluded.ready",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),stamp,ready])?;
            Ok(())
        })
    }
}

impl Node {
    fn plan_join_history_release(
        &self,
        channel: ConversationId,
        projection: ChannelMembershipProjection,
    ) -> Result<ReleasePlan, NodeError> {
        let (source, selected) = {
            let mut work = self.join_history_work.lock().expect("history retry lock");
            let log = self.log.lock().expect("log lock");
            let source = self
                .channel_checkpoints
                .source_binding(channel)?
                .ok_or(LogError::StaleCursor)?;
            let view = IndexedView::new(
                &self.channel_checkpoints,
                log.repository()?,
                &self.identity,
                source,
            );
            let stamp = self.channel_checkpoints.control_stamp(&source)?;
            if let Some(ready) = view.cached_send(stamp)? {
                return Ok(ReleasePlan {
                    source,
                    payload: None,
                    ready,
                });
            }
            if work
                .send
                .get(&channel)
                .is_some_and(|query| query.source != source)
            {
                work.send.remove(&channel);
            }
            if !work.send.contains_key(&channel) {
                if work.send_full() {
                    work.prune(&self.channel_checkpoints)?;
                }
                if work.send_full() {
                    return Ok(ReleasePlan {
                        source,
                        payload: None,
                        ready: false,
                    });
                }
                let me = self.identity.public();
                let projection = Arc::new(projection);
                work.send.insert(
                    channel,
                    HistoryQuery::new(source, move |context| async move {
                        context.prepare_send(me, projection).await
                    }),
                );
            }
            let mut query = work.send.remove(&channel).ok_or(LogError::StaleCursor)?;
            match query.step(&view, PreparationBudget::default())? {
                Step::Pending(_) => {
                    work.send.insert(channel, query);
                    self.channel_recovery_notify.notify_one();
                    return Ok(ReleasePlan {
                        source,
                        payload: None,
                        ready: false,
                    });
                }
                Step::Ready((None, ready), _) => {
                    view.finish_send(stamp, ready)?;
                    return Ok(ReleasePlan {
                        source,
                        payload: None,
                        ready,
                    });
                }
                Step::Ready((Some(selected), _), _) => (source, selected),
            }
        };
        let SendSelection {
            event,
            offer,
            root,
            recipient,
            acceptance,
            account,
        } = selected;
        let mut keys = Vec::new();
        let now = crate::node::node::now_millis();
        for row in &offer.rows {
            if self
                .recalls
                .lock()
                .expect("recall lock")
                .contains(channel, row.id)?
            {
                continue;
            }
            let key = row_key(&root, row.id);
            let Ok(mut plaintext) = crate::channel::open_channel_message(&key, &row.ciphertext)
            else {
                continue;
            };
            let visible = crate::expiry::split(&plaintext)
                .is_some_and(|(deadline, _)| deadline.is_none_or(|deadline| deadline > now));
            plaintext.zeroize();
            if visible {
                keys.push(RowKey {
                    id: row.id,
                    key: *key.as_bytes(),
                });
            }
        }
        let bytes = zeroize::Zeroizing::new(encode(&keys));
        let sealed_key = crate::dm::seal(&self.identity, &recipient.x25519_pub, &bytes)
            .map_err(|error| NodeError::Channel(error.to_string()))?;
        let release = Release {
            offer: event.id,
            acceptance,
            sealed_key,
        };
        let payload = if let Some((grant, device_authorization)) = account {
            (
                EventKind::ChannelDeviceInheritance,
                encode_account_release(&AccountRelease {
                    version: 1,
                    grant,
                    device_authorization,
                    recipient,
                    release,
                }),
            )
        } else {
            (
                EventKind::ChannelJoinHistory,
                encode(&HistoryControl::Release(release)),
            )
        };
        self.channel_checkpoints.check_source(&source)?;
        Ok(ReleasePlan {
            source,
            payload: Some(payload),
            ready: true,
        })
    }

    /// One existing worker turn selects one channel and advances one bounded
    /// history continuation. An appended release schedules the next control turn.
    pub(in crate::node) fn retry_join_history(&self) -> Result<Vec<ConversationId>, NodeError> {
        self.retry_join_history_turn(true)
    }
    pub(in crate::node) fn retry_join_history_new(&self) -> Result<Vec<ConversationId>, NodeError> {
        self.retry_join_history_turn(false)
    }
    fn retry_join_history_turn(
        &self,
        redistribute_ready: bool,
    ) -> Result<Vec<ConversationId>, NodeError> {
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("channel membership commit lock");
        let mut channels = self.channels.lock().expect("channels lock").channel_ids();
        channels.sort_by_key(|channel| *channel.as_bytes());
        let Some(channel) = ({
            let mut work = self.join_history_work.lock().expect("history retry lock");
            let selected = channels
                .iter()
                .copied()
                .find(|channel| {
                    work.sender_after
                        .is_none_or(|after| channel.as_bytes() > after.as_bytes())
                })
                .or_else(|| channels.first().copied());
            work.sender_after = selected;
            selected
        }) else {
            return Ok(Vec::new());
        };
        if !self.channel_replica_enabled(channel, &self.user_id()) {
            return Ok(Vec::new());
        }
        let projection = match self.account_group_projection(channel) {
            Ok(value) => value,
            Err(LogError::StaleCursor) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let Some(current) = projection.state.as_ref() else {
            return Ok(Vec::new());
        };
        if current.is_dissolved() || !self.channel_identity_is_current(current)? {
            return Ok(Vec::new());
        }
        let plan = self.plan_join_history_release(channel, projection)?;
        let appended = plan.payload.is_some();
        if let Some((kind, payload)) = plan.payload {
            self.append_channel_control_from_source(&plan.source, kind, payload)?;
            self.channel_recovery_notify.notify_one();
        }
        Ok(if plan.ready && (redistribute_ready || appended) {
            vec![channel]
        } else {
            Vec::new()
        })
    }
}

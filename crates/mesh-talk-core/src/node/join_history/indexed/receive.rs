//! A release contains at most the existing frozen snapshot budget. Its copied
//! rows, exact reception evidence and release cursor publish in one transaction.
use super::*;
use super::{
    continuation::{HistoryQuery, Step},
    selection::ReceiveSelection,
    work::ReceiveWork,
};
use crate::eventlog::sync::network::PreparationBudget;
use crate::node::{
    received_log::ReceivedEntry,
    received_store::ReceivedStore,
    reception_effects::{ArrivalKind, ReceptionMetadata},
};
use std::sync::Arc;

impl IndexedView<'_> {
    fn next_pending(&self, stamp: i64) -> Result<Option<(EventId, i64)>, LogError> {
        self.checkpoints.profile().database().transaction(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            // A same-job tail cannot alter the canonical/causal prefix of a
            // rejected release. Missing ancestry or reordered controls replace
            // the job; a later rename must not reopen immutable ciphertext.
            c.execute("INSERT INTO channel_history_scan VALUES(?1,?2,?3,-1) ON CONFLICT(job,channel) DO UPDATE SET stamp=excluded.stamp WHERE stamp<>excluded.stamp",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),stamp])?;
            let row:Option<(Vec<u8>,i64)>=c.query_row("SELECT p.event,p.rank FROM channel_history_scan s JOIN channel_history_pending p INDEXED BY channel_history_retry ON p.job=s.job AND p.channel=s.channel AND p.rank>s.after_rank WHERE s.job=?1 AND s.channel=?2 ORDER BY p.rank LIMIT 1",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            row.map(|(id,rank)|Ok::<_,LogError>((EventId::new(id.try_into().map_err(|_|LogError::CorruptFile("invalid pending history release".into()))?),rank))).transpose()
        })
    }
}
fn advance_on(
    c: &Connection,
    source: ChannelSource,
    event: EventId,
    stamp: i64,
    rank: i64,
    complete: bool,
) -> Result<(), LogError> {
    if complete {
        c.execute(
            "DELETE FROM channel_history_pending WHERE job=?1 AND channel=?2 AND event=?3",
            params![
                source.job.as_slice(),
                source.channel.as_bytes().as_slice(),
                event.as_bytes().as_slice()
            ],
        )?;
    } else {
        c.execute("UPDATE channel_history_pending SET attempt_stamp=?4 WHERE job=?1 AND channel=?2 AND event=?3",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),event.as_bytes().as_slice(),stamp])?;
    }
    if c.execute("UPDATE channel_history_scan SET after_rank=?4 WHERE job=?1 AND channel=?2 AND stamp=?3 AND after_rank<?4",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),stamp,rank])?!=1 {return Err(LogError::StaleCursor);}
    Ok(())
}

impl Node {
    pub(in crate::node) fn process_join_history_reception(
        &self,
        channel: ConversationId,
    ) -> Result<bool, NodeError> {
        let (source, stamp, event, rank, selected, name, reopening) = {
            let mut work = self.join_history_work.lock().expect("history retry lock");
            let log = self.log.lock().expect("log lock");
            let repository = log.repository()?;
            let Some(source) = self.channel_checkpoints.source_binding(channel)? else {
                return Ok(true);
            };
            if work
                .receive
                .get(&channel)
                .is_some_and(|entry| entry.query.source != source)
            {
                work.receive.remove(&channel);
            }
            let view = IndexedView::new(
                &self.channel_checkpoints,
                repository,
                &self.identity,
                source,
            );
            if !work.receive.contains_key(&channel) {
                let stamp = self.channel_checkpoints.control_stamp(&source)?;
                let Some((id, rank)) = view.next_pending(stamp)? else {
                    return Ok(true);
                };
                if work.receive_full() {
                    work.prune(&self.channel_checkpoints)?;
                }
                if work.receive_full() {
                    return Ok(false);
                }
                let Some(projection) = self.channel_checkpoints.published_projection(channel)?
                else {
                    return Ok(true);
                };
                let Some(current) = projection.state.as_ref().filter(|state| {
                    !state.is_dissolved() && state.is_accepted_member(&self.user_id())
                }) else {
                    return Ok(true);
                };
                let name = current.name().to_owned();
                let reopening = self.channel_checkpoints.retained_before_open(id)?;
                let me = self.identity.public();
                let projection = Arc::new(projection);
                let query = HistoryQuery::new(source, move |context| async move {
                    context.prepare_receive(id, me, projection).await
                });
                work.receive.insert(
                    channel,
                    ReceiveWork {
                        event: id,
                        rank,
                        stamp,
                        reopening,
                        name,
                        query,
                    },
                );
            }
            let mut entry = work.receive.remove(&channel).ok_or(LogError::StaleCursor)?;
            match entry.query.step(&view, PreparationBudget::default())? {
                Step::Pending(_) => {
                    work.receive.insert(channel, entry);
                    self.channel_recovery_notify.notify_one();
                    return Ok(false);
                }
                Step::Ready(selected, _) => (
                    source,
                    entry.stamp,
                    entry.event,
                    entry.rank,
                    selected,
                    entry.name,
                    entry.reopening,
                ),
            }
        };
        let rows =
            selected.and_then(|selected| open_selected_rows(&self.identity, channel, selected));
        let Some(rows) = rows else {
            self.storage.database().transaction(|c| {
                self.channel_checkpoints.check_source_on(c, &source)?;
                advance_on(c, source, event, stamp, rank, false)
            })?;
            self.channel_recovery_notify.notify_one();
            return Ok(false);
        };
        let own_devices: HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        // At most ten rows / the existing snapshot bytes are resident. Local
        // erasure and expiry create exact evidence without an unread effect.
        let mut candidates = Vec::with_capacity(rows.len());
        for row in rows {
            let own = own_devices.contains(&row.from);
            let suppressed = self
                .account_suppression
                .lock()
                .expect("suppression lock")
                .contains(channel, row.event_id)?;
            let expired = if reopening && !own {
                crate::expiry::deadline(&row.plaintext).is_some()
            } else {
                self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
            };
            let metadata = ReceptionMetadata {
                from_device: row.from.clone(),
                received_at: crate::node::node::now_millis(),
                arrival_kind: ArrivalKind::HistorySync,
                own_device: own,
                batch_id: None,
                batch_complete: true,
                batch_size: 1,
                reception_only: false,
            };
            candidates.push((row, metadata, !suppressed && !expired));
        }
        let mut received = self.received.lock().expect("received lock");
        received.check_write()?;
        let inserted = self.storage.database().transaction(|c| {
            if !self
                .channel_checkpoints
                .install_history_receive_on(c, &source, event)?
            {
                advance_on(c, source, event, stamp, rank, true)?;
                return Ok(Vec::new());
            }
            let mut inserted = Vec::new();
            for (index, (row, metadata, retain)) in candidates.iter().enumerate() {
                if ReceivedStore::processed_on(c, row.event_id)? {
                    continue;
                }
                if !retain {
                    ReceivedStore::install_evidence_on(c, channel, row.event_id)?;
                } else if ReceivedStore::install_channel_on(c, row, Some(metadata))? {
                    inserted.push(index);
                }
            }
            advance_on(c, source, event, stamp, rank, true)?;
            Ok(inserted)
        })?;
        received.note_channel_commit(inserted.len());
        drop(received);
        if !inserted.is_empty() {
            self.note_reception_change();
        }
        for index in inserted {
            let (row, metadata, _) = &candidates[index];
            if !metadata.own_device {
                self.note_account_local_change();
            }
            if !reopening && self.channel_incoming.is_legacy() {
                let body = MessageBody::decode(&row.plaintext);
                let _ = self.channel_incoming.send(ReceivedChannelMessage {
                    channel_id: channel,
                    channel_name: name.clone(),
                    from: row.from.clone(),
                    text: body.text,
                    protection: body.protection,
                    reply_to: body.reply_to,
                    event_id: row.event_id,
                    wall_clock: row.wall_clock,
                    wrapped: row.plaintext.clone(),
                    received_at: metadata.received_at,
                    arrival_kind: "history_sync",
                    own_device: metadata.own_device,
                    batch_id: None,
                    batch_complete: true,
                    batch_size: 1,
                    reception_only: false,
                });
            }
        }
        self.channel_recovery_notify.notify_one();
        Ok(false)
    }
}

fn open_selected_rows(
    me: &DeviceIdentity,
    channel: ConversationId,
    selected: ReceiveSelection,
) -> Option<Vec<ReceivedEntry>> {
    let ReceiveSelection {
        release,
        offer,
        author,
        invitation,
        originals,
    } = selected;
    let mut key_bytes = crate::dm::open(me, &author.x25519_pub, &release.sealed_key).ok()?;
    let keys: Option<Vec<RowKey>> = decode(&key_bytes);
    key_bytes.zeroize();
    let keys = keys?;
    if keys.len() > offer.policy.count() {
        return None;
    }
    let mut rows = Vec::new();
    let mut ids = HashSet::new();
    for key in keys {
        let Some(encrypted) = offer.rows.iter().find(|row| row.id == key.id) else {
            continue;
        };
        let Ok(body) = crate::channel::open_channel_message(
            &GroupKey::from_bytes(key.key),
            &encrypted.ciphertext,
        ) else {
            continue;
        };
        let original = originals.iter().find(|header| header.id == key.id)?;
        // The same source-fenced offer query already proved each selected row
        // belongs to invitation ancestry; never reopen that graph after keys.
        if original.kind != EventKind::Message
            || !offer
                .policy
                .permits_time(original.wall_clock, invitation.wall_clock)
            || crate::expiry::split(&body).is_none()
        {
            return None;
        }
        if ids.insert(key.id) {
            rows.push(ReceivedEntry {
                event_id: key.id,
                conversation: channel,
                from: original.author.user_id(),
                wall_clock: original.wall_clock,
                plaintext: body,
            });
        }
    }
    Some(rows)
}

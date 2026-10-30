//! A release contains at most the existing frozen snapshot budget. Its copied
//! rows, exact reception evidence and release cursor publish in one transaction.
use super::*;
use crate::node::{
    channel::{ChannelMembershipProjection, graph::IndexedGraph},
    received_log::ReceivedEntry,
    received_store::ReceivedStore,
    reception_effects::{ArrivalKind, ReceptionMetadata},
};

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
    fn prepare_rows(
        &self,
        event: &Event,
        projection: &ChannelMembershipProjection,
    ) -> Result<Option<Vec<ReceivedEntry>>, LogError> {
        let Some(current) = &projection.state else {
            return Ok(None);
        };
        let (release, account) = if event.kind == EventKind::ChannelDeviceInheritance {
            let Some(value) = decode_account_release(&event.ciphertext)
                .filter(|value| value.recipient == self.me.public())
            else {
                return Ok(None);
            };
            let graph = IndexedGraph {
                repository: self.repository,
                conversation: self.source.channel,
            };
            if !account_release_admitted(current, event, &projection.authority, &graph)? {
                return Ok(None);
            }
            (value.release, true)
        } else if event.kind == EventKind::ChannelJoinHistory {
            let Some(HistoryControl::Release(value)) = decode(&event.ciphertext) else {
                return Ok(None);
            };
            (value, false)
        } else {
            return Ok(None);
        };
        let Some((offer_event, offer)) = self.first_offer(release.offer)? else {
            return Ok(None);
        };
        if (!account && offer.recipient != self.me.user_id())
            || (account && self.lifetime_closed(&offer, Some(event.id))?)
            || (!account && self.release(event, &offer_event, &offer)?.is_none())
        {
            return Ok(None);
        }
        let Some(author) = self.author(event.id)? else {
            return Ok(None);
        };
        let Ok(mut key_bytes) = crate::dm::open(self.me, &author.x25519_pub, &release.sealed_key)
        else {
            return Ok(None);
        };
        let keys: Option<Vec<RowKey>> = decode(&key_bytes);
        key_bytes.zeroize();
        let Some(keys) = keys else {
            return Ok(None);
        };
        if keys.len() > offer.policy.count() {
            return Ok(None);
        }
        let Some(invitation) = self.repository.header(&offer.invitation)? else {
            return Ok(None);
        };
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
            let Some(original) = self.repository.header(&key.id)? else {
                return Ok(None);
            };
            if original.kind != EventKind::Message
                || !offer
                    .policy
                    .permits_time(original.wall_clock, invitation.wall_clock)
                || !self.repository.descends_from(
                    self.source.channel,
                    invitation.id,
                    original.id,
                )?
                || crate::expiry::split(&body).is_none()
            {
                return Ok(None);
            }
            if ids.insert(key.id) {
                rows.push(ReceivedEntry {
                    event_id: key.id,
                    conversation: self.source.channel,
                    from: original.author.user_id(),
                    wall_clock: original.wall_clock,
                    plaintext: body,
                });
            }
        }
        self.checkpoints.check_source(&self.source)?;
        Ok(Some(rows))
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
        reopening: bool,
    ) -> Result<(), NodeError> {
        let own_devices: HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        loop {
            let (source, stamp, event, rank, rows, name) = {
                let log = self.log.lock().expect("log lock");
                let repository = log.repository()?;
                let Some(source) = self.channel_checkpoints.source_binding(channel)? else {
                    return Ok(());
                };
                let Some(projection) = self.channel_checkpoints.published_projection(channel)?
                else {
                    return Ok(());
                };
                let Some(current) = projection.state.as_ref().filter(|state| {
                    !state.is_dissolved() && state.is_accepted_member(&self.user_id())
                }) else {
                    return Ok(());
                };
                self.channel_checkpoints.check_source(&source)?;
                let view = IndexedView {
                    checkpoints: &self.channel_checkpoints,
                    repository,
                    me: &self.identity,
                    source,
                };
                let stamp = self.channel_checkpoints.control_stamp(&source)?;
                let Some((id, rank)) = view.next_pending(stamp)? else {
                    return Ok(());
                };
                let event = repository.get(&id)?.ok_or(LogError::StaleCursor)?;
                let rows = view.prepare_rows(&event, &projection)?;
                (source, stamp, id, rank, rows, current.name().to_owned())
            };
            let Some(rows) = rows else {
                self.storage.database().transaction(|c| {
                    self.channel_checkpoints.check_source_on(c, &source)?;
                    advance_on(c, source, event, stamp, rank, false)
                })?;
                continue;
            };
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
        }
    }
}

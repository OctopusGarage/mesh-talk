//! Bounded direct and channel history catch-up between certified account devices.

use super::account_sync_policy::{DeviceSyncPolicy, SyncCategory};
use super::account_sync_wire::{AccountHistoryFrame, MAX_CHUNK, MAX_RECORD, VERSION};
use super::conversation::account_conversation_id;
use super::dm_envelope::DmEnvelope;
use super::node::NodeError;
use super::pairing::BackfillRecord;
use super::session::SessionError;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::device::PublicIdentity;
use crate::transport::SecureChannel;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

const RECORDS_PER_PAGE: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Default)]
struct PeerCursor {
    next: u32,
    known_digest: [u8; 32],
    sweep_digest: [u8; 32],
}

#[derive(Clone)]
struct PendingRecord {
    channel: bool,
    conversation: [u8; 32],
    from: String,
    wall_clock: u64,
    event_id: [u8; 32],
    total_len: usize,
    plaintext: Vec<u8>,
}

#[derive(Clone)]
struct ChannelRecord {
    conversation: [u8; 32],
    from: String,
    wall_clock: u64,
    event_id: [u8; 32],
    plaintext: Vec<u8>,
}

enum HistoryRecord {
    Direct(BackfillRecord),
    Channel(ChannelRecord),
}

impl HistoryRecord {
    fn id(&self) -> [u8; 32] {
        match self {
            Self::Direct(record) => record.event_id,
            Self::Channel(record) => record.event_id,
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Self::Direct(_) => 0,
            Self::Channel(_) => 1,
        }
    }
}

impl PendingRecord {
    fn finish(self, own_account_id: &str) -> Option<HistoryRecord> {
        if self.plaintext.len() != self.total_len {
            return None;
        }
        if self.channel {
            return Some(HistoryRecord::Channel(ChannelRecord {
                conversation: self.conversation,
                from: self.from,
                wall_clock: self.wall_clock,
                event_id: self.event_id,
                plaintext: self.plaintext,
            }));
        }
        let env = DmEnvelope::decode(&self.plaintext)?;
        if env.msg_id != self.event_id || env.route.sender_account != self.from {
            return None;
        }
        let other = if env.route.sender_account == own_account_id {
            &env.route.recipient_account
        } else if env.route.recipient_account == own_account_id {
            &env.route.sender_account
        } else {
            return None;
        };
        if *account_conversation_id(own_account_id, other).as_bytes() != self.conversation {
            return None;
        }
        Some(HistoryRecord::Direct(BackfillRecord {
            conv: self.conversation,
            from: self.from,
            wall_clock: self.wall_clock,
            plaintext: self.plaintext,
            event_id: self.event_id,
        }))
    }
}

fn transport(error: crate::transport::TransportError) -> NodeError {
    NodeError::Session(SessionError::Transport(error))
}

async fn send_frame(
    channel: &mut SecureChannel<TcpStream>,
    frame: &AccountHistoryFrame,
) -> Result<(), NodeError> {
    let bytes = frame
        .encode()
        .ok_or_else(|| NodeError::Channel("invalid account-history frame".into()))?;
    channel.send(&bytes).await.map_err(transport)
}

impl Node {
    pub fn device_sync_policy(&self) -> DeviceSyncPolicy {
        self.account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned")
            .current()
    }

    pub fn set_device_sync_category(
        &self,
        category: SyncCategory,
        enabled: bool,
    ) -> Result<(), NodeError> {
        if !matches!(category, SyncCategory::History | SyncCategory::Profile) {
            return Err(NodeError::Channel(
                "this sync category is not available yet".into(),
            ));
        }
        self.account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned")
            .set(category, enabled)
            .map_err(NodeError::Log)?;
        if category == SyncCategory::History && enabled {
            self.account_sync_notify.notify_one();
        }
        if category == SyncCategory::Profile && enabled {
            let conversations = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .conversations();
            for conversation in conversations {
                self.process_profile_events(conversation);
            }
        }
        Ok(())
    }

    fn export_sync_records(&self, requester: &PublicIdentity) -> Vec<HistoryRecord> {
        let own = self.account_id();
        let mut records = self.export_account_backfill();
        records.retain_mut(|record| {
            if record.plaintext.len() > MAX_RECORD {
                return false;
            }
            let Some(env) = DmEnvelope::decode(&record.plaintext) else {
                return false;
            };
            if env.route.sender_account != own && env.route.recipient_account != own {
                return false;
            }
            record.event_id = env.msg_id;
            true
        });
        records.sort_by_key(|record| record.event_id);
        records.dedup_by_key(|record| record.event_id);
        let mut merged: Vec<HistoryRecord> =
            records.into_iter().map(HistoryRecord::Direct).collect();
        let requester_id = requester.user_id();
        let me = self.user_id();
        let channels = {
            let book = self.channels.lock().expect("channels mutex not poisoned");
            book.channel_ids()
                .into_iter()
                .filter(|id| {
                    book.state(id).is_some_and(|state| {
                        !state.is_dissolved()
                            && state.is_accepted_member(&me)
                            && state.is_member(&requester_id)
                    })
                })
                .collect::<Vec<_>>()
        };
        for channel in channels {
            let mut seen = HashSet::new();
            for (id, entry) in self.raw_sent_history(channel) {
                if entry.plaintext.is_empty()
                    || entry.plaintext.len() > MAX_RECORD
                    || !seen.insert(id)
                {
                    continue;
                }
                let from = self
                    .log
                    .lock()
                    .expect("log mutex not poisoned")
                    .get(&id)
                    .map(|event| event.author.user_id())
                    .unwrap_or_else(|| me.clone());
                merged.push(HistoryRecord::Channel(ChannelRecord {
                    conversation: *channel.as_bytes(),
                    from,
                    wall_clock: entry.wall_clock,
                    event_id: *id.as_bytes(),
                    plaintext: entry.plaintext,
                }));
            }
            let received = self
                .received
                .lock()
                .expect("received mutex not poisoned")
                .entries(&channel);
            for entry in received {
                if entry.plaintext.is_empty()
                    || entry.plaintext.len() > MAX_RECORD
                    || !seen.insert(entry.event_id)
                {
                    continue;
                }
                merged.push(HistoryRecord::Channel(ChannelRecord {
                    conversation: *channel.as_bytes(),
                    from: entry.from,
                    wall_clock: entry.wall_clock,
                    event_id: *entry.event_id.as_bytes(),
                    plaintext: entry.plaintext,
                }));
            }
        }
        merged.sort_by_key(|record| (record.tag(), record.id()));
        merged
    }

    pub(in crate::node) async fn serve_account_history(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        cursor: u32,
        known_digest: [u8; 32],
    ) {
        if account_id != self.account_id() || !channel.io_admitted() {
            return;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return;
        }
        for device in self.linked_devices() {
            if send_frame(channel, &AccountHistoryFrame::Device { device })
                .await
                .is_err()
            {
                return;
            }
        }
        if !self.device_sync_policy().history {
            let _ = send_frame(
                channel,
                &AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest: [0; 32],
                },
            )
            .await;
            return;
        }
        let records = self.export_sync_records(&peer);
        let mut hasher = Sha256::new();
        for record in &records {
            hasher.update([record.tag()]);
            hasher.update(record.id());
        }
        let digest: [u8; 32] = hasher.finalize().into();
        if cursor == 0 && known_digest == digest {
            let _ = send_frame(
                channel,
                &AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest,
                },
            )
            .await;
            return;
        }
        let start = (cursor as usize).min(records.len());
        let end = start.saturating_add(RECORDS_PER_PAGE).min(records.len());
        for record in &records[start..end] {
            let (header, plaintext) = match record {
                HistoryRecord::Direct(record) => (
                    AccountHistoryFrame::RecordHeader {
                        conversation: record.conv,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
                HistoryRecord::Channel(record) => (
                    AccountHistoryFrame::ChannelHeader {
                        channel: record.conversation,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
            };
            if send_frame(channel, &header).await.is_err() {
                return;
            }
            for chunk in plaintext.chunks(MAX_CHUNK) {
                if send_frame(
                    channel,
                    &AccountHistoryFrame::RecordChunk {
                        bytes: chunk.to_vec(),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
            }
        }
        let next_cursor = (end < records.len()).then_some(end as u32);
        let _ = send_frame(
            channel,
            &AccountHistoryFrame::Complete {
                next_cursor,
                digest,
            },
        )
        .await;
    }

    async fn pull_account_history_page(
        &self,
        peer: &PeerRecord,
        cursor: u32,
        known_digest: [u8; 32],
    ) -> Result<(Option<u32>, [u8; 32]), NodeError> {
        if !self.device_sync_policy().history {
            return Ok((None, [0; 32]));
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self.linked_devices().iter().any(|device| {
                device.public == peer.public
                    && device.certificate.verify()
                    && device.certificate.account_id() == own
                    && device.certificate.device_ed25519_pub == peer.public.ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "peer is not a certified linked device".into(),
            ));
        }
        let mut channel = self
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(transport)?;
        send_frame(
            &mut channel,
            &AccountHistoryFrame::Request {
                version: VERSION,
                account_id: own.clone(),
                cursor,
                known_digest,
            },
        )
        .await?;
        let mut pending: Option<PendingRecord> = None;
        let mut page = Vec::new();
        let mut seen_devices = HashSet::new();
        let mut started_records = false;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("account-history read timed out".into()))?
                .map_err(transport)?;
            let frame = AccountHistoryFrame::decode(&bytes)
                .ok_or_else(|| NodeError::Channel("invalid account-history response".into()))?;
            match frame {
                AccountHistoryFrame::Device { device }
                    if !started_records && pending.is_none() && seen_devices.len() < 64 =>
                {
                    if !seen_devices.insert(device.public.user_id()) {
                        return Err(NodeError::Channel("duplicate linked device".into()));
                    }
                    self.account_devices
                        .lock()
                        .expect("account devices mutex not poisoned")
                        .insert(device)
                        .map_err(NodeError::Log)?;
                }
                AccountHistoryFrame::RecordHeader {
                    conversation,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } if pending.is_none() => {
                    started_records = true;
                    if page.len() >= RECORDS_PER_PAGE {
                        return Err(NodeError::Channel(
                            "too many account-history records".into(),
                        ));
                    }
                    if total_len == 0 || total_len as usize > MAX_RECORD {
                        return Err(NodeError::Channel(
                            "account-history record too large".into(),
                        ));
                    }
                    pending = Some(PendingRecord {
                        channel: false,
                        conversation,
                        from,
                        wall_clock,
                        event_id,
                        total_len: total_len as usize,
                        plaintext: Vec::with_capacity(total_len as usize),
                    });
                }
                AccountHistoryFrame::ChannelHeader {
                    channel: conversation,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } if pending.is_none() => {
                    started_records = true;
                    if page.len() >= RECORDS_PER_PAGE
                        || total_len == 0
                        || total_len as usize > MAX_RECORD
                    {
                        return Err(NodeError::Channel("invalid channel-history record".into()));
                    }
                    pending = Some(PendingRecord {
                        channel: true,
                        conversation,
                        from,
                        wall_clock,
                        event_id,
                        total_len: total_len as usize,
                        plaintext: Vec::with_capacity(total_len as usize),
                    });
                }
                AccountHistoryFrame::RecordChunk { bytes } => {
                    if bytes.is_empty() || bytes.len() > MAX_CHUNK {
                        return Err(NodeError::Channel("invalid account-history chunk".into()));
                    }
                    let Some(record) = pending.as_mut() else {
                        return Err(NodeError::Channel("unexpected history chunk".into()));
                    };
                    if record.plaintext.len().saturating_add(bytes.len()) > record.total_len {
                        return Err(NodeError::Channel("history record overflow".into()));
                    }
                    record.plaintext.extend_from_slice(&bytes);
                    if record.plaintext.len() == record.total_len {
                        let complete = pending.take().unwrap();
                        let record = complete.finish(&own).ok_or_else(|| {
                            NodeError::Channel("invalid account-history record".into())
                        })?;
                        page.push(record);
                    }
                }
                AccountHistoryFrame::Complete {
                    next_cursor,
                    digest,
                } if pending.is_none() => {
                    if self.device_sync_policy().history {
                        {
                            let book = self.channels.lock().expect("channels mutex not poisoned");
                            for record in &page {
                                if let HistoryRecord::Channel(record) = record {
                                    let channel = ConversationId::new(record.conversation);
                                    if !book.state(&channel).is_some_and(|state| {
                                        !state.pending_invite()
                                            && state.is_accepted_member(&self.user_id())
                                    }) {
                                        return Err(NodeError::Authorization(
                                            "channel history requires accepted membership".into(),
                                        ));
                                    }
                                }
                            }
                        }
                        {
                            let log = self.log.lock().expect("log mutex not poisoned");
                            for record in &page {
                                if let HistoryRecord::Channel(record) = record {
                                    let channel = ConversationId::new(record.conversation);
                                    if let Some(event) = log.get(&EventId::new(record.event_id))
                                        && (event.conversation_id != channel
                                            || event.kind != crate::eventlog::EventKind::Message
                                            || event.author.user_id() != record.from)
                                    {
                                        return Err(NodeError::Authorization(
                                            "channel history conflicts with signed event".into(),
                                        ));
                                    }
                                }
                            }
                        }
                        let direct: Vec<_> = page
                            .iter()
                            .filter_map(|record| match record {
                                HistoryRecord::Direct(record) => Some(record.clone()),
                                HistoryRecord::Channel(_) => None,
                            })
                            .collect();
                        self.import_account_backfill(&direct)?;
                        for record in page {
                            if let HistoryRecord::Channel(record) = record {
                                let conversation = ConversationId::new(record.conversation);
                                let event_id = EventId::new(record.event_id);
                                if self
                                    .account_suppression
                                    .lock()
                                    .expect("account suppression mutex not poisoned")
                                    .contains(conversation, event_id)
                                {
                                    continue;
                                }
                                self.received
                                    .lock()
                                    .expect("received mutex not poisoned")
                                    .record(
                                        conversation,
                                        record.from,
                                        record.wall_clock,
                                        &record.plaintext,
                                        event_id,
                                    )
                                    .map_err(NodeError::Log)?;
                            }
                        }
                    }
                    return Ok((next_cursor, digest));
                }
                _ => return Err(NodeError::Channel("unexpected history frame".into())),
            }
        }
    }

    pub(in crate::node) async fn run_account_history_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursors: HashMap<String, PeerCursor> = HashMap::new();
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = self.account_sync_notify.notified() => {}
            }
            if !self.device_sync_policy().history {
                cursors.clear();
                continue;
            }
            for device in self.linked_devices() {
                let id = device.public.user_id();
                if id == self.user_id() {
                    continue;
                }
                let Some(peer) = self.routing_peer(&id) else {
                    continue;
                };
                if peer.public != device.public {
                    continue;
                }
                let state = cursors.entry(id).or_default();
                if let Ok((next, digest)) = self
                    .pull_account_history_page(&peer, state.next, state.known_digest)
                    .await
                {
                    match next {
                        Some(next) if state.next == 0 => {
                            state.next = next;
                            state.sweep_digest = digest;
                            state.known_digest = [0; 32];
                        }
                        Some(next) if state.sweep_digest == digest => state.next = next,
                        Some(_) => *state = PeerCursor::default(),
                        None if state.next == 0 || state.sweep_digest == digest => {
                            state.next = 0;
                            state.known_digest = digest;
                        }
                        None => *state = PeerCursor::default(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;

    #[test]
    fn pending_history_record_rejects_cross_account_and_tampered_headers() {
        let own = Account::generate().account_id();
        let peer = Account::generate().account_id();
        let outsider = Account::generate().account_id();
        let id = [9; 32];
        let plaintext =
            DmEnvelope::new(peer.clone(), own.clone(), id, b"history".to_vec()).encode();
        let valid = PendingRecord {
            channel: false,
            conversation: *account_conversation_id(&own, &peer).as_bytes(),
            from: peer.clone(),
            wall_clock: 1,
            event_id: id,
            total_len: plaintext.len(),
            plaintext,
        };
        assert!(valid.clone().finish(&own).is_some());
        assert!(valid.clone().finish(&outsider).is_none());
        assert!(
            PendingRecord {
                from: outsider,
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                event_id: [8; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                conversation: [7; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                total_len: valid.total_len + 1,
                ..valid
            }
            .finish(&own)
            .is_none()
        );
    }
}

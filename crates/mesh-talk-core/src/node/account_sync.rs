//! Bounded direct history catch-up between certified devices on one account.

use super::account_sync_policy::{DeviceSyncPolicy, SyncCategory};
use super::account_sync_wire::{AccountHistoryFrame, MAX_CHUNK, MAX_RECORD, VERSION};
use super::conversation::account_conversation_id;
use super::dm_envelope::DmEnvelope;
use super::node::NodeError;
use super::pairing::BackfillRecord;
use super::session::SessionError;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::transport::SecureChannel;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

const RECORDS_PER_PAGE: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct PendingRecord {
    conversation: [u8; 32],
    from: String,
    wall_clock: u64,
    event_id: [u8; 32],
    total_len: usize,
    plaintext: Vec<u8>,
}

impl PendingRecord {
    fn finish(self, own_account_id: &str) -> Option<BackfillRecord> {
        if self.plaintext.len() != self.total_len {
            return None;
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
        Some(BackfillRecord {
            conv: self.conversation,
            from: self.from,
            wall_clock: self.wall_clock,
            plaintext: self.plaintext,
            event_id: self.event_id,
        })
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
        self.account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned")
            .set(category, enabled)
            .map_err(NodeError::Log)?;
        if category == SyncCategory::History && enabled {
            self.account_sync_notify.notify_one();
        }
        Ok(())
    }

    fn export_sync_records(&self) -> Vec<BackfillRecord> {
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
        records
    }

    pub(in crate::node) async fn serve_account_history(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        cursor: u32,
    ) {
        if account_id != self.account_id()
            || !channel.io_admitted()
            || !self.device_sync_policy().history
        {
            return;
        }
        let peer = channel.peer_identity();
        if !self
            .linked_devices()
            .iter()
            .any(|device| device.public == *peer && device.certificate.verify())
        {
            return;
        }
        let records = self.export_sync_records();
        let start = (cursor as usize).min(records.len());
        let end = start.saturating_add(RECORDS_PER_PAGE).min(records.len());
        for record in &records[start..end] {
            let header = AccountHistoryFrame::RecordHeader {
                conversation: record.conv,
                from: record.from.clone(),
                wall_clock: record.wall_clock,
                event_id: record.event_id,
                total_len: record.plaintext.len() as u32,
            };
            if send_frame(channel, &header).await.is_err() {
                return;
            }
            for chunk in record.plaintext.chunks(MAX_CHUNK) {
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
        let _ = send_frame(channel, &AccountHistoryFrame::Complete { next_cursor }).await;
    }

    async fn pull_account_history_page(
        &self,
        peer: &PeerRecord,
        cursor: u32,
    ) -> Result<Option<u32>, NodeError> {
        if !self.device_sync_policy().history {
            return Ok(None);
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self
                .linked_devices()
                .iter()
                .any(|device| device.public == peer.public)
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
            },
        )
        .await?;
        let mut pending: Option<PendingRecord> = None;
        let mut page = Vec::new();
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("account-history read timed out".into()))?
                .map_err(transport)?;
            let frame = AccountHistoryFrame::decode(&bytes)
                .ok_or_else(|| NodeError::Channel("invalid account-history response".into()))?;
            match frame {
                AccountHistoryFrame::RecordHeader {
                    conversation,
                    from,
                    wall_clock,
                    event_id,
                    total_len,
                } if pending.is_none() => {
                    if page.len() >= RECORDS_PER_PAGE {
                        return Err(NodeError::Channel(
                            "too many account-history records".into(),
                        ));
                    }
                    pending = Some(PendingRecord {
                        conversation,
                        from,
                        wall_clock,
                        event_id,
                        total_len: total_len as usize,
                        plaintext: Vec::with_capacity(total_len as usize),
                    });
                }
                AccountHistoryFrame::RecordChunk { bytes } => {
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
                AccountHistoryFrame::Complete { next_cursor } if pending.is_none() => {
                    if self.device_sync_policy().history {
                        self.import_account_backfill(&page)?;
                    }
                    return Ok(next_cursor);
                }
                _ => return Err(NodeError::Channel("unexpected history frame".into())),
            }
        }
    }

    pub(in crate::node) async fn run_account_history_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut cursors: HashMap<String, u32> = HashMap::new();
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
                let cursor = *cursors.get(&id).unwrap_or(&0);
                if let Ok(next) = self.pull_account_history_page(&peer, cursor).await {
                    cursors.insert(id, next.unwrap_or(0));
                }
            }
        }
    }
}

//! Frozen, owner-authorized history copies. The offer contains a separately
//! encrypted snapshot, never sender-chain keys. Existing accepted holders can
//! release its independent key only after the invitee's signed acceptance.
use super::channel::{ChannelBook, ReceivedChannelMessage, SealedPayload};
use super::{HistoryEntry, Node, NodeError};
use crate::channel::{GroupKey, HistoryPolicy};
use crate::eventlog::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::message::MessageBody;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use zeroize::Zeroize;

#[cfg(test)]
const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const SNAPSHOT_BYTES: usize = 24 * 1024;
const WIRE_BYTES: u64 = 60 * 1024;
const MAX_SOURCES: usize = 64;
const MAX_RETRIES: usize = 8;

fn encode<T: Serialize>(value: &T) -> Vec<u8> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(value)
        .expect("history record serializes")
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(WIRE_BYTES)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}

#[derive(Serialize, Deserialize)]
struct SnapshotRow {
    id: EventId,
    body: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct EncryptedRow {
    id: EventId,
    ciphertext: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct RowKey {
    id: EventId,
    key: [u8; 32],
}
impl Drop for RowKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}
fn row_key(root: &GroupKey, id: EventId) -> GroupKey {
    let mut key = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(b"mesh-talk-join-history-v1"), root.as_bytes())
        .expand(id.as_bytes(), &mut key)
        .expect("fixed key length");
    GroupKey::from_bytes(key)
}

#[derive(Serialize, Deserialize)]
struct Offer {
    recipient: String,
    invitation: EventId,
    policy: HistoryPolicy,
    /// A new random key encrypts ONLY this bounded snapshot. Each already
    /// accepted source receives that key sealed to its own device identity.
    keys: SealedPayload,
    rows: Vec<EncryptedRow>,
}
#[derive(Serialize, Deserialize)]
struct Release {
    offer: EventId,
    acceptance: EventId,
    sealed_key: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
enum HistoryControl {
    Offer(Offer),
    Release(Release),
}

struct View<'a> {
    events: Vec<&'a Event>,
    by_id: HashMap<EventId, &'a Event>,
}
impl<'a> View<'a> {
    fn new(events: &[&'a Event]) -> Self {
        let by_id: HashMap<_, _> = events.iter().map(|event| (event.id, *event)).collect();
        let events = super::timeline::ordered_ids(events, &[], true)
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        Self { events, by_id }
    }
    fn before(&self, event: &Event) -> Option<crate::channel::ChannelState> {
        // Only ancestors establish authority; concurrent control events cannot
        // grant permission to a snapshot or release that did not observe them.
        let mut ancestors = HashSet::new();
        let mut pending = event.parents.clone();
        while let Some(id) = pending.pop() {
            if ancestors.insert(id)
                && let Some(parent) = self.by_id.get(&id)
            {
                pending.extend_from_slice(&parent.parents);
            }
        }
        let ancestors: Vec<_> = self
            .events
            .iter()
            .copied()
            .filter(|prior| ancestors.contains(&prior.id))
            .collect();
        ChannelBook::membership_view(event.conversation_id, &ancestors, None, &self.by_id).0
    }
    fn canonical_before(&self, event: &Event) -> Option<crate::channel::ChannelState> {
        ChannelBook::membership_view(
            event.conversation_id,
            &self.events,
            Some(event.id),
            &self.by_id,
        )
        .0
    }
    fn owner_authorized(&self, event: &Event) -> bool {
        let author = event.author.user_id();
        let Some(current) = self.canonical_before(event) else {
            return false;
        };
        let Some(causal) = self.before(event) else {
            return false;
        };
        if current.owner() != author
            || current.is_dissolved()
            || causal.owner() != author
            || causal.is_dissolved()
        {
            return false;
        }
        // A transfer closes the old owner's control lifetime. Its observed
        // ancestry pins inherited history settings/offers, so the former owner
        // cannot backdate an unobserved fork into that closed lifetime.
        let Some(position) = self
            .events
            .iter()
            .position(|candidate| candidate.id == event.id)
        else {
            return false;
        };
        !self.events[position + 1..].iter().any(|transfer| {
            if transfer.kind != EventKind::ChannelOwnerTransfer
                || transfer.author.user_id() != author
                || super::channel::descends_from(transfer, event.id, &self.by_id)
            {
                return false;
            }
            let Some(mut state) = self.canonical_before(transfer) else {
                return false;
            };
            state.owner() == author
                && !state.is_dissolved()
                && std::str::from_utf8(&transfer.ciphertext)
                    .ok()
                    .is_some_and(|next| next != author && state.transfer_owner(next))
        })
    }
    fn policy(&self, cutoff: Option<&Event>) -> HistoryPolicy {
        self.events
            .iter()
            .filter(|event| {
                event.kind == EventKind::ChannelHistoryPolicy
                    && cutoff.is_none_or(|cutoff| {
                        super::channel::descends_from(cutoff, event.id, &self.by_id)
                    })
            })
            .filter_map(|event| {
                self.owner_authorized(event)
                    .then(|| decode(&event.ciphertext))
                    .flatten()
            })
            .next_back()
            .unwrap_or_default()
    }
    fn offer(&self, event: &Event) -> Option<Offer> {
        let HistoryControl::Offer(offer) = decode(&event.ciphertext)? else {
            return None;
        };
        let invitation = *self.by_id.get(&offer.invitation)?;
        let before = self.before(invitation)?;
        let state = self.before(event)?;
        let meta = crate::channel::ChannelMeta::decode(&invitation.ciphertext)?;
        let mut canonical_invite = self.canonical_before(invitation)?;
        if canonical_invite.is_member(&offer.recipient) {
            return None;
        }
        canonical_invite.apply_meta(meta.clone());
        if !canonical_invite.is_member(&offer.recipient) {
            return None;
        }
        if !self.owner_authorized(invitation)
            || !self.owner_authorized(event)
            || invitation.kind != EventKind::MembershipChange
            || !crate::channel::ChannelMeta::is_invitation(&invitation.ciphertext)
            || before.is_member(&offer.recipient)
            || !meta.is_member(&offer.recipient)
            || invitation.author.user_id() != before.owner()
            || event.author != invitation.author
            || state.owner() != event.author.user_id()
            || state.is_dissolved()
            || !super::channel::descends_from(event, invitation.id, &self.by_id)
            || offer.policy == HistoryPolicy::Off
            || offer.policy != self.policy(Some(invitation))
            || offer.keys.sender != event.author.user_id()
            || offer.rows.len() > offer.policy.count()
            || offer
                .rows
                .iter()
                .map(|row| row.ciphertext.len() + 32)
                .sum::<usize>()
                > SNAPSHOT_BYTES
            || offer.keys.entries.len() > MAX_SOURCES
            || offer.keys.entries.iter().any(|entry| {
                entry.user_id == offer.recipient || !before.is_accepted_member(&entry.user_id)
            })
        {
            return None;
        }
        let mut identities = HashSet::new();
        for row in &offer.rows {
            let original = *self.by_id.get(&row.id)?;
            if !identities.insert(row.id)
                || original.kind != EventKind::Message
                || !offer
                    .policy
                    .permits_time(original.wall_clock, invitation.wall_clock)
                || !super::channel::descends_from(invitation, original.id, &self.by_id)
            {
                return None;
            }
            let authorized = self.before(original)?;
            let canonical = self.canonical_before(original)?;
            if offer.keys.entries.iter().any(|source| {
                source.user_id != event.author.user_id()
                    && (!authorized.is_accepted_member(&source.user_id)
                        || !canonical.is_accepted_member(&source.user_id))
            }) {
                return None;
            }
        }
        Some(offer)
    }
    fn offers(&self) -> Vec<(&Event, Offer)> {
        let mut seen = HashSet::new();
        self.events
            .iter()
            .filter(|event| event.kind == EventKind::ChannelJoinHistory)
            .filter_map(|event| self.offer(event).map(|offer| (*event, offer)))
            .filter(|(_, offer)| seen.insert((offer.invitation, offer.recipient.clone())))
            .collect()
    }
    fn lifetime_closed(&self, offer: &Offer, cutoff: Option<&Event>) -> bool {
        self.events.iter().any(|change| {
            if !matches!(
                change.kind,
                EventKind::MembershipChange | EventKind::ChannelLeave
            ) || !super::channel::descends_from(change, offer.invitation, &self.by_id)
                || cutoff.is_some_and(|event| {
                    !super::channel::descends_from(event, change.id, &self.by_id)
                })
            {
                return false;
            }
            let Some(mut state) = self.canonical_before(change) else {
                return false;
            };
            if change.kind == EventKind::ChannelLeave {
                return change.ciphertext.is_empty()
                    && change.author.user_id() == offer.recipient
                    && state.leave_member(&offer.recipient);
            }
            if state.owner() != change.author.user_id() {
                return false;
            }
            let Some(meta) = crate::channel::ChannelMeta::decode(&change.ciphertext) else {
                return false;
            };
            state.apply_meta(meta);
            !state.is_member(&offer.recipient)
        })
    }
    fn release(&self, event: &Event, offer_event: &Event, offer: &Offer) -> Option<Release> {
        let HistoryControl::Release(release) = decode(&event.ciphertext)? else {
            return None;
        };
        let acceptance = *self.by_id.get(&release.acceptance)?;
        let state = self.before(event)?;
        let accepted_before = self.before(acceptance)?;
        let canonical = self.canonical_before(event)?;
        if canonical.is_dissolved()
            || !canonical.is_accepted_member(&offer.recipient)
            || !canonical.is_accepted_member(&event.author.user_id())
        {
            return None;
        }
        if release.offer != offer_event.id || acceptance.kind != EventKind::ChannelAccept
            || !acceptance.ciphertext.is_empty() || acceptance.author.user_id() != offer.recipient
            || !accepted_before.is_member(&offer.recipient) || accepted_before.is_accepted_member(&offer.recipient)
            || !state.is_accepted_member(&offer.recipient) || state.is_dissolved()
            || !state.is_accepted_member(&event.author.user_id())
            || !offer.keys.entries.iter().any(|source| source.user_id == event.author.user_id())
            || !super::channel::descends_from(acceptance, offer.invitation, &self.by_id)
            || !super::channel::descends_from(event, acceptance.id, &self.by_id)
            || !super::channel::descends_from(event, offer_event.id, &self.by_id)
            // Removing/re-inviting a device creates a new permission lifetime.
            || self.lifetime_closed(offer, Some(event))
        {
            return None;
        }
        Some(release)
    }
    fn author(&self, event: &Event) -> Option<PublicIdentity> {
        self.before(event)?
            .members()
            .iter()
            .find(|member| member.user_id() == event.author.user_id())
            .cloned()
    }
}

/// Structural WAL validation. Full membership/policy authority remains checked
/// against signed ancestry by the channel projection.
pub(super) fn valid_invitation_transaction(events: &[Event]) -> bool {
    let Some(invitation) = events.first() else {
        return false;
    };
    if invitation.kind != EventKind::MembershipChange
        || !crate::channel::ChannelMeta::is_invitation(&invitation.ciphertext)
        || !crate::channel::ChannelMeta::decode(&invitation.ciphertext).is_some_and(|meta| {
            meta.owner == invitation.author.user_id() && meta.is_member(&meta.owner)
        })
    {
        return false;
    }
    match events {
        [_] => true,
        [_, event] => {
            event.kind == EventKind::ChannelJoinHistory
                && event.conversation_id == invitation.conversation_id
                && event.author == invitation.author
                && event.seq == invitation.seq.saturating_add(1)
                && event.parents == [invitation.id]
                && matches!(decode::<HistoryControl>(&event.ciphertext), Some(HistoryControl::Offer(offer)) if offer.invitation == invitation.id)
        }
        _ => false,
    }
}

pub(super) fn encode_policy(policy: HistoryPolicy) -> Vec<u8> {
    encode(&policy)
}

pub(super) fn policy(events: &[&Event]) -> HistoryPolicy {
    View::new(events).policy(None)
}

fn select_history(
    entries: Vec<HistoryEntry>,
    policy: HistoryPolicy,
    invitation: u64,
) -> Vec<HistoryEntry> {
    let mut bytes = 0usize;
    let mut selected: Vec<_> = entries
        .into_iter()
        .rev()
        .filter(|row| {
            row.file.is_none()
                && !row.recalled
                && row
                    .expires_at_ms
                    .is_none_or(|deadline| deadline > invitation)
                && policy.permits_time(row.wall_clock, invitation)
        })
        .filter(|row| {
            let next = bytes
                .saturating_add(row.text.len())
                .saturating_add(row.sticker.as_ref().map_or(0, String::len))
                .saturating_add(256);
            if next > SNAPSHOT_BYTES {
                return false;
            }
            bytes = next;
            true
        })
        .take(policy.count())
        .collect();
    selected.reverse();
    selected
}

impl Node {
    pub fn channel_history_policy(&self, channel: ConversationId) -> HistoryPolicy {
        let log = self.log.lock().expect("log lock");
        policy(&log.events(&channel))
    }
    pub async fn set_channel_history_policy(
        &self,
        channel: ConversationId,
        policy: HistoryPolicy,
    ) -> Result<(), NodeError> {
        self.set_channel_history_policy_if(channel, policy, |commit| commit())
            .await
    }
    pub async fn set_channel_history_policy_if(
        &self,
        channel: ConversationId,
        policy: HistoryPolicy,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut members = Vec::new();
        authorize(&mut || {
            {
                let book = self.channels.lock().expect("channels lock");
                let state = book
                    .state(&channel)
                    .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
                if state.owner() != self.user_id()
                    || !state.is_accepted_member(&self.user_id())
                    || state.is_dissolved()
                {
                    return Err(NodeError::Channel(
                        "only the active channel owner can change history policy".into(),
                    ));
                }
                members = state.members().to_vec();
            }
            self.append_event(channel, EventKind::ChannelHistoryPolicy, encode(&policy))?;
            self.process_channel(channel);
            Ok(())
        })?;
        self.distribute_channel(channel, &members).await;
        Ok(())
    }
    fn prepare_join_history(
        &self,
        channel: ConversationId,
        recipient: &PublicIdentity,
        selected: Vec<HistoryEntry>,
        policy: HistoryPolicy,
    ) -> Result<Option<Offer>, NodeError> {
        if selected.is_empty() || policy == HistoryPolicy::Off {
            return Ok(None);
        }
        let key = GroupKey::generate();
        let rows: Vec<_> = selected
            .into_iter()
            .map(|row| {
                let body = MessageBody {
                    text: row.text,
                    reply_to: row.reply_to,
                    sticker: row.sticker,
                }
                .encode();
                let body = crate::expiry::wrap(&body, row.expires_at_ms);
                crate::channel::seal_channel_message(&row_key(&key, row.id), &body)
                    .map(|ciphertext| EncryptedRow {
                        id: row.id,
                        ciphertext,
                    })
                    .map_err(|error| NodeError::Channel(error.to_string()))
            })
            .collect::<Result<_, _>>()?;
        let sources = {
            let book = self.channels.lock().expect("channels lock");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
            let mut sources: Vec<_> = state
                .members()
                .iter()
                .filter(|member| {
                    state.is_accepted_member(&member.user_id())
                        && member.user_id() != recipient.user_id()
                })
                .cloned()
                .collect();
            sources.sort_by_key(|member| (member.user_id() != self.user_id(), member.user_id()));
            sources.truncate(MAX_SOURCES);
            sources
        };
        // A later member may be accepted now but still lack authority for an
        // earlier row. Never give that member the snapshot root as a source.
        let sources = {
            let log = self.log.lock().expect("log lock");
            let refs = log.events(&channel);
            let view = View::new(&refs);
            let authorities: Vec<_> = rows
                .iter()
                .filter_map(|row| {
                    view.by_id.get(&row.id).and_then(|original| {
                        Some((view.before(original)?, view.canonical_before(original)?))
                    })
                })
                .collect();
            sources
                .into_iter()
                .filter(|source| {
                    source.user_id() == self.user_id()
                        || (authorities.len() == rows.len()
                            && authorities.iter().all(|(causal, canonical)| {
                                causal.is_accepted_member(&source.user_id())
                                    && canonical.is_accepted_member(&source.user_id())
                            }))
                })
                .collect::<Vec<_>>()
        };
        let offer = Offer {
            recipient: recipient.user_id(),
            invitation: EventId::new([0; 32]),
            policy,
            keys: SealedPayload::seal(&self.identity, &sources, key.as_bytes())
                .map_err(|e| NodeError::Channel(e.to_string()))?,
            rows,
        };
        Ok(Some(offer))
    }
    /// Reserve the invitation and its optional offer in the existing durable
    /// delivery journal together. A crash cannot commit an invitation while
    /// losing the frozen selection needed to complete its history permission.
    pub(super) fn append_invitation_with_history(
        &self,
        channel: ConversationId,
        meta: &crate::channel::ChannelMeta,
        recipient: &PublicIdentity,
        selected: Vec<HistoryEntry>,
        policy: HistoryPolicy,
    ) -> Result<(), NodeError> {
        let now = super::node::now_millis();
        let selected = select_history(selected, policy, now);
        let mut offer = self.prepare_join_history(channel, recipient, selected, policy)?;
        let mut delivery = self.delivery.lock().expect("delivery lock");
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        let book = self.channels.lock().expect("channels lock");
        let state = book
            .state(&channel)
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        if state.owner() != self.user_id()
            || state.is_dissolved()
            || !state.is_accepted_member(&self.user_id())
            || state.epoch().checked_add(1) != Some(meta.epoch)
            || state.owner() != meta.owner
        {
            return Err(NodeError::Channel(
                "channel authority changed before invitation".into(),
            ));
        }
        let senders = state.export_my_senders();
        let log = self.log.lock().expect("log lock");
        let seq = log
            .next_sequence(
                &channel,
                &crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub),
            )
            .map_err(NodeError::Log)?;
        let (parents, lamport) = log.prepare(&channel).map_err(NodeError::Log)?;
        let invitation = Event::new(
            &self.identity,
            channel,
            seq,
            parents,
            lamport,
            now,
            EventKind::MembershipChange,
            meta.encode_invitation(),
        );
        let mut events = vec![invitation];
        if let Some(mut offer) = offer.take() {
            offer.invitation = events[0].id;
            events.push(Event::new(
                &self.identity,
                channel,
                seq + 1,
                vec![events[0].id],
                lamport + 1,
                now,
                EventKind::ChannelJoinHistory,
                encode(&HistoryControl::Offer(offer)),
            ));
        }
        if events
            .iter()
            .any(|event| !super::session::event_fits_frame(event))
        {
            return Err(NodeError::Channel(
                "invitation history exceeds transport frame".into(),
            ));
        }
        log.sync().map_err(NodeError::Log)?;
        drop(log);
        let last_seq = events.last().expect("invitation exists").seq;
        delivery
            .begin(
                super::delivery_store::DeliveryTransaction::ChannelOutgoing {
                    events,
                    senders,
                    sent: super::sentlog::SentEntry {
                        conversation: channel,
                        seq: last_seq,
                        wall_clock: now,
                        plaintext: Vec::new(),
                    },
                },
            )
            .map_err(NodeError::Log)?;
        drop(book);
        self.recover_delivery(&mut delivery)
            .map_err(NodeError::Log)?;
        Ok(())
    }
    /// At most eight ready invitations are released per control tick. All bytes
    /// remain in the durable event log, so an offline source can retry on restart.
    pub(super) fn retry_join_history(&self) -> Result<Vec<ConversationId>, NodeError> {
        let mut channels = self.channels.lock().expect("channels lock").channel_ids();
        channels.sort_by_key(|channel| *channel.as_bytes());
        if !channels.is_empty() {
            let offset =
                ((super::node::now_millis() / 3000) as usize).wrapping_mul(32) % channels.len();
            channels.rotate_left(offset);
        }
        channels.truncate(32);
        let mut remaining = MAX_RETRIES;
        let mut ready = HashSet::new();
        for channel in channels {
            if !self.channel_replica_enabled(channel, &self.user_id()) {
                continue;
            }
            let events: Vec<_> = self
                .log
                .lock()
                .expect("log lock")
                .events(&channel)
                .into_iter()
                .cloned()
                .collect();
            let refs: Vec<_> = events.iter().collect();
            let view = View::new(&refs);
            let Some(current) = ChannelBook::projected_metadata(channel, &refs) else {
                continue;
            };
            if !current.is_accepted_member(&self.user_id()) || current.is_dissolved() {
                continue;
            }
            for (event, offer) in view.offers() {
                if remaining == 0 {
                    return Ok(ready.into_iter().collect());
                }
                if view.lifetime_closed(&offer, None) {
                    continue;
                }
                if !current.is_accepted_member(&offer.recipient) {
                    continue;
                }
                let releases: Vec<_> = view
                    .events
                    .iter()
                    .filter(|candidate| {
                        candidate.kind == EventKind::ChannelJoinHistory
                            && view.release(candidate, event, &offer).is_some()
                    })
                    .collect();
                if !releases.is_empty() {
                    if releases
                        .iter()
                        .any(|release| release.author.user_id() == self.user_id())
                    {
                        ready.insert(channel);
                    }
                    continue;
                }
                let Some(acceptance) = view.events.iter().find(|candidate| {
                    candidate.kind == EventKind::ChannelAccept
                        && candidate.author.user_id() == offer.recipient
                        && candidate.ciphertext.is_empty()
                        && view.before(candidate).is_some_and(|state| {
                            state.is_member(&offer.recipient)
                                && !state.is_accepted_member(&offer.recipient)
                        })
                        && super::channel::descends_from(candidate, offer.invitation, &view.by_id)
                }) else {
                    continue;
                };
                let Some(author) = view.author(event) else {
                    continue;
                };
                let Some(mut key) = offer.keys.open(&self.identity, &author.x25519_pub) else {
                    continue;
                };
                let root = <[u8; 32]>::try_from(key.as_slice())
                    .ok()
                    .map(GroupKey::from_bytes);
                key.zeroize();
                let Some(root) = root else {
                    continue;
                };
                let recalled = self.recalled_targets_channel(channel);
                let now = super::node::now_millis();
                let keys: Vec<_> = offer
                    .rows
                    .iter()
                    .filter_map(|row| {
                        if recalled.contains(&row.id) {
                            return None;
                        }
                        let key = row_key(&root, row.id);
                        let mut plaintext =
                            crate::channel::open_channel_message(&key, &row.ciphertext).ok()?;
                        let visible =
                            crate::expiry::split(&plaintext).is_some_and(|(deadline, _)| {
                                deadline.is_none_or(|deadline| deadline > now)
                            });
                        plaintext.zeroize();
                        visible.then_some(RowKey {
                            id: row.id,
                            key: *key.as_bytes(),
                        })
                    })
                    .collect();
                let key_bytes = zeroize::Zeroizing::new(encode(&keys));
                let Some(recipient) = current
                    .members()
                    .iter()
                    .find(|member| member.user_id() == offer.recipient)
                else {
                    continue;
                };
                let sealed_key = crate::dm::seal(&self.identity, &recipient.x25519_pub, &key_bytes)
                    .map_err(|e| NodeError::Channel(e.to_string()))?;
                self.append_event(
                    channel,
                    EventKind::ChannelJoinHistory,
                    encode(&HistoryControl::Release(Release {
                        offer: event.id,
                        acceptance: acceptance.id,
                        sealed_key,
                    })),
                )?;
                self.schedule_channel_recovery(channel);
                ready.insert(channel);
                remaining -= 1;
            }
        }
        Ok(ready.into_iter().collect())
    }
    pub(super) fn select_join_history(
        &self,
        channel: ConversationId,
        policy: HistoryPolicy,
        now: u64,
    ) -> Vec<HistoryEntry> {
        if policy == HistoryPolicy::Off {
            return Vec::new();
        }
        select_history(self.channel_history(channel, 500), policy, now)
    }
}

pub(super) fn received(
    me: &DeviceIdentity,
    channel: ConversationId,
    events: &[&Event],
) -> Vec<ReceivedChannelMessage> {
    let Some(current) = ChannelBook::projected_metadata(channel, events) else {
        return Vec::new();
    };
    let uid = me.user_id();
    if current.is_dissolved() || !current.is_accepted_member(&uid) {
        return Vec::new();
    }
    let view = View::new(events);
    let offers: HashMap<_, _> = view
        .offers()
        .into_iter()
        .map(|(event, offer)| (event.id, (event, offer)))
        .collect();
    let mut out = Vec::new();
    let mut emitted = HashSet::new();
    for event in &view.events {
        if event.kind != EventKind::ChannelJoinHistory {
            continue;
        }
        let Some(HistoryControl::Release(release)) = decode(&event.ciphertext) else {
            continue;
        };
        let Some((offer_event, offer)) = offers
            .get(&release.offer)
            .filter(|(_, offer)| offer.recipient == uid)
        else {
            continue;
        };
        if view.release(event, offer_event, offer).is_none() {
            continue;
        }
        let Some(author) = view.author(event) else {
            continue;
        };
        let Ok(mut key_bytes) = crate::dm::open(me, &author.x25519_pub, &release.sealed_key) else {
            continue;
        };
        let keys: Option<Vec<RowKey>> = decode(&key_bytes);
        key_bytes.zeroize();
        let Some(keys) = keys else {
            continue;
        };
        if keys.len() > offer.policy.count() {
            continue;
        }
        let mut rows = Vec::new();
        for key in keys {
            let Some(encrypted) = offer.rows.iter().find(|row| row.id == key.id) else {
                continue;
            };
            if let Ok(body) = crate::channel::open_channel_message(
                &GroupKey::from_bytes(key.key),
                &encrypted.ciphertext,
            ) {
                rows.push(SnapshotRow { id: key.id, body });
            }
        }
        let invitation = view.by_id[&offer.invitation];
        let mut batch = Vec::new();
        let mut invalid = false;
        for row in rows {
            let Some(original) = view.by_id.get(&row.id) else {
                invalid = true;
                break;
            };
            if original.kind != EventKind::Message
                || !offer
                    .policy
                    .permits_time(original.wall_clock, invitation.wall_clock)
                || !super::channel::descends_from(invitation, original.id, &view.by_id)
                || crate::expiry::split(&row.body).is_none()
            {
                invalid = true;
                break;
            }
            let body = MessageBody::decode(&row.body);
            batch.push(ReceivedChannelMessage {
                channel_id: channel,
                channel_name: current.name().to_string(),
                from: original.author.user_id(),
                text: body.text,
                reply_to: body.reply_to,
                event_id: row.id,
                wall_clock: original.wall_clock,
                wrapped: row.body,
                received_at: 0,
                arrival_kind: "history_sync",
                own_device: false,
                batch_id: None,
                batch_complete: true,
                batch_size: 1,
                reception_only: false,
            });
        }
        if !invalid {
            out.extend(batch.into_iter().filter(|row| emitted.insert(row.event_id)));
        }
    }
    out
}

#[cfg(test)]
#[path = "join_history_tests.rs"]
mod tests;

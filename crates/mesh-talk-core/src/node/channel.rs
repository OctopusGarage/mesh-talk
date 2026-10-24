//! Channel orchestration over the event log: the per-member sealed
//! sender-key-distribution payload carried by `KeyRotation` events, the payload
//! builders for channel events, and a [`ChannelBook`] that replays a
//! channel's events into per-channel state + decrypted messages. Operates on
//! `Event`s — no live network.

use crate::channel::sender_key::SenderKeyDistribution;
use crate::channel::{ChannelMeta, ChannelState};
use crate::eventlog::event::{Author, ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::message::MessageBody;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// The `KeyRotation` event payload: the author's epoch sender-key distribution
/// sealed to each member. `sender` is the author's user id (whose chain the SKD
/// seeds); each `(user_id, sealed)` is `dm::seal(author, member.x25519, skd_bytes)`,
/// so only that member can open its entry. The post office relays it but never reads
/// it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeys {
    pub epoch: u64,
    pub sender: String,
    pub entries: Vec<SealedKeyEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedKeyEntry {
    pub user_id: String,
    pub sealed: Vec<u8>,
}

impl SealedKeys {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("sealed keys serialize")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// Seal `author`'s epoch sender-key distribution to each member's X25519, producing
/// the `KeyRotation` payload. Members open their entry to follow `author`'s chain
/// from `n = 0`. `author` is the sender whose static key the recipients use to open.
pub fn seal_keys_for(
    author: &DeviceIdentity,
    members: &[PublicIdentity],
    skd: &SenderKeyDistribution,
    epoch: u64,
) -> Result<SealedKeys, crate::channel::ChannelError> {
    let skd_bytes = skd.encode();
    let mut entries = Vec::with_capacity(members.len());
    for member in members {
        let sealed = crate::dm::seal(author, &member.x25519_pub, &skd_bytes)
            .map_err(crate::channel::ChannelError::Seal)?;
        entries.push(SealedKeyEntry {
            user_id: member.user_id(),
            sealed,
        });
    }
    Ok(SealedKeys {
        epoch,
        sender: author.public().user_id(),
        entries,
    })
}

/// Open the sender-key distribution sealed to `me` in a `SealedKeys`, using the
/// rotation author's X25519 (looked up from the channel membership). `None` if there
/// is no entry for us or it fails to open/decode.
fn open_my_skd(
    me: &DeviceIdentity,
    author_x25519: &[u8; 32],
    sealed: &SealedKeys,
) -> Option<SenderKeyDistribution> {
    let my_uid = me.public().user_id();
    let entry = sealed.entries.iter().find(|e| e.user_id == my_uid)?;
    let bytes = crate::dm::open(me, author_x25519, &entry.sealed).ok()?;
    SenderKeyDistribution::decode(&bytes)
}

/// A channel payload (e.g. a reaction) sealed per-member with the DM sealed box: the
/// author's bytes sealed to each member's X25519, so any member can open their entry.
/// Used for content that must be RE-READABLE by all members (no forward secrecy) —
/// unlike channel messages, which use single-use sender keys. The post office relays
/// it but never reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedPayload {
    pub sender: String,
    pub entries: Vec<SealedKeyEntry>,
}

impl SealedPayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("sealed payload serialize")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// Seal `plaintext` to each member's X25519. `author` is the sender whose static
    /// key the recipients use to open.
    pub fn seal(
        author: &DeviceIdentity,
        members: &[PublicIdentity],
        plaintext: &[u8],
    ) -> Result<Self, crate::channel::ChannelError> {
        let mut entries = Vec::with_capacity(members.len());
        for member in members {
            let sealed = crate::dm::seal(author, &member.x25519_pub, plaintext)
                .map_err(crate::channel::ChannelError::Seal)?;
            entries.push(SealedKeyEntry {
                user_id: member.user_id(),
                sealed,
            });
        }
        Ok(SealedPayload {
            sender: author.public().user_id(),
            entries,
        })
    }

    /// Open the bytes sealed to `me`, using the author's X25519. `None` if there is no
    /// entry for us or it fails to open.
    pub fn open(&self, me: &DeviceIdentity, author_x25519: &[u8; 32]) -> Option<Vec<u8>> {
        let my_uid = me.public().user_id();
        let entry = self.entries.iter().find(|e| e.user_id == my_uid)?;
        crate::dm::open(me, author_x25519, &entry.sealed).ok()
    }
}

pub(in crate::node) const CHANNEL_PENDING_BYTES: usize = 8 * 1024 * 1024;

/// A decrypted channel message surfaced to the application. Carries the source
/// event's id + wall-clock and the decoded plaintext body so the node can persist it
/// to the received store (sender-key wire keys are single-use, so history is served
/// from the store, not by re-opening the event).
#[derive(Clone, PartialEq, Eq)]
pub struct ReceivedChannelMessage {
    pub channel_id: ConversationId,
    pub channel_name: String,
    pub from: String,
    pub text: Vec<u8>,
    pub protection: Option<super::MessageProtection>,
    pub reply_to: Option<EventId>,
    pub event_id: EventId,
    pub wall_clock: u64,
    /// The wrapped `MessageBody` plaintext (text + reply_to), for the received store.
    pub(in crate::node) wrapped: Vec<u8>,
    /// Local first successful decryption observation; not persisted in signed events.
    pub received_at: u64,
    pub arrival_kind: &'static str,
    pub own_device: bool,
    pub batch_id: Option<String>,
    pub batch_complete: bool,
    pub batch_size: usize,
    /// Metadata-only batch terminal; never render as a message or count unread.
    pub reception_only: bool,
}

impl std::fmt::Debug for ReceivedChannelMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReceivedChannelMessage")
            .field("event_id", &self.event_id)
            .field("protection", &self.protection)
            .finish_non_exhaustive()
    }
}

impl ReceivedChannelMessage {
    pub(in crate::node) fn terminal(&self, batch_id: Option<String>, batch_size: usize) -> Self {
        Self {
            channel_id: self.channel_id,
            channel_name: self.channel_name.clone(),
            from: self.from.clone(),
            text: Vec::new(),
            protection: None,
            reply_to: None,
            event_id: self.event_id,
            wall_clock: self.wall_clock,
            wrapped: Vec::new(),
            received_at: self.received_at,
            arrival_kind: "catch_up",
            own_device: self.own_device,
            batch_id,
            batch_complete: true,
            batch_size,
            reception_only: true,
        }
    }

    pub(in crate::node) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.wrapped.capacity()
            + self.text.capacity()
            + self.from.capacity()
            + self.channel_name.capacity()
            + self.batch_id.as_ref().map_or(0, String::capacity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelActivity {
    pub kind: &'static str,
    pub actor: String,
    pub target: Option<String>,
    pub wall_clock: u64,
}

/// A node's view of all the channels it knows: each channel's [`ChannelState`]
/// keyed by channel id. [`ChannelBook::process`] replays a channel's events (in the
/// causal order the log returns them) to update membership/keys and surface
/// decrypted messages.
#[derive(Default)]
pub struct ChannelBook {
    states: HashMap<ConversationId, ChannelState>,
    emitted: HashSet<EventId>,
    applied_leaves: HashSet<EventId>,
    applied_transfers: HashSet<EventId>,
    transferred: HashSet<(ConversationId, String)>,
    departed: HashSet<(ConversationId, String)>,
    activities: HashMap<ConversationId, Vec<ChannelActivity>>,
    activity_emitted: HashSet<EventId>,
    lineage_owner: HashMap<ConversationId, String>,
    history_policies: HashMap<ConversationId, crate::channel::HistoryPolicy>,
    replica_preferences: HashMap<(ConversationId, String), (u64, EventId, bool)>,
}

/// One validated snapshot traversal, resumed across bounded plaintext batches.
/// Ordering and ancestry indices are built once, independent of batch count.
pub(in crate::node) struct ChannelReplay<'a> {
    events: Vec<&'a Event>,
    by_id: HashMap<EventId, &'a Event>,
    cursor: usize,
    after_accept: HashSet<EventId>,
    dissolved: bool,
    history: Option<std::collections::VecDeque<ReceivedChannelMessage>>,
}

impl<'a> ChannelReplay<'a> {
    pub(in crate::node) fn new(events: &[&'a Event]) -> Self {
        let by_id: HashMap<_, _> = events.iter().map(|event| (event.id, *event)).collect();
        let events = super::timeline::ordered_ids(events, &[], true)
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        Self {
            events,
            by_id,
            cursor: 0,
            after_accept: HashSet::new(),
            dissolved: false,
            history: None,
        }
    }
}

/// A Lamport timestamp cannot establish whether an owner observed a leave: an
/// unrelated local event can make a concurrent snapshot look newer. Follow the
/// signed event DAG instead when deciding whether a snapshot may re-invite them.
pub(super) fn descends_from(
    event: &Event,
    ancestor: EventId,
    by_id: &HashMap<EventId, &Event>,
) -> bool {
    let mut pending = event.parents.clone();
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if id == ancestor {
            return true;
        }
        if seen.insert(id)
            && let Some(parent) = by_id.get(&id)
        {
            pending.extend(&parent.parents);
        }
    }
    false
}

/// Presentation-relevant accepted projection, excluding sender chains and message state.
#[derive(PartialEq, Eq)]
pub(in crate::node) struct ChannelInfoProjection {
    name: String,
    owner: String,
    dissolved: bool,
    pending: bool,
    members: Vec<(PublicIdentity, bool, bool)>,
    history_policy: crate::channel::HistoryPolicy,
}

pub(super) struct ChannelMembershipProjection {
    pub state: Option<ChannelState>,
    pub departures: Vec<(String, EventId)>,
    pub authority: super::channel_account_authority::AccountAuthority,
    pub accepted: Vec<super::channel_account_authority::LegacyAccountMember>,
    pub authorized: HashSet<EventId>,
    pub invited: Vec<super::channel_account_authority::LegacyAccountMember>,
}

impl ChannelBook {
    pub(in crate::node) fn info_projection(
        &self,
        channel: ConversationId,
    ) -> Option<ChannelInfoProjection> {
        let state = self.state(&channel)?;
        Some(ChannelInfoProjection {
            history_policy: self
                .history_policies
                .get(&channel)
                .copied()
                .unwrap_or_default(),
            name: state.name().to_owned(),
            owner: state.owner().to_owned(),
            dissolved: state.is_dissolved(),
            pending: state.pending_invite(),
            members: state
                .members()
                .iter()
                .map(|member| {
                    let id = member.user_id();
                    (
                        member.clone(),
                        state.is_accepted_member(&id),
                        self.replica_enabled(channel, &id),
                    )
                })
                .collect(),
        })
    }
    /// Missing preference means the member participates by default.
    pub fn replica_enabled(&self, channel: ConversationId, member: &str) -> bool {
        self.replica_preferences
            .get(&(channel, member.to_string()))
            .is_none_or(|(_, _, enabled)| *enabled)
    }
    /// Replay signed channel metadata without opening sender keys or messages.
    /// Late-invite reconstruction, admission and archived summaries share these
    /// membership/ownership/terminal-state rules.
    pub(super) fn membership_view(
        channel_id: ConversationId,
        events: &[&Event],
        before: Option<EventId>,
        by_id: &HashMap<EventId, &Event>,
    ) -> (Option<ChannelState>, Vec<(String, EventId)>) {
        let projection = Self::membership_projection(channel_id, events, before, by_id);
        (projection.state, projection.departures)
    }

    pub(super) fn membership_projection(
        channel_id: ConversationId,
        events: &[&Event],
        before: Option<EventId>,
        by_id: &HashMap<EventId, &Event>,
    ) -> ChannelMembershipProjection {
        use super::channel_account_authority::{
            AccountAuthority, LegacyAccountMember, LegacyAuthorityContext,
        };
        use super::channel_account_bootstrap::DeviceControl;
        let mut authority = AccountAuthority::default();
        let mut authorized = HashSet::new();
        let mut bindings: HashMap<String, crate::discovery::Announce> = HashMap::new();
        let mut roots: HashMap<String, (PublicIdentity, EventId, Option<EventId>, bool)> =
            HashMap::new();
        let mut prior: Option<ChannelState> = None;
        let mut departures: Vec<(String, EventId)> = Vec::new();
        for event in events {
            if before == Some(event.id) {
                break;
            }
            if prior.as_ref().is_some_and(ChannelState::is_dissolved) {
                break;
            }
            let author = event.author.user_id();
            let old_members: HashSet<_> = prior
                .as_ref()
                .map(|state| {
                    state
                        .members()
                        .iter()
                        .map(PublicIdentity::user_id)
                        .collect()
                })
                .unwrap_or_default();
            let was_accepted = prior
                .as_ref()
                .is_some_and(|state| state.is_accepted_member(&author));
            let mut valid = was_accepted
                && !matches!(
                    event.kind,
                    EventKind::MembershipChange
                        | EventKind::ChannelRename
                        | EventKind::ChannelDissolve
                        | EventKind::ChannelAccept
                        | EventKind::ChannelLeave
                        | EventKind::ChannelOwnerTransfer
                        | EventKind::ChannelAccountAuthority
                        | EventKind::ChannelDeviceInheritance
                );
            match event.kind {
                EventKind::MembershipChange => {
                    let Some(mut meta) = ChannelMeta::decode(&event.ciphertext) else {
                        continue;
                    };
                    let original_member_count = meta.members.len();
                    meta.members.retain(|member| {
                        !departures.iter().any(|(uid, leave_id)| {
                            uid == &member.user_id() && !descends_from(event, *leave_id, by_id)
                        })
                    });
                    if let Some(state) = prior.as_mut() {
                        if !state.can_admin(&author) {
                            continue;
                        }
                        valid = true;
                        let old: HashSet<_> = state
                            .members()
                            .iter()
                            .map(PublicIdentity::user_id)
                            .collect();
                        let invited = ChannelMeta::is_invitation(&event.ciphertext);
                        if meta.members.len() < original_member_count {
                            state.apply_meta_after_concurrent_leave(meta);
                        } else {
                            state.apply_meta(meta);
                        }
                        let added: Vec<_> = state
                            .members()
                            .iter()
                            .map(PublicIdentity::user_id)
                            .filter(|uid| !old.contains(uid))
                            .collect();
                        for uid in added {
                            if invited {
                                state.mark_invited(&uid);
                            } else {
                                state.auto_accept_legacy_member(&uid);
                            }
                        }
                    } else if event.parents.is_empty()
                        && author == meta.owner
                        && meta.is_member(&author)
                    {
                        valid = true;
                        let invited = ChannelMeta::is_invitation(&event.ciphertext);
                        let mut state = ChannelState::from_meta(channel_id, meta);
                        if invited {
                            let invitees: Vec<_> = state
                                .members()
                                .iter()
                                .map(PublicIdentity::user_id)
                                .filter(|uid| uid != &author)
                                .collect();
                            for uid in invitees {
                                state.mark_invited(&uid);
                            }
                        }
                        prior = Some(state);
                    }
                }
                EventKind::ChannelRename => {
                    if let Some(state) = prior.as_mut()
                        && state.can_admin(&author)
                        && let Ok(name) = std::str::from_utf8(&event.ciphertext)
                    {
                        valid = true;
                        state.apply_rename(name, event.lamport, event.id);
                    }
                }
                EventKind::ChannelDissolve => {
                    if event.ciphertext.is_empty()
                        && let Some(state) = prior.as_mut()
                        && state.can_admin(&author)
                    {
                        valid = true;
                        state.dissolve();
                    }
                }
                EventKind::ChannelAccept => {
                    if authority.current_event().is_some() {
                        let proof = bindings.get(&author).cloned().or_else(|| {
                            authority
                                .original_device_proofs()
                                .into_iter()
                                .chain(authority.invited_device_proofs())
                                .find(|proof| proof.user_id == author)
                        });
                        if let Some(proof) = proof
                            && (authority.is_member(&proof.public(), &proof)
                                || authority
                                    .invitation_for_device(&proof.public(), &proof)
                                    .is_some())
                        {
                            valid = authority.accept_account(event, &proof, by_id).is_ok();
                        }
                    } else if event.ciphertext.is_empty()
                        && let Some(state) = prior.as_mut()
                    {
                        valid = state.is_member(&author);
                        state.accept_invite(&author, event.id);
                    }
                }
                EventKind::ChannelOwnerTransfer => {
                    if authority.current_event().is_some() {
                        continue;
                    }
                    if let Some(state) = prior.as_mut()
                        && state.can_admin(&author)
                        && let Ok(next) = std::str::from_utf8(&event.ciphertext)
                    {
                        valid = state.transfer_owner(next);
                    }
                }
                EventKind::ChannelLeave => {
                    if authority.current_event().is_some() {
                        let proof = bindings.get(&author).cloned().or_else(|| {
                            authority
                                .original_device_proofs()
                                .into_iter()
                                .find(|proof| proof.user_id == author)
                        });
                        if let Some(proof) = proof
                            && authority.is_member(&proof.public(), &proof)
                        {
                            valid = authority.leave_account(event, &proof, by_id).is_ok();
                        }
                    } else if event.ciphertext.is_empty()
                        && let Some(state) = prior.as_mut()
                        && state.leave_member(&author)
                    {
                        valid = true;
                        departures.push((author.clone(), event.id));
                    }
                }
                EventKind::ChannelAccountAuthority => {
                    valid = false;
                    if let Some(state) = prior.as_ref() {
                        let issuer =
                            super::channel_account_authority::AccountAuthorityControl::decode(
                                &event.ciphertext,
                            )
                            .map(|control| control.issuer.public());
                        if let Some(owner) = state
                            .members()
                            .iter()
                            .find(|member| member.user_id() == state.owner())
                            .cloned()
                            .or(issuer)
                        {
                            let accepted = roots
                                .values()
                                .filter(|(_, _, _, accepted)| *accepted)
                                .map(|(public, membership_event, acceptance_event, _)| {
                                    LegacyAccountMember {
                                        public: public.clone(),
                                        membership_event: *membership_event,
                                        acceptance_event: *acceptance_event,
                                    }
                                })
                                .collect();
                            let context = LegacyAuthorityContext {
                                channel: channel_id,
                                owner,
                                accepted,
                                invited: roots
                                    .values()
                                    .filter(|(_, _, _, accepted)| !*accepted)
                                    .map(|(public, membership_event, acceptance_event, _)| {
                                        LegacyAccountMember {
                                            public: public.clone(),
                                            membership_event: *membership_event,
                                            acceptance_event: *acceptance_event,
                                        }
                                    })
                                    .collect(),
                                epoch: state.epoch(),
                            };
                            valid = authority.apply(event, &context, by_id).is_ok();
                        }
                    }
                }
                EventKind::ChannelDeviceInheritance => {
                    valid = false;
                    if let Some((_, _, proof)) = DeviceControl::signed_inheritance(event)
                        && authority.inherit_device(event, by_id).is_ok()
                    {
                        valid = true;
                        bindings.insert(proof.user_id.clone(), proof);
                    } else if let Some(state) = prior.as_ref() {
                        valid = super::channel_account_bootstrap::receive_keys_admitted(
                            state, event, &authority, by_id,
                        ) || super::join_history::account_release_admitted(
                            state, event, &authority, by_id,
                        );
                    }
                }
                _ => {}
            }
            if valid
                && event
                    .parents
                    .iter()
                    .all(|parent| authorized.contains(parent))
            {
                authorized.insert(event.id);
            }
            if let Some(state) = prior.as_ref() {
                if event.kind == EventKind::MembershipChange {
                    roots.retain(|uid, _| state.is_member(uid));
                    for member in state.members() {
                        if !old_members.contains(&member.user_id()) {
                            roots.insert(
                                member.user_id(),
                                (
                                    member.clone(),
                                    event.id,
                                    None,
                                    state.is_accepted_member(&member.user_id()),
                                ),
                            );
                        }
                    }
                }
                if event.kind == EventKind::ChannelAccept
                    && !was_accepted
                    && state.is_accepted_member(&author)
                    && let Some(root) = roots.get_mut(&author)
                {
                    root.2 = Some(event.id);
                    root.3 = true;
                }
            }
            if let Some(state) = prior.as_mut()
                && let Some(owner) = authority.owner_account_id()
                && let Some(epoch) = authority.key_epoch()
            {
                let mut proofs = authority.original_device_proofs();
                proofs.extend(authority.grants().into_iter().map(|grant| grant.original));
                proofs.extend(bindings.values().cloned());
                let participants = proofs
                    .into_iter()
                    .filter(|proof| authority.is_member(&proof.public(), proof))
                    .filter_map(|proof| proof.account_id().map(|account| (proof.public(), account)))
                    .collect();
                state.install_account_projection(owner, participants, epoch);
                let mut invites = authority.invited_device_proofs();
                invites.extend(
                    bindings
                        .values()
                        .filter(|proof| {
                            authority
                                .invitation_for_device(&proof.public(), proof)
                                .is_some()
                        })
                        .cloned(),
                );
                state.install_account_invites(
                    invites
                        .into_iter()
                        .filter_map(|proof| {
                            proof.account_id().map(|account| (proof.public(), account))
                        })
                        .collect(),
                );
            }
        }
        let invited = roots
            .values()
            .filter(|(_, _, _, accepted)| !*accepted)
            .map(
                |(public, membership_event, acceptance_event, _)| LegacyAccountMember {
                    public: public.clone(),
                    membership_event: *membership_event,
                    acceptance_event: *acceptance_event,
                },
            )
            .collect();
        let accepted = roots
            .into_values()
            .filter(|(_, _, _, accepted)| *accepted)
            .map(
                |(public, membership_event, acceptance_event, _)| LegacyAccountMember {
                    public,
                    membership_event,
                    acceptance_event,
                },
            )
            .collect();
        ChannelMembershipProjection {
            state: prior,
            departures,
            authority,
            accepted,
            authorized,
            invited,
        }
    }

    pub(super) fn project_membership(
        channel_id: ConversationId,
        events: &[&Event],
    ) -> ChannelMembershipProjection {
        let by_id: HashMap<_, _> = events.iter().map(|event| (event.id, *event)).collect();
        let order = super::timeline::ordered_ids(events, &[], true);
        let ordered: Vec<_> = order
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        Self::membership_projection(channel_id, &ordered, None, &by_id)
    }

    /// Channel metadata from the validated event DAG. This intentionally creates
    /// no sender/receiver chains and can project a recovery archive for a device
    /// which is not itself a member.
    pub(super) fn projected_metadata(
        channel_id: ConversationId,
        events: &[&Event],
    ) -> Option<ChannelState> {
        Self::project_membership(channel_id, events).state
    }

    /// Current members from the validated event DAG, without decrypting or
    /// surfacing message content. Used for connection admission decisions.
    pub fn projected_members(
        channel_id: ConversationId,
        events: &[&Event],
    ) -> Option<Vec<PublicIdentity>> {
        Self::projected_metadata(channel_id, events).map(|state| state.members().to_vec())
    }

    pub fn new() -> Self {
        ChannelBook::default()
    }

    /// The state for a known channel, if any.
    pub fn state(&self, channel_id: &ConversationId) -> Option<&ChannelState> {
        self.states.get(channel_id)
    }

    /// Mutable state for a known channel (needed to seal/open with sender keys, which
    /// ratchet the chain).
    pub fn state_mut(&mut self, channel_id: &ConversationId) -> Option<&mut ChannelState> {
        self.states.get_mut(channel_id)
    }

    /// The ids of all channels this book knows.
    pub fn channel_ids(&self) -> Vec<crate::eventlog::event::ConversationId> {
        self.states.keys().copied().collect()
    }

    pub fn activity(&self, channel_id: &ConversationId) -> Vec<ChannelActivity> {
        self.activities.get(channel_id).cloned().unwrap_or_default()
    }

    /// Only the departing node may dial a current member to finish its own
    /// departure sync. A remaining member must not admit a former member after
    /// processing the leave, which would reveal signed presence indefinitely.
    pub fn departure_link(&self, own: &str, peer: &str) -> bool {
        self.states.iter().any(|(id, state)| {
            !state.is_member(own)
                && state.is_member(peer)
                && self.departed.contains(&(*id, own.to_string()))
        })
    }

    pub fn pending_departures(&self, own: &str) -> Vec<(ConversationId, Vec<PublicIdentity>)> {
        self.states
            .iter()
            .filter(|(id, state)| {
                !state.is_member(own) && self.departed.contains(&(**id, own.to_string()))
            })
            .map(|(id, state)| (*id, state.members().to_vec()))
            .collect()
    }

    pub fn pending_acceptances(&self, own: &str) -> Vec<(ConversationId, Vec<PublicIdentity>)> {
        self.states
            .iter()
            .filter(|(_, state)| {
                state.is_accepted_member(own) && state.accepted_event_id().is_some()
            })
            .map(|(id, state)| (*id, state.members().to_vec()))
            .collect()
    }

    pub fn pending_transfers(&self, own: &str) -> Vec<(ConversationId, Vec<PublicIdentity>)> {
        self.states
            .iter()
            .filter(|(id, state)| {
                state.is_member(own)
                    && state.owner() != own
                    && self.transferred.contains(&(**id, own.to_string()))
            })
            .map(|(id, state)| (*id, state.members().to_vec()))
            .collect()
    }

    pub fn pending_replica_preferences(
        &self,
        own: &str,
    ) -> Vec<(ConversationId, Vec<PublicIdentity>)> {
        self.states
            .iter()
            .filter(|(id, state)| {
                state.is_accepted_member(own)
                    && self
                        .replica_preferences
                        .contains_key(&(**id, own.to_string()))
            })
            .map(|(id, state)| (*id, state.members().to_vec()))
            .collect()
    }

    /// Replay `events` (a channel's events, in log/causal order) for the node `me`.
    /// Updates membership + keys and returns the channel `Message`s that are newly
    /// decryptable and authored by someone other than `me`. Events we can't act on
    /// (a key we don't have, a membership we're not in) are skipped.
    ///
    /// Call with the channel's FULL event log, not a delta: an event skipped because
    /// its prerequisite hasn't been applied yet (e.g. a `KeyRotation` seen before its
    /// `MembershipChange` under out-of-order delivery) is recovered on the next
    /// full-log replay, where the prerequisite is processed first. The caller dedups
    /// the returned messages across calls (the live node uses an emitted-id set).
    #[cfg(test)]
    pub fn process(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        events: &[&Event],
    ) -> Vec<ReceivedChannelMessage> {
        self.process_limited(me, channel_id, events, usize::MAX)
    }

    /// The node bounds decrypted plaintext awaiting durable projection. Replaying
    /// later skips emitted IDs and resumes messages left encrypted by this limit.
    #[cfg(test)]
    pub(in crate::node) fn process_limited(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        events: &[&Event],
        limit: usize,
    ) -> Vec<ReceivedChannelMessage> {
        self.process_bounded(me, channel_id, events, limit, CHANNEL_PENDING_BYTES)
            .0
    }

    #[cfg(test)]
    pub(in crate::node) fn process_bounded(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        events: &[&Event],
        limit: usize,
        byte_budget: usize,
    ) -> (Vec<ReceivedChannelMessage>, bool) {
        self.process_replay(
            me,
            channel_id,
            &mut ChannelReplay::new(events),
            limit,
            byte_budget,
        )
    }

    pub(in crate::node) fn process_replay(
        &mut self,
        me: &DeviceIdentity,
        channel_id: ConversationId,
        replay: &mut ChannelReplay<'_>,
        limit: usize,
        byte_budget: usize,
    ) -> (Vec<ReceivedChannelMessage>, bool) {
        let my_author = Author::from_ed25519(me.public().ed25519_pub);
        if replay.history.is_none() {
            self.history_policies
                .insert(channel_id, super::join_history::policy(&replay.events));
            replay.history =
                Some(super::join_history::received(me, channel_id, &replay.events).into());
        }
        let ChannelReplay {
            events,
            by_id,
            cursor,
            after_accept,
            dissolved,
            history,
        } = replay;
        let events = events.as_slice();
        let by_id: &HashMap<EventId, &Event> = by_id;
        let mut out = Vec::new();
        let mut retained_upper_bound = 0usize;
        let mut limited = false;
        while *cursor < events.len() {
            let event = events[*cursor];
            if out.len() >= limit {
                limited = true;
                break;
            }
            *cursor += 1;
            if *dissolved {
                continue;
            }
            if let Some(accepted) = self
                .states
                .get(&channel_id)
                .and_then(ChannelState::accepted_event_id)
                && (event.id == accepted
                    || event
                        .parents
                        .iter()
                        .any(|parent| after_accept.contains(parent)))
            {
                after_accept.insert(event.id);
            }
            let upgraded_control = matches!(
                event.kind,
                EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
            ) || (self
                .states
                .get(&channel_id)
                .is_some_and(|state| state.owner_account_id().is_some())
                && matches!(
                    event.kind,
                    EventKind::MembershipChange
                        | EventKind::ChannelAccept
                        | EventKind::ChannelLeave
                        | EventKind::ChannelOwnerTransfer
                        | EventKind::ChannelRename
                        | EventKind::ChannelDissolve
                ));
            if upgraded_control {
                let projection = Self::membership_projection(
                    channel_id,
                    events,
                    events.get(*cursor).map(|event| event.id),
                    by_id,
                );
                if let Some(mut projected) = projection.state
                    && (projected.is_member(&me.public().user_id())
                        || self.states.contains_key(&channel_id))
                {
                    if let Some(state) = self.states.get_mut(&channel_id) {
                        state.install_membership_projection(projected);
                    } else {
                        projected.set_identity(me.public().user_id());
                        self.states.insert(channel_id, projected);
                    }
                    if let Some(state) = self.states.get_mut(&channel_id) {
                        super::channel_account_bootstrap::apply_receive_keys(
                            me,
                            state,
                            event,
                            &projection.authority,
                            by_id,
                        );
                    }
                }
                continue;
            }
            let mut activity: Option<(&'static str, Option<String>)> = None;
            match event.kind {
                EventKind::ChannelDissolve => {
                    if event.ciphertext.is_empty()
                        && let Some(state) = self.states.get_mut(&channel_id)
                        && event.author.user_id() == state.owner()
                    {
                        state.dissolve();
                        *dissolved = true;
                    }
                }
                EventKind::MembershipChange => {
                    if let Some(mut meta) = ChannelMeta::decode(&event.ciphertext) {
                        let author_uid = event.author.user_id();
                        let prior_view = (!self.states.contains_key(&channel_id)).then(|| {
                            Self::membership_view(channel_id, events, Some(event.id), by_id)
                        });
                        let prior_pending: Vec<_> = prior_view
                            .as_ref()
                            .and_then(|(state, _)| state.as_ref())
                            .map(|state| {
                                state
                                    .members()
                                    .iter()
                                    .map(PublicIdentity::user_id)
                                    .filter(|uid| !state.is_accepted_member(uid))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let original_member_count = meta.members.len();
                        meta.members.retain(|member| {
                            let departed_before_join =
                                prior_view.as_ref().is_some_and(|(_, leaves)| {
                                    leaves.iter().any(|(uid, leave_id)| {
                                        uid == &member.user_id()
                                            && !descends_from(event, *leave_id, by_id)
                                    })
                                });
                            !departed_before_join
                                && !events.iter().any(|leave| {
                                    leave.kind == EventKind::ChannelLeave
                                        && self.applied_leaves.contains(&leave.id)
                                        && leave.author.user_id() == member.user_id()
                                        && !descends_from(event, leave.id, by_id)
                                })
                        });
                        if event.parents.is_empty()
                            && author_uid == meta.owner
                            && meta.is_member(&author_uid)
                        {
                            self.lineage_owner
                                .entry(channel_id)
                                .or_insert_with(|| author_uid.clone());
                        }
                        match self.states.get_mut(&channel_id) {
                            Some(state) => {
                                // Only the current owner may change membership. A
                                // snapshot cannot transfer ownership.
                                if author_uid == state.owner() {
                                    let prior_members: HashSet<_> = state
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .collect();
                                    let invited = ChannelMeta::is_invitation(&event.ciphertext);
                                    if meta.members.len() < original_member_count {
                                        state.apply_meta_after_concurrent_leave(meta);
                                    } else {
                                        state.apply_meta(meta);
                                    }
                                    let after: HashSet<_> = state
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .collect();
                                    if let Some(added) = after.difference(&prior_members).next() {
                                        activity = Some(("joined", Some(added.clone())));
                                    } else if let Some(removed) =
                                        prior_members.difference(&after).next()
                                    {
                                        activity = Some(("removed", Some(removed.clone())));
                                    }
                                    let added: Vec<_> = state
                                        .members()
                                        .iter()
                                        .map(PublicIdentity::user_id)
                                        .filter(|uid| !prior_members.contains(uid))
                                        .collect();
                                    for uid in added {
                                        if invited {
                                            state.mark_invited(&uid);
                                        } else {
                                            state.auto_accept_legacy_member(&uid);
                                        }
                                    }
                                }
                            }
                            None => {
                                // First time we hear of this channel: an invite. Trust-on-first-
                                // use — accept only if it includes US, the author is the owner
                                // the snapshot declares, and that owner is itself a member. This
                                // genesis snapshot pins the owner for all later changes.
                                if meta.is_member(&me.public().user_id())
                                    && author_uid == meta.owner
                                    && meta.is_member(&meta.owner)
                                {
                                    let invited = ChannelMeta::is_invitation(&event.ciphertext);
                                    let is_genesis = meta.epoch == 0;
                                    let mut state = ChannelState::from_meta(channel_id, meta);
                                    if let Some(origin) = self.lineage_owner.get(&channel_id) {
                                        state.set_genesis_owner(origin.clone());
                                    }
                                    state.set_identity(me.public().user_id());
                                    if invited {
                                        let invitees: Vec<_> = if is_genesis {
                                            state
                                                .members()
                                                .iter()
                                                .map(PublicIdentity::user_id)
                                                .filter(|uid| uid != &author_uid)
                                                .collect()
                                        } else {
                                            vec![me.public().user_id()]
                                        };
                                        for uid in invitees {
                                            state.mark_invited(&uid);
                                        }
                                    }
                                    if !is_genesis {
                                        for uid in &prior_pending {
                                            if state.is_member(uid) {
                                                state.mark_invited(uid);
                                            }
                                        }
                                    }
                                    self.states.insert(channel_id, state);
                                    if let Some((_, leaves)) = &prior_view {
                                        for (uid, leave_id) in leaves {
                                            self.applied_leaves.insert(*leave_id);
                                            self.departed.insert((channel_id, uid.clone()));
                                        }
                                    }
                                    activity = Some(if is_genesis {
                                        ("created", None)
                                    } else {
                                        ("joined", Some(me.public().user_id()))
                                    });
                                }
                            }
                        }
                    }
                }
                EventKind::ChannelRename => {
                    // Owner-only, like membership: a non-owner's rename is dropped by every
                    // node. Applied last-writer-wins by (lamport, id), independent of the
                    // membership epoch.
                    if let Some(state) = self.states.get_mut(&channel_id)
                        && event.author.user_id() == state.owner()
                        && let Ok(name) = std::str::from_utf8(&event.ciphertext)
                    {
                        state.apply_rename(name, event.lamport, event.id);
                        activity = Some(("renamed", None));
                    }
                }
                EventKind::ChannelOwnerTransfer => {
                    if !self.applied_transfers.contains(&event.id)
                        && let Some(state) = self.states.get_mut(&channel_id)
                        && event.author.user_id() == state.owner()
                        && let Ok(next) = std::str::from_utf8(&event.ciphertext)
                        && state.transfer_owner(next)
                    {
                        self.applied_transfers.insert(event.id);
                        self.transferred
                            .insert((channel_id, event.author.user_id()));
                        activity = Some(("transferred", Some(next.to_string())));
                    }
                }
                EventKind::ChannelLeave => {
                    if event.ciphertext.is_empty()
                        && !self.applied_leaves.contains(&event.id)
                        && let Some(state) = self.states.get_mut(&channel_id)
                        && state.leave_member(&event.author.user_id())
                    {
                        self.applied_leaves.insert(event.id);
                        self.departed.insert((channel_id, event.author.user_id()));
                        activity = Some(("left", Some(event.author.user_id())));
                    }
                }
                EventKind::ChannelAccept => {
                    if event.ciphertext.is_empty()
                        && let Some(state) = self.states.get_mut(&channel_id)
                        && state.is_member(&event.author.user_id())
                    {
                        if !state.is_accepted_member(&event.author.user_id()) {
                            activity = Some(("accepted", Some(event.author.user_id())));
                        }
                        state.accept_invite(&event.author.user_id(), event.id);
                        if event.author == my_author {
                            after_accept.insert(event.id);
                        }
                    }
                }
                EventKind::ChannelReplica => {
                    if let [enabled @ 0..=1] = event.ciphertext.as_slice()
                        && self
                            .states
                            .get(&channel_id)
                            .is_some_and(|state| state.is_accepted_member(&event.author.user_id()))
                    {
                        let key = (channel_id, event.author.user_id());
                        let version = (event.lamport, event.id);
                        if self
                            .replica_preferences
                            .get(&key)
                            .is_none_or(|(lamport, id, _)| version > (*lamport, *id))
                        {
                            self.replica_preferences
                                .insert(key, (event.lamport, event.id, *enabled == 1));
                        }
                    }
                }
                EventKind::KeyRotation => {
                    let Some(sealed) = SealedKeys::decode(&event.ciphertext) else {
                        continue;
                    };
                    let Some(state) = self.states.get_mut(&channel_id) else {
                        continue;
                    };
                    let author_uid = event.author.user_id();
                    if !state.is_accepted_member(&author_uid) {
                        continue;
                    }
                    let Some(author_x25519) = state
                        .members()
                        .iter()
                        .find(|m| m.user_id() == author_uid)
                        .map(|m| m.x25519_pub)
                    else {
                        continue;
                    };
                    if let Some(skd) = open_my_skd(me, &author_x25519, &sealed) {
                        state.record_sender_chain(author_uid, sealed.epoch, &skd);
                    }
                }
                EventKind::Message => {
                    if event.author == my_author || self.emitted.contains(&event.id) {
                        continue; // our own message, or already surfaced
                    }
                    let Some(state) = self.states.get_mut(&channel_id) else {
                        continue;
                    };
                    if state.pending_invite()
                        || (state.accepted_event_id().is_some()
                            && !after_accept.contains(&event.id))
                        || !state.is_accepted_member(&event.author.user_id())
                    {
                        continue;
                    }
                    // Sender-key encryption does not compress. Wrapped + decoded
                    // retained allocations are conservatively bounded by four times
                    // the ciphertext (including decoder capacity), plus metadata.
                    // Check before advancing a receive key. A single legacy record
                    // larger than half the global budget stays encrypted, allowing
                    // terminal metadata plus at least one next row without a spin.
                    let upper = event
                        .ciphertext
                        .len()
                        .saturating_mul(4)
                        .saturating_add(state.name().len().saturating_mul(2))
                        .saturating_add(
                            2 * std::mem::size_of::<ReceivedChannelMessage>() + 64 + 192,
                        );
                    if upper > CHANNEL_PENDING_BYTES / 2 {
                        log::warn!(
                            "channel message exceeds local reception byte budget; retaining encrypted event"
                        );
                        continue;
                    }
                    if retained_upper_bound.saturating_add(upper) > byte_budget {
                        *cursor -= 1;
                        limited = true;
                        break;
                    }
                    if let Some(plaintext) =
                        state.open_sender_message(&event.author.user_id(), &event.ciphertext)
                    {
                        let body = MessageBody::decode(&plaintext);
                        let msg = ReceivedChannelMessage {
                            channel_id,
                            channel_name: state.name().to_string(),
                            from: event.author.user_id(),
                            text: body.text,
                            protection: body.protection,
                            reply_to: body.reply_to,
                            event_id: event.id,
                            wall_clock: event.wall_clock,
                            wrapped: plaintext,
                            received_at: 0,
                            arrival_kind: "live",
                            own_device: false,
                            batch_id: None,
                            batch_complete: true,
                            batch_size: 1,
                            reception_only: false,
                        };
                        retained_upper_bound += upper;
                        self.emitted.insert(event.id);
                        out.push(msg);
                    }
                }
                _ => {}
            }
            if let Some((kind, target)) = activity
                && self.activity_emitted.insert(event.id)
            {
                self.activities
                    .entry(channel_id)
                    .or_default()
                    .push(ChannelActivity {
                        kind,
                        actor: event.author.user_id(),
                        target,
                        wall_clock: event.wall_clock,
                    });
            }
        }
        if !limited {
            let history = history.as_mut().expect("history projection initialized");
            while let Some(message) = history.front() {
                if self.emitted.contains(&message.event_id) {
                    history.pop_front();
                    continue;
                }
                if out.len() >= limit
                    || retained_upper_bound.saturating_add(message.retained_bytes()) > byte_budget
                {
                    limited = true;
                    break;
                }
                let message = history.pop_front().expect("front exists");
                retained_upper_bound += message.retained_bytes();
                self.emitted.insert(message.event_id);
                out.push(message);
            }
        }
        (out, limited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{ChannelMeta, ChannelState, new_channel_id};
    use crate::eventlog::event::{Event, EventKind};
    use crate::eventlog::store::EventLog;
    use crate::eventlog::sync::reconcile;

    fn append(
        log: &mut EventLog,
        author: &DeviceIdentity,
        channel: ConversationId,
        kind: EventKind,
        ciphertext: Vec<u8>,
    ) {
        let (parents, lamport) = log.prepare(&channel).unwrap();
        let self_author = Author::from_ed25519(author.public().ed25519_pub);
        let seq = log
            .version_vector(&channel)
            .get(&self_author)
            .copied()
            .unwrap_or(0)
            + 1;
        let event = Event::new(author, channel, seq, parents, lamport, 0, kind, ciphertext);
        log.append(event).unwrap();
    }

    fn collect<'a>(log: &'a EventLog, channel: &ConversationId) -> Vec<&'a Event> {
        log.events(channel)
    }

    /// Build the author's own [`ChannelState`] for `meta`, with its identity set and
    /// a fresh epoch sender key generated (so it can seal). Returns the state.
    fn author_state(
        author: &DeviceIdentity,
        channel: ConversationId,
        meta: &ChannelMeta,
    ) -> ChannelState {
        let mut state = ChannelState::from_meta(channel, meta.clone());
        state.set_identity(author.public().user_id());
        state
    }

    #[test]
    fn group_reception_replays_4097_messages_with_one_ordered_cursor() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let meta = ChannelMeta {
            name: "replay benchmark".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode(),
        );
        let mut sender = author_state(&alice, channel, &meta);
        let keys =
            seal_keys_for(&alice, &meta.members, &sender.my_sender_distribution(), 0).unwrap();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::KeyRotation,
            keys.encode(),
        );
        for index in 0..4097 {
            append(
                &mut log,
                &alice,
                channel,
                EventKind::Message,
                sender
                    .seal_sender_message(format!("message {index}").as_bytes())
                    .unwrap(),
            );
        }
        let events = collect(&log, &channel);
        let started = std::time::Instant::now();
        let mut replay = ChannelReplay::new(&events);
        let mut book = ChannelBook::default();
        let mut identities = HashSet::new();
        let mut batches = 0;
        loop {
            let (messages, limited) =
                book.process_replay(&bob, channel, &mut replay, 64, CHANNEL_PENDING_BYTES);
            batches += 1;
            assert!(messages.len() <= 64);
            assert!(
                messages
                    .iter()
                    .map(ReceivedChannelMessage::retained_bytes)
                    .sum::<usize>()
                    <= CHANNEL_PENDING_BYTES
            );
            for message in messages {
                assert!(identities.insert(message.event_id));
            }
            if !limited {
                break;
            }
        }
        assert_eq!(identities.len(), 4097);
        assert_eq!(batches, 65);
        assert_eq!(replay.cursor, events.len());
        eprintln!(
            "group reception ordered replay: 4097 messages, 65 bounded batches, {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn group_reception_byte_budget_checks_before_consuming_receive_keys() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let meta = ChannelMeta {
            name: "bytes".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode(),
        );
        let mut sender = author_state(&alice, channel, &meta);
        let keys =
            seal_keys_for(&alice, &meta.members, &sender.my_sender_distribution(), 0).unwrap();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::KeyRotation,
            keys.encode(),
        );
        let large = sender
            .seal_sender_message(&vec![b'x'; 2 * 1024 * 1024])
            .unwrap();
        append(&mut log, &alice, channel, EventKind::Message, large.clone());
        append(
            &mut log,
            &alice,
            channel,
            EventKind::Message,
            sender.seal_sender_message(b"after").unwrap(),
        );
        let events = collect(&log, &channel);
        let mut book = ChannelBook::default();
        let (messages, limited) =
            book.process_bounded(&bob, channel, &events, 64, CHANNEL_PENDING_BYTES);
        assert!(
            !limited,
            "an oversized trusted legacy row must not cause a retry spin"
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].text, b"after");
        assert!(messages[0].retained_bytes() <= CHANNEL_PENDING_BYTES);
        let recovered = book
            .state_mut(&channel)
            .unwrap()
            .open_sender_message(&alice.public().user_id(), &large)
            .unwrap();
        assert_eq!(
            recovered.len(),
            2 * 1024 * 1024,
            "skipping the oversized row must preserve its receive key"
        );

        let mut fresh = ChannelBook::default();
        let (messages, limited) = fresh.process_bounded(&bob, channel, &events, 64, 1);
        assert!(messages.is_empty());
        assert!(limited);
        let (messages, limited) =
            fresh.process_bounded(&bob, channel, &events, 64, CHANNEL_PENDING_BYTES);
        assert_eq!(messages.len(), 1);
        assert!(!limited);
    }

    #[test]
    fn group_reception_bounded_processing_does_not_skip_messages_before_dissolution() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let meta = ChannelMeta {
            name: "bounded".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode(),
        );
        let mut sender = author_state(&alice, channel, &meta);
        let keys =
            seal_keys_for(&alice, &meta.members, &sender.my_sender_distribution(), 0).unwrap();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::KeyRotation,
            keys.encode(),
        );
        for text in [b"one".as_slice(), b"two".as_slice()] {
            append(
                &mut log,
                &alice,
                channel,
                EventKind::Message,
                sender.seal_sender_message(text).unwrap(),
            );
        }
        append(
            &mut log,
            &alice,
            channel,
            EventKind::ChannelDissolve,
            vec![],
        );
        let events = collect(&log, &channel);
        let mut book = ChannelBook::default();
        assert_eq!(
            book.process_limited(&bob, channel, &events, 1)[0].text,
            b"one"
        );
        assert!(!book.state(&channel).unwrap().is_dissolved());
        assert_eq!(
            book.process_limited(&bob, channel, &events, 1)[0].text,
            b"two"
        );
        assert!(book.process_limited(&bob, channel, &events, 1).is_empty());
        assert!(book.state(&channel).unwrap().is_dissolved());
    }

    #[test]
    fn group_reception_projection_removal_respects_signed_ancestry_with_backward_lamport() {
        let owner = DeviceIdentity::generate();
        let removed = DeviceIdentity::generate();
        let channel = new_channel_id();
        let genesis = Event::new(
            &owner,
            channel,
            1,
            vec![],
            900,
            1000,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "skew".into(),
                members: vec![owner.public(), removed.public()],
                epoch: 0,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        let removal = Event::new(
            &owner,
            channel,
            2,
            vec![genesis.id],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "skew".into(),
                members: vec![owner.public()],
                epoch: 1,
                owner: owner.public().user_id(),
            }
            .encode(),
        );
        let mut log = EventLog::default();
        log.append(genesis).unwrap();
        log.append(removal).unwrap();
        let events = collect(&log, &channel);
        let members = ChannelBook::projected_members(channel, &events).unwrap();
        assert_eq!(members, vec![owner.public()]);
        let mut book = ChannelBook::default();
        book.process(&owner, channel, &events);
        assert!(
            !book
                .state(&channel)
                .unwrap()
                .is_member(&removed.public().user_id())
        );
    }

    #[test]
    fn only_owner_can_dissolve_a_channel_and_closure_survives_replay() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        let meta = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode(),
        );
        append(&mut log, &bob, channel, EventKind::ChannelDissolve, vec![]);
        let mut book = ChannelBook::new();
        book.process(&bob, channel, &collect(&log, &channel));
        assert!(!book.state(&channel).unwrap().is_dissolved());

        append(
            &mut log,
            &alice,
            channel,
            EventKind::ChannelDissolve,
            vec![],
        );
        book.process(&bob, channel, &collect(&log, &channel));
        assert!(book.state(&channel).unwrap().is_dissolved());
        let mut replayed = ChannelBook::new();
        replayed.process(&bob, channel, &collect(&log, &channel));
        assert!(replayed.state(&channel).unwrap().is_dissolved());
        append(
            &mut log,
            &alice,
            channel,
            EventKind::ChannelRename,
            b"too late".to_vec(),
        );
        replayed.process(&bob, channel, &collect(&log, &channel));
        assert_eq!(replayed.state(&channel).unwrap().name(), "general");
    }

    #[test]
    fn sealed_keys_round_trip_and_reject_trailing() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();

        let bytes = sealed.encode();
        assert_eq!(SealedKeys::decode(&bytes), Some(sealed.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(SealedKeys::decode(&junk), None);
        assert_eq!(sealed.entries.len(), 2);
        assert_eq!(sealed.sender, alice.public().user_id());
    }

    #[test]
    fn a_member_opens_its_own_sealed_skd() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();

        let opened = open_my_skd(&bob, &alice.public().x25519_pub, &sealed).unwrap();
        assert_eq!(opened.encode(), skd.encode());
    }

    #[test]
    fn a_non_member_has_no_entry_to_open() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let mut alice_state = author_state(
            &alice,
            new_channel_id(),
            &ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            },
        );
        let skd = alice_state.my_sender_distribution();
        let sealed = seal_keys_for(&alice, &[alice.public(), bob.public()], &skd, 0).unwrap();
        assert!(open_my_skd(&carol, &alice.public().x25519_pub, &sealed).is_none());
    }

    #[test]
    fn channel_create_message_and_rotation_on_add() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        // Alice creates the channel {Alice, Bob} at epoch 0.
        let mut alice_log = EventLog::default();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"hello team").unwrap(),
        );

        // Bob reconciles + processes → learns the channel, records Alice's chain, reads it.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, b"hello team");
        assert_eq!(got[0].from, alice.public().user_id());
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 0);

        // Alice adds Carol → epoch 1, re-distributing her epoch-1 SKD.
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public(), carol.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"welcome carol").unwrap(),
        );

        // Carol reconciles fresh → gets epoch-1 SKD only → reads "welcome carol".
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let carol_got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        assert!(carol_got.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(carol_book.state(&channel).unwrap().epoch(), 1);

        // Bob reconciles the new events and reads the epoch-1 message.
        reconcile(&mut bob_log, &mut alice_log, channel);
        let bob_got2 = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert!(bob_got2.iter().any(|m| m.text == b"welcome carol"));
        assert_eq!(bob_book.state(&channel).unwrap().epoch(), 1);
    }

    #[test]
    fn late_joiner_cannot_read_pre_join_history() {
        // Carol is added at epoch 1. She must read epoch-1 messages but NOT the epoch-0
        // message sent before she joined (she never receives an epoch-0 sender chain).
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();

        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"secret pre-join").unwrap(),
        );

        // Add Carol at epoch 1 and send a post-join message.
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public(), carol.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"post-join hello").unwrap(),
        );

        // Carol syncs the FULL log (incl. the epoch-0 message) and processes it.
        let mut carol_log = EventLog::default();
        reconcile(&mut carol_log, &mut alice_log, channel);
        let mut carol_book = ChannelBook::new();
        let got = carol_book.process(&carol, channel, &collect(&carol_log, &channel));
        let texts: Vec<&[u8]> = got.iter().map(|m| m.text.as_slice()).collect();
        assert!(
            texts.contains(&&b"post-join hello"[..]),
            "must read post-join"
        );
        assert!(
            !texts.contains(&&b"secret pre-join"[..]),
            "late joiner must NOT read pre-join history",
        );
    }

    #[test]
    fn removed_member_cannot_read_post_removal_messages() {
        // Group forward security: after Bob is removed (epoch bump + a re-key that
        // excludes him), Bob cannot decrypt messages sent in the new epoch.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();

        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"before removal").unwrap(),
        );

        // Bob is up to date and reads the epoch-0 message.
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut bob_book = ChannelBook::new();
        assert!(
            bob_book
                .process(&bob, channel, &collect(&bob_log, &channel))
                .iter()
                .any(|m| m.text == b"before removal")
        );

        // Alice removes Bob → epoch 1, re-keying ONLY to the remaining member (herself).
        let meta1 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public()],
            epoch: 1,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta1.encode(),
        );
        alice_state.apply_meta(meta1.clone());
        let skd1 = alice_state.my_sender_distribution();
        let sealed1 = seal_keys_for(&alice, &meta1.members, &skd1, 1).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed1.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"after removal").unwrap(),
        );

        // Bob syncs everything and processes — he must NOT be able to read the epoch-1
        // message (the re-key excluded him; he holds no epoch-1 chain).
        reconcile(&mut bob_log, &mut alice_log, channel);
        let got = bob_book.process(&bob, channel, &collect(&bob_log, &channel));
        assert!(
            !got.iter().any(|m| m.text == b"after removal"),
            "removed member must NOT read post-removal messages",
        );
    }

    #[test]
    fn process_does_not_re_emit_an_already_seen_message() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut alice_log = EventLog::default();
        let meta0 = ChannelMeta {
            name: "general".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta0.encode(),
        );
        let mut alice_state = author_state(&alice, channel, &meta0);
        let skd0 = alice_state.my_sender_distribution();
        let sealed0 = seal_keys_for(&alice, &meta0.members, &skd0, 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed0.encode(),
        );
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            alice_state.seal_sender_message(b"hi").unwrap(),
        );

        let mut book = ChannelBook::new();
        let first = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert_eq!(first.len(), 1);
        // Re-feeding the same events does NOT re-open the message (single-use key;
        // the emitted set + consumed chain key both prevent a second surface).
        let second = book.process(&bob, channel, &collect(&alice_log, &channel));
        assert!(second.is_empty());
    }

    #[test]
    fn a_non_owner_cannot_change_channel_membership() {
        // Alice (owner) + Bob in a channel. Mallory is NOT a member but knows the channel
        // id; she forges a higher-epoch MembershipChange adding herself, naming Alice as
        // owner so the snapshot looks legitimate. A current member (Bob) must REJECT it —
        // only the OWNER (the event author == owner) may change membership.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        // Mallory forges {Alice, Bob, Mallory} at a higher epoch, signed by HERSELF but
        // claiming Alice as owner. The author (Mallory) != owner (Alice) → rejected.
        append(
            &mut log,
            &mallory,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "general".into(),
                members: vec![alice.public(), bob.public(), mallory.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        let mut bob_book = ChannelBook::new();
        bob_book.process(&bob, channel, &collect(&log, &channel));
        let state = bob_book.state(&channel).unwrap();
        assert_eq!(
            state.epoch(),
            0,
            "forged change by a non-owner must be ignored"
        );
        assert!(
            !state
                .members()
                .iter()
                .any(|m| m.user_id() == mallory.public().user_id()),
            "a non-owner cannot add a member to the channel"
        );
    }

    #[test]
    fn owner_transfer_allows_the_new_owner_to_manage_membership() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::ChannelOwnerTransfer,
            bob.public().user_id().into_bytes(),
        );
        append(
            &mut log,
            &bob,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 1,
                owner: bob.public().user_id(),
            }
            .encode(),
        );
        let mut book = ChannelBook::new();
        book.process(&alice, channel, &collect(&log, &channel));
        let state = book.state(&channel).unwrap();
        assert_eq!(state.owner(), bob.public().user_id());
        assert!(state.is_member(&carol.public().user_id()));
        book.process(&alice, channel, &collect(&log, &channel));
        assert_eq!(
            book.state(&channel).unwrap().owner(),
            bob.public().user_id()
        );
        append(
            &mut log,
            &bob,
            channel,
            EventKind::ChannelOwnerTransfer,
            alice.public().user_id().into_bytes(),
        );
        book.process(&alice, channel, &collect(&log, &channel));
        assert_eq!(
            book.state(&channel).unwrap().owner(),
            alice.public().user_id()
        );
        let kinds: Vec<_> = book
            .activity(&channel)
            .into_iter()
            .map(|entry| entry.kind)
            .collect();
        assert_eq!(kinds, ["created", "transferred", "joined", "transferred"]);
        book.process(&alice, channel, &collect(&log, &channel));
        assert_eq!(
            book.state(&channel).unwrap().owner(),
            alice.public().user_id()
        );
    }

    #[test]
    fn member_leave_removes_only_its_author_and_is_idempotent() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(&mut log, &bob, channel, EventKind::ChannelLeave, vec![]);
        let mut book = ChannelBook::new();
        let events = collect(&log, &channel);
        book.process(&carol, channel, &events);
        let state = book.state(&channel).unwrap();
        assert!(!state.is_member(&bob.public().user_id()));
        assert!(state.is_member(&alice.public().user_id()));
        assert_eq!(state.epoch(), 1);
        book.process(&carol, channel, &events);
        assert_eq!(book.state(&channel).unwrap().epoch(), 1);
        let mut departed_book = ChannelBook::new();
        departed_book.process(&bob, channel, &events);
        assert!(
            departed_book
                .state_mut(&channel)
                .unwrap()
                .seal_sender_message(b"after leaving")
                .is_err()
        );
    }

    #[test]
    fn concurrent_owner_snapshot_cannot_restore_a_departed_member() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut owner_log = EventLog::default();
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut owner_log, channel);
        // Alice's unrelated rename raises her next event's Lamport time, but she
        // still has not observed Bob's concurrently signed leave.
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::ChannelRename,
            b"team 2".to_vec(),
        );
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        append(&mut bob_log, &bob, channel, EventKind::ChannelLeave, vec![]);
        reconcile(&mut owner_log, &mut bob_log, channel);

        let mut book = ChannelBook::new();
        let events = collect(&owner_log, &channel);
        book.process(&alice, channel, &events);
        let state = book.state(&channel).unwrap();
        assert!(!state.is_member(&bob.public().user_id()));
        assert!(state.is_member(&carol.public().user_id()));
        book.process(&alice, channel, &events);
        assert!(
            !book
                .state(&channel)
                .unwrap()
                .is_member(&bob.public().user_id())
        );

        // After observing the leave, Alice may deliberately invite Bob again.
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 2,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        book.process(&alice, channel, &collect(&owner_log, &channel));
        assert!(
            book.state(&channel)
                .unwrap()
                .is_member(&bob.public().user_id())
        );
    }

    #[test]
    fn late_invitee_does_not_inherit_a_concurrently_departed_member() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut owner_log = EventLog::default();
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut owner_log, channel);
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::ChannelRename,
            b"team 2".to_vec(),
        );
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        append(&mut bob_log, &bob, channel, EventKind::ChannelLeave, vec![]);
        reconcile(&mut owner_log, &mut bob_log, channel);

        let mut book = ChannelBook::new();
        book.process(&carol, channel, &collect(&owner_log, &channel));
        let state = book.state(&channel).unwrap();
        assert!(!state.is_member(&bob.public().user_id()));
        assert!(state.is_member(&carol.public().user_id()));
        book.process(&carol, channel, &collect(&owner_log, &channel));
        assert!(
            !book
                .state(&channel)
                .unwrap()
                .is_member(&bob.public().user_id())
        );
    }

    #[test]
    fn concurrent_owner_removal_and_member_leave_both_take_effect() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut owner_log = EventLog::default();
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut owner_log, channel);
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::ChannelRename,
            b"team 2".to_vec(),
        );
        append(
            &mut owner_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(&mut bob_log, &bob, channel, EventKind::ChannelLeave, vec![]);
        reconcile(&mut owner_log, &mut bob_log, channel);

        let mut book = ChannelBook::new();
        book.process(&alice, channel, &collect(&owner_log, &channel));
        let state = book.state(&channel).unwrap();
        assert!(!state.is_member(&bob.public().user_id()));
        assert!(!state.is_member(&carol.public().user_id()));
    }

    #[test]
    fn unmarked_legacy_member_addition_remains_auto_accepted() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        let mut book = ChannelBook::new();
        book.process(&alice, channel, &collect(&log, &channel));
        assert!(
            book.state(&channel)
                .unwrap()
                .is_accepted_member(&carol.public().user_id())
        );
    }

    #[test]
    fn new_invitation_requires_the_invitee_to_accept() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        let meta = ChannelMeta {
            name: "team".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode_invitation(),
        );
        let mut sender = author_state(&alice, channel, &meta);
        let sealed =
            seal_keys_for(&alice, &meta.members, &sender.my_sender_distribution(), 0).unwrap();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed.encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::Message,
            sender.seal_sender_message(b"before").unwrap(),
        );
        let mut book = ChannelBook::new();
        assert!(
            book.process(&bob, channel, &collect(&log, &channel))
                .is_empty()
        );
        assert!(book.state(&channel).unwrap().pending_invite());
        let mut owner_book = ChannelBook::new();
        owner_book.process(&alice, channel, &collect(&log, &channel));
        assert!(
            !owner_book
                .state_mut(&channel)
                .unwrap()
                .transfer_owner(&bob.public().user_id())
        );
        append(&mut log, &bob, channel, EventKind::ChannelAccept, vec![]);
        append(
            &mut log,
            &alice,
            channel,
            EventKind::Message,
            sender.seal_sender_message(b"after").unwrap(),
        );
        let got = book.process(&bob, channel, &collect(&log, &channel));
        assert_eq!(
            got.iter()
                .map(|message| message.text.as_slice())
                .collect::<Vec<_>>(),
            vec![b"after".as_slice()]
        );
        assert!(!book.state(&channel).unwrap().pending_invite());
        assert_eq!(
            book.activity(&channel)
                .iter()
                .filter(|entry| entry.kind == "accepted")
                .count(),
            1
        );
        owner_book.process(&alice, channel, &collect(&log, &channel));
        assert!(
            owner_book
                .state_mut(&channel)
                .unwrap()
                .transfer_owner(&bob.public().user_id())
        );
    }

    #[test]
    fn acceptance_uses_causal_ancestry_not_wall_clock_or_lamport() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let channel = new_channel_id();
        let meta = ChannelMeta {
            name: "team".into(),
            members: vec![alice.public(), bob.public()],
            epoch: 0,
            owner: alice.public().user_id(),
        };
        let mut alice_log = EventLog::default();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::MembershipChange,
            meta.encode_invitation(),
        );
        let mut sender = author_state(&alice, channel, &meta);
        let sealed =
            seal_keys_for(&alice, &meta.members, &sender.my_sender_distribution(), 0).unwrap();
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::KeyRotation,
            sealed.encode(),
        );
        let mut bob_log = EventLog::default();
        reconcile(&mut bob_log, &mut alice_log, channel);
        append(
            &mut bob_log,
            &bob,
            channel,
            EventKind::ChannelAccept,
            vec![],
        );
        // Alice has not seen Bob's acceptance. Later local sends are concurrent,
        // even if their Lamport clock advances past the acceptance's clock.
        for n in 0..4 {
            append(
                &mut alice_log,
                &alice,
                channel,
                EventKind::Message,
                sender
                    .seal_sender_message(format!("concurrent {n}").as_bytes())
                    .unwrap(),
            );
        }
        reconcile(&mut bob_log, &mut alice_log, channel);
        append(
            &mut alice_log,
            &alice,
            channel,
            EventKind::Message,
            sender.seal_sender_message(b"causally after").unwrap(),
        );
        reconcile(&mut bob_log, &mut alice_log, channel);
        let mut book = ChannelBook::new();
        let received = book.process(&bob, channel, &collect(&bob_log, &channel));
        assert_eq!(
            received
                .iter()
                .map(|message| message.text.as_slice())
                .collect::<Vec<_>>(),
            vec![b"causally after".as_slice()]
        );
    }

    #[test]
    fn late_invitee_keeps_earlier_unaccepted_members_pending() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();
        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "team".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode_invitation(),
        );
        let mut book = ChannelBook::new();
        book.process(&carol, channel, &collect(&log, &channel));
        let state = book.state(&channel).unwrap();
        assert!(state.pending_invite());
        assert!(!state.is_accepted_member(&bob.public().user_id()));
        append(&mut log, &bob, channel, EventKind::ChannelAccept, vec![]);
        book.process(&carol, channel, &collect(&log, &channel));
        assert!(
            book.state(&channel)
                .unwrap()
                .is_accepted_member(&bob.public().user_id())
        );
        assert!(book.state(&channel).unwrap().pending_invite());
    }

    #[test]
    fn a_member_who_is_not_owner_cannot_kick_anyone() {
        // The core fix: Alice owns a channel {Alice, Bob, Carol}. Bob (a member but NOT the
        // owner) authors a higher-epoch MembershipChange that removes Carol. Every node must
        // IGNORE it — Carol stays a member on Alice's, Carol's, and a fresh node's view.
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        // Bob (not the owner) tries to kick Carol at a higher epoch, naming Alice as owner.
        append(
            &mut log,
            &bob,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        for viewer in [&alice, &bob, &carol] {
            let mut book = ChannelBook::new();
            book.process(viewer, channel, &collect(&log, &channel));
            let state = book.state(&channel).unwrap();
            assert_eq!(state.epoch(), 0, "non-owner kick must be ignored");
            assert!(
                state
                    .members()
                    .iter()
                    .any(|m| m.user_id() == carol.public().user_id()),
                "the victim must remain a member on every node",
            );
        }
    }

    #[test]
    fn ownership_cannot_be_hijacked_by_a_higher_epoch() {
        // Mallory (a member) tries to seize ownership with a higher-epoch snapshot that
        // names HERSELF owner (and is authored by her). The owner is pinned at genesis, so
        // the snapshot — author == its claimed owner but != the genesis owner — is rejected.
        let alice = DeviceIdentity::generate();
        let mallory = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), mallory.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &mallory,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![mallory.public()],
                epoch: 9,
                owner: mallory.public().user_id(),
            }
            .encode(),
        );

        let mut book = ChannelBook::new();
        book.process(&alice, channel, &collect(&log, &channel));
        let state = book.state(&channel).unwrap();
        assert_eq!(
            state.epoch(),
            0,
            "ownership-hijack snapshot must be ignored"
        );
        assert_eq!(state.owner(), alice.public().user_id());
        assert!(
            state
                .members()
                .iter()
                .any(|m| m.user_id() == alice.public().user_id())
        );
    }

    #[test]
    fn a_node_excluded_by_the_winning_membership_change_still_converges() {
        // epoch 0: {alice, bob, carol}, owner alice. Two same-epoch-1 changes BOTH authored
        // by the owner (e.g. concurrent edits from the owner's two devices) — {alice, carol}
        // (drops bob) and {alice, bob} (drops carol). The deterministic tie-break picks one
        // winner; BOTH bob and carol must converge to it, even the one the winner excludes
        // (the regression: an excluded node used to skip + keep a stale view).
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let channel = new_channel_id();

        let mut log = EventLog::default();
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public(), carol.public()],
                epoch: 0,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), carol.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );
        append(
            &mut log,
            &alice,
            channel,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "g".into(),
                members: vec![alice.public(), bob.public()],
                epoch: 1,
                owner: alice.public().user_id(),
            }
            .encode(),
        );

        let members_seen_by = |who: &DeviceIdentity| -> Vec<String> {
            let mut book = ChannelBook::new();
            book.process(who, channel, &collect(&log, &channel));
            let mut m: Vec<String> = book
                .state(&channel)
                .unwrap()
                .members()
                .iter()
                .map(|p| p.user_id())
                .collect();
            m.sort();
            m
        };
        // Every node — including whichever one the winner excludes — agrees on the membership.
        assert_eq!(members_seen_by(&bob), members_seen_by(&carol));
        assert_eq!(members_seen_by(&bob), members_seen_by(&alice));
    }
}

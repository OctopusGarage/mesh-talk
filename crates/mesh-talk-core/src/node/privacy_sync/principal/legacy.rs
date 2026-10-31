//! Current legacy channel state for one principal. Historical departures and
//! admitted IDs belong to the selection's SQL facts, never these current values.
use super::*;
use crate::channel::{ChannelMeta, ChannelState};

pub(super) struct LegacyChannel {
    pub(super) future: Option<ChannelState>,
    pub(super) state: Option<ChannelState>,
    pub(super) future_pair: bool,
    established: Option<(String, Vec<PublicIdentity>)>,
    me: PublicIdentity,
    peer: PublicIdentity,
    relay: bool,
    own_peer: bool,
    dissolved: bool,
    departed_actor: Option<String>,
    removed_self: bool,
}
fn apply_meta(state: &mut ChannelState, meta: ChannelMeta, original: usize, invited: bool) {
    let prior: HashSet<_> = state
        .members()
        .iter()
        .map(PublicIdentity::user_id)
        .collect();
    if meta.members.len() < original {
        state.apply_meta_after_concurrent_leave(meta);
    } else {
        state.apply_meta(meta);
    }
    // This reducer only needs acceptance of current members. Re-addition below
    // still follows the signed invitation/legacy rule; removed identities are
    // represented by indexed departure facts, not a growing current snapshot.
    for id in &prior {
        if !state.is_member(id) {
            state.mark_invited(id);
        }
    }
    let added: Vec<_> = state
        .members()
        .iter()
        .map(PublicIdentity::user_id)
        .filter(|id| !prior.contains(id))
        .collect();
    for id in added {
        if invited {
            state.mark_invited(&id);
        } else {
            state.auto_accept_legacy_member(&id);
        }
    }
}
fn initial(
    channel: ConversationId,
    meta: ChannelMeta,
    author: &str,
    invited: bool,
) -> ChannelState {
    let mut state = ChannelState::from_meta(channel, meta);
    if invited {
        let ids: Vec<_> = state
            .members()
            .iter()
            .map(PublicIdentity::user_id)
            .filter(|id| id != author)
            .collect();
        for id in ids {
            state.mark_invited(&id);
        }
    }
    state
}
impl LegacyChannel {
    pub(super) fn new(
        store: &GuardedSyncStore<'_>,
        conversation: ConversationId,
        relay: bool,
        own_peer: bool,
    ) -> Self {
        let established = store
            .node
            .channels
            .lock()
            .expect("channels lock")
            .state(&conversation)
            .map(|s| (s.genesis_owner().to_string(), s.members().to_vec()));
        Self {
            future: None,
            state: None,
            future_pair: false,
            established,
            me: store.node.identity.public(),
            peer: store.peer.clone(),
            relay,
            own_peer,
            dissolved: false,
            departed_actor: None,
            removed_self: false,
        }
    }
    pub(super) fn finish_future(&mut self) {
        self.future_pair = self.future.take().is_some_and(|state| {
            state.is_member(&self.me.user_id())
                && (state.is_member(&self.peer.user_id()) || self.relay)
        });
    }
    pub(super) fn skip(&self, kind: EventKind) -> bool {
        self.dissolved
            || self.removed_self && kind != EventKind::MembershipChange
            || self.departed_actor.is_some()
                && !matches!(
                    kind,
                    EventKind::MembershipChange
                        | EventKind::ChannelOwnerTransfer
                        | EventKind::ChannelAccept
                )
    }
    /// Filtered metadata comes from an incremental exact ancestry/departure
    /// query; its original member count preserves concurrent-leave semantics.
    pub(super) fn future_step(
        &mut self,
        header: &EventHeader,
        body: Option<&Event>,
        meta: Option<(ChannelMeta, usize)>,
    ) -> Option<String> {
        let author = header.author.user_id();
        match header.kind {
            EventKind::MembershipChange => {
                if let Some((meta, original)) = meta {
                    let invited =
                        body.is_some_and(|body| ChannelMeta::is_invitation(&body.ciphertext));
                    if let Some(current) = self.future.as_mut() {
                        if author == current.owner() {
                            apply_meta(current, meta, original, invited);
                        }
                    } else if header.parent_count == 0
                        && author == meta.owner
                        && meta.is_member(&author)
                        && self
                            .established
                            .as_ref()
                            .is_none_or(|(owner, _)| owner == &author)
                    {
                        self.future = Some(initial(header.conversation, meta, &author, invited));
                    }
                }
            }
            EventKind::ChannelOwnerTransfer => {
                if let Some(current) = self.future.as_mut()
                    && author == current.owner()
                    && let Some(next) = body.and_then(|b| std::str::from_utf8(&b.ciphertext).ok())
                {
                    current.transfer_owner(next);
                }
            }
            EventKind::ChannelAccept => {
                if header.ciphertext_bytes == 0
                    && let Some(current) = self.future.as_mut()
                {
                    current.accept_invite(&author, header.id);
                }
            }
            EventKind::ChannelLeave
                if header.ciphertext_bytes == 0
                    && self
                        .future
                        .as_mut()
                        .is_some_and(|s| s.leave_member(&author)) =>
            {
                return Some(author);
            }
            _ => {}
        }
        None
    }
    pub(super) fn step(
        &mut self,
        header: &EventHeader,
        body: Option<&Event>,
        meta: Option<(ChannelMeta, usize)>,
        owner_allowed: bool,
    ) -> (bool, Option<String>) {
        if self.skip(header.kind) {
            return (false, None);
        }
        let author = header.author.user_id();
        let mut departure = None;
        let accepted = match header.kind {
            EventKind::MembershipChange => {
                if let Some((meta, original)) = meta {
                    let invited =
                        body.is_some_and(|body| ChannelMeta::is_invitation(&body.ciphertext));
                    if let Some(current) = self.state.as_mut() {
                        if author == current.owner()
                            && meta.owner == current.owner()
                            && (!self.removed_self || meta.is_member(&self.me.user_id()))
                        {
                            let was_member = current.is_member(&self.me.user_id());
                            apply_meta(current, meta, original, invited);
                            self.removed_self = (self.removed_self || was_member)
                                && !current.is_member(&self.me.user_id());
                            if self
                                .departed_actor
                                .as_ref()
                                .is_some_and(|id| current.is_member(id))
                            {
                                self.departed_actor = None;
                            }
                            true
                        } else {
                            false
                        }
                    } else {
                        let pinned = self
                            .established
                            .as_ref()
                            .is_some_and(|(owner, _)| owner == &author);
                        if header.parent_count == 0
                            && meta.owner == author
                            && meta.is_member(&author)
                            && (meta.is_member(&self.me.user_id()) || self.future_pair)
                            && if self.established.is_some() {
                                pinned
                            } else {
                                owner_allowed
                            }
                        {
                            self.state = Some(initial(header.conversation, meta, &author, invited));
                            true
                        } else {
                            false
                        }
                    }
                } else {
                    false
                }
            }
            EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance => false,
            EventKind::ChannelOwnerTransfer => self.state.as_mut().is_some_and(|current| {
                author == current.owner()
                    && body
                        .and_then(|b| std::str::from_utf8(&b.ciphertext).ok())
                        .is_some_and(|next| current.transfer_owner(next))
            }),
            EventKind::ChannelLeave => {
                let leaving = header.ciphertext_bytes == 0
                    && self
                        .state
                        .as_mut()
                        .is_some_and(|current| current.leave_member(&author));
                if leaving {
                    departure = Some(author.clone());
                    if author == self.me.user_id() || author == self.peer.user_id() {
                        self.departed_actor = Some(author);
                    }
                }
                leaving
            }
            EventKind::ChannelAccept => {
                header.ciphertext_bytes == 0
                    && self.state.as_mut().is_some_and(|current| {
                        if current.is_member(&author) {
                            current.accept_invite(&author, header.id);
                            true
                        } else {
                            false
                        }
                    })
            }
            _ => self.state.as_ref().is_some_and(|state| {
                state.is_accepted_member(&author)
                    && (!matches!(
                        header.kind,
                        EventKind::ChannelRename | EventKind::ChannelDissolve
                    ) || author == state.owner())
                    && (header.kind != EventKind::ChannelDissolve || header.ciphertext_bytes == 0)
                    && header.kind != EventKind::Profile
            }),
        };
        if accepted && header.kind == EventKind::ChannelDissolve {
            self.dissolved = true;
        }
        (accepted, departure)
    }
    pub(super) fn permitted(&self) -> bool {
        let current = self
            .state
            .as_ref()
            .map(|state| state.members())
            .or_else(|| {
                self.established
                    .as_ref()
                    .map(|(_, members)| members.as_slice())
            });
        let pair = current.is_some_and(|members| {
            members.contains(&self.me)
                && (members.contains(&self.peer) || self.relay || self.own_peer)
        });
        let departure = self.departed_actor.as_ref().is_some_and(|departed| {
            current.is_some_and(|members| {
                (departed == &self.me.user_id() && members.contains(&self.peer))
                    || (departed == &self.peer.user_id() && members.contains(&self.me))
            })
        });
        let removal = self.removed_self
            && current.is_some_and(|members| members.contains(&self.peer) || self.relay);
        pair || departure || removal
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct LegacyProgress {
    future: Option<Vec<u8>>,
    state: Option<Vec<u8>>,
    future_pair: bool,
    dissolved: bool,
    departed_actor: Option<String>,
    removed_self: bool,
}
impl LegacyChannel {
    pub(super) fn progress(&self) -> Result<LegacyProgress, LogError> {
        let encode = |state: &ChannelState| {
            state
                .encode_current()
                .map_err(|error| LogError::Serialization(error.to_string()))
        };
        Ok(LegacyProgress {
            future: self.future.as_ref().map(encode).transpose()?,
            state: self.state.as_ref().map(encode).transpose()?,
            future_pair: self.future_pair,
            dissolved: self.dissolved,
            departed_actor: self.departed_actor.clone(),
            removed_self: self.removed_self,
        })
    }
    pub(super) fn restore(&mut self, progress: LegacyProgress) -> Result<(), LogError> {
        let decode = |bytes: Vec<u8>| {
            ChannelState::decode_current(&bytes)
                .map_err(|error| LogError::Serialization(error.to_string()))
        };
        self.future = progress.future.map(decode).transpose()?;
        self.state = progress.state.map(decode).transpose()?;
        self.future_pair = progress.future_pair;
        self.dissolved = progress.dissolved;
        self.departed_actor = progress.departed_actor;
        self.removed_self = progress.removed_self;
        Ok(())
    }
}

/// Collecting SDK/reference adapter. Production network preparation calls the
/// same two reducer steps with indexed departure/parent facts instead.
pub(in crate::node::privacy_sync) fn project(
    store: &GuardedSyncStore<'_>,
    conversation: ConversationId,
    events: &[Event],
    relay: bool,
    own_peer: bool,
    private: Option<&PrivacyState>,
) -> HashSet<EventId> {
    let graph: HashMap<_, _> = events.iter().map(|event| (event.id, event)).collect();
    let mut reducer = LegacyChannel::new(store, conversation, relay, own_peer);
    let filter = |event: &Event, departures: &[(String, EventId)]| {
        let mut meta = crate::channel::ChannelMeta::decode(&event.ciphertext)?;
        let original = meta.members.len();
        meta.members.retain(|member| {
            !departures.iter().any(|(id, leave)| {
                id == &member.user_id()
                    && !crate::node::channel::descends_from(event, *leave, &graph)
            })
        });
        Some((meta, original))
    };
    let mut future_departures = Vec::new();
    for event in events {
        let header = EventHeader::of(event, 0);
        let meta = (event.kind == EventKind::MembershipChange)
            .then(|| filter(event, &future_departures))
            .flatten();
        if let Some(member) = reducer.future_step(&header, Some(event), meta) {
            future_departures.push((member, event.id));
        }
    }
    reducer.finish_future();
    let mut departures = Vec::new();
    let mut allowed = HashSet::new();
    for event in events {
        if !event.parents.iter().all(|id| allowed.contains(id)) {
            continue;
        }
        let header = EventHeader::of(event, 0);
        let meta = (event.kind == EventKind::MembershipChange)
            .then(|| filter(event, &departures))
            .flatten();
        let owner_allowed = private.is_none_or(|state| {
            state.proofs.announcements().iter().any(|proof| {
                proof.ed25519_pub == *event.author.ed25519_pub()
                    && proof
                        .account_id()
                        .is_some_and(|id| id == store.node.account_id() || state.policy.allows(&id))
            })
        });
        let (accepted, departure) = reducer.step(&header, Some(event), meta, owner_allowed);
        if let Some(member) = departure {
            departures.push((member, event.id));
        }
        if accepted {
            allowed.insert(event.id);
        }
    }
    if !reducer.permitted() {
        allowed.clear();
    }
    allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn participant_network_legacy_current_state_does_not_keep_removed_acceptances() {
        let owner = DeviceIdentity::generate();
        let initial = DeviceIdentity::generate();
        let conversation = ConversationId::new([187; 32]);
        let mut state = ChannelState::from_meta(
            conversation,
            ChannelMeta {
                name: "current membership".into(),
                members: vec![owner.public(), initial.public()],
                epoch: 0,
                owner: owner.public().user_id(),
            },
        );
        let initial_bytes = state.encode_current().unwrap().len();
        for epoch in 1..=128 {
            let next = DeviceIdentity::generate();
            apply_meta(
                &mut state,
                ChannelMeta {
                    name: "current membership".into(),
                    members: vec![owner.public(), next.public()],
                    epoch,
                    owner: owner.public().user_id(),
                },
                2,
                false,
            );
            assert!(
                state.is_accepted_member(&next.public().user_id()),
                "legacy snapshots still auto-accept the added current member"
            );
            assert_eq!(
                state.encode_current().unwrap().len(),
                initial_bytes,
                "removed acceptance identities must not accumulate in current state at epoch {epoch}"
            );
            state = ChannelState::decode_current(&state.encode_current().unwrap()).unwrap();
        }
    }
}

//! One authoritative channel reduction step shared by pure validation and the
//! indexed runtime. Historical membership facts have an indexed owner; only
//! current roles, member roots and proof bindings are retained between steps.
use super::super::channel_account_authority::{
    AccountAuthority, LegacyAccountMember, LegacyAuthorityContext, admitted,
};
use super::super::channel_account_bootstrap::DeviceControl;
use super::graph::AuthorityGraph;
use super::*;
use crate::eventlog::LogError;

pub(super) struct MembershipReducer {
    pub(super) channel_id: ConversationId,
    pub(super) authority: AccountAuthority,
    pub(super) authorized: HashSet<EventId>,
    pub(super) bindings: HashMap<String, crate::discovery::Announce>,
    pub(super) roots: HashMap<String, (PublicIdentity, EventId, Option<EventId>, bool)>,
    pub(super) prior: Option<ChannelState>,
    pub(super) known_local: bool,
    pub(super) departures: Vec<(String, EventId)>,
    pub(super) history: Option<std::sync::Arc<super::projection_facts::ProjectionFacts>>,
}
impl MembershipReducer {
    pub(super) fn new(channel_id: ConversationId) -> Self {
        Self {
            channel_id,
            authority: AccountAuthority::default(),
            authorized: HashSet::new(),
            bindings: HashMap::new(),
            roots: HashMap::new(),
            prior: None,
            known_local: false,
            departures: Vec::new(),
            history: None,
        }
    }
    /// Non-control events cannot change membership. Their admission needs only
    /// immutable header metadata and the indexed dependency-closed parent facts.
    pub(super) fn step_header(
        &self,
        header: &crate::eventlog::repository::EventHeader,
    ) -> Result<bool, LogError> {
        if super::projection_facts::is_membership_control(header.kind) {
            return Err(LogError::CorruptFile(
                "control requires authenticated body".into(),
            ));
        }
        let valid = self.prior.as_ref().is_some_and(|state| {
            !state.is_dissolved() && state.is_accepted_member(&header.author.user_id())
        });
        if !valid {
            return Ok(false);
        }
        self.history
            .as_ref()
            .ok_or_else(|| LogError::CorruptFile("indexed reducer has no fact owner".into()))?
            .parents_authorized(header.id)
    }
    pub(super) fn step(
        &mut self,
        event: &Event,
        by_id: &impl AuthorityGraph,
    ) -> Result<bool, LogError> {
        let channel_id = self.channel_id;
        let Self {
            authority,
            authorized,
            bindings,
            roots,
            prior,
            departures,
            history,
            ..
        } = self;
        if prior.as_ref().is_some_and(ChannelState::is_dissolved) {
            return Ok(false);
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
                    return Ok(false);
                };
                let original_member_count = meta.members.len();
                let mut retained = Vec::new();
                for member in std::mem::take(&mut meta.members) {
                    let mut blocked = false;
                    for (uid, leave_id) in departures.iter() {
                        if uid == &member.user_id() && !by_id.descends(event.id, *leave_id)? {
                            blocked = true;
                            break;
                        }
                    }
                    if !blocked && let Some(history) = history.as_ref() {
                        blocked = history.departure_blocks(&member.user_id(), event.id, by_id)?;
                    }
                    if !blocked {
                        retained.push(member);
                    }
                }
                meta.members = retained;
                if let Some(state) = prior.as_mut() {
                    if !state.can_admin(&author) {
                        return Ok(false);
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
                    *prior = Some(state);
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
                        valid = admitted(authority.accept_account(event, &proof, by_id))?;
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
                    return Ok(false);
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
                        valid = admitted(authority.leave_account(event, &proof, by_id))?;
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
                        super::super::channel_account_authority::AccountAuthorityControl::decode(
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
                        valid = admitted(authority.apply(event, &context, by_id))?;
                    }
                }
            }
            EventKind::ChannelDeviceInheritance => {
                valid = false;
                if let Some((_, _, proof)) = DeviceControl::signed_inheritance(event)
                    && admitted(authority.inherit_device(event, by_id))?
                {
                    valid = true;
                    bindings.insert(proof.user_id.clone(), proof);
                } else if let Some(state) = prior.as_ref() {
                    valid = super::super::channel_account_bootstrap::receive_keys_admitted(
                        state, event, authority, by_id,
                    )? || super::super::join_history::account_release_admitted(
                        state, event, authority, by_id,
                    )?;
                }
            }
            _ => {}
        }
        let closed = valid
            && if let Some(history) = history.as_ref() {
                history.parents_authorized(event.id)?
            } else {
                event
                    .parents
                    .iter()
                    .all(|parent| authorized.contains(parent))
            };
        if closed {
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
                    .filter_map(|proof| proof.account_id().map(|account| (proof.public(), account)))
                    .collect(),
            );
        }
        Ok(closed)
    }
    pub(super) fn finish(self) -> ChannelMembershipProjection {
        let invited = self
            .roots
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
        let accepted = self
            .roots
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
            state: self.prior,
            #[cfg(test)]
            departures: self.departures,
            authority: self.authority,
            accepted,
            authorized: self.authorized,
            historical: self.history,
            invited,
        }
    }
}

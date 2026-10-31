//! Incremental principal authorization over immutable headers. The current
//! policy/reducer state is bounded by current identities; historical membership,
//! departures, ancestry and selected IDs live in the existing selection owner.
use super::network::ParticipantView;
use super::*;
use crate::{
    discovery::Announce,
    eventlog::{
        Author,
        repository::{
            EventHeader,
            order::{CausalOrder, OrderGeneration, OrderReadiness, OrderWorkBudget},
            selection::{AncestryProgress, Selection, SelectionRow},
        },
        sync::network::{NetworkSyncStore, PreparationBudget, PreparationStep, SyncMetadata},
    },
};
use sha2::{Digest, Sha256};
pub(super) mod legacy;
mod progress;
use legacy::LegacyChannel;

enum Principal {
    Denied,
    Public,
    Direct([Author; 2]),
    Receipt(Announce),
    RelayReceipt([Author; 2]),
    Files {
        scopes: Box<
            dyn Iterator<Item = Result<super::super::privacy_runtime::ManifestScope, LogError>>
                + Send,
        >,
        scope: Option<super::super::privacy_runtime::ManifestScope>,
        parent: Option<Box<ParticipantView>>,
        examined: usize,
        skip: usize,
    },
    Modern {
        projection: super::super::channel::ChannelMembershipProjection,
        closing: bool,
        accounts: [Option<[u8; 32]>; 2],
        account: usize,
        after: Option<Vec<u8>>,
    },
    Legacy(LegacyChannel),
}
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum Phase {
    Begin,
    Kinds,
    Policy,
    Order,
    Authority,
    Classify,
    Files,
    ClosingRoots,
    ClosingAncestors,
    Future,
    Rows,
    Erase,
    Hash,
    Ready,
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Active {
    header: EventHeader,
    parent_after: Option<EventId>,
    parents_done: bool,
    rejected: bool,
    receipt: bool,
    receipt_checked: bool,
    receipt_candidate: bool,
    #[serde(skip)]
    body: Option<Event>,
    meta: Option<crate::channel::ChannelMeta>,
    original_members: usize,
    ancestry: Option<AncestryProgress>,
    ancestry_started: bool,
    ancestry_ready: bool,
    member: usize,
    departure_after: Option<EventId>,
}
pub(super) struct Preparation {
    pub(super) selection: Option<Selection>,
    phase: Phase,
    pub(super) metadata: SyncMetadata,
    generation: Option<OrderGeneration>,
    principal: Option<Principal>,
    rank: i64,
    active: Option<Active>,
    closing: Option<AncestryProgress>,
    hash: Option<Sha256>,
    hash_after: Option<EventId>,
    hash_pending: usize,
    hash_head: Option<EventId>,
    hash_eof: bool,
    reload: bool,
    pub(super) replica_shared: bool,
    restricted: bool,
    policy_local_channel: bool,
    policy_started: bool,
    policy_examined: usize,
    policy_skip: usize,
    policy_scopes: Option<
        Box<
            dyn Iterator<Item = Result<super::super::privacy_runtime::ManifestScope, LogError>>
                + Send,
        >,
    >,
}
impl Default for Preparation {
    fn default() -> Self {
        Self {
            selection: None,
            phase: Phase::Begin,
            metadata: SyncMetadata::default(),
            generation: None,
            principal: None,
            rank: 0,
            active: None,
            closing: None,
            hash: None,
            hash_after: None,
            hash_pending: 0,
            hash_head: None,
            hash_eof: false,
            reload: false,
            replica_shared: true,
            restricted: false,
            policy_local_channel: false,
            policy_started: false,
            policy_examined: 0,
            policy_skip: 0,
            policy_scopes: None,
        }
    }
}
impl Preparation {
    pub(super) fn ready(&self) -> bool {
        self.phase == Phase::Ready && !self.reload
    }
    pub(super) fn selection(&self) -> Result<&Selection, LogError> {
        self.selection.as_ref().ok_or(LogError::StaleCursor)
    }
    fn stage(&mut self, phase: Phase) -> PreparationStep {
        self.phase = phase;
        PreparationStep::default()
    }
    fn step_inner(
        &mut self,
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        match self.phase {
            Phase::Begin => {
                let rows = budget.rows_for(64)?;
                self.selection = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .begin_selection(
                        &view.repository,
                        2 * super::super::session_pool::MAX_TCP_CONNECTIONS,
                        rows,
                    )?;
                if self.selection.is_some() {
                    Ok(self.stage(Phase::Kinds))
                } else {
                    Ok(PreparationStep {
                        rows,
                        bytes: rows * 64,
                        ..Default::default()
                    })
                }
            }
            Phase::Kinds => {
                let step = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .prepare_kind_metadata_step(budget)?;
                if step.ready {
                    self.phase = Phase::Policy;
                }
                Ok(PreparationStep {
                    ready: false,
                    ..step
                })
            }
            Phase::Policy => {
                if !self.policy_started {
                    let book = store.node.channels.lock().expect("channels lock");
                    self.policy_local_channel = book.state(&view.conversation).is_some();
                    if self.policy_local_channel {
                        self.replica_shared =
                            book.replica_enabled(view.conversation, &store.node.user_id());
                    }
                    self.policy_scopes =
                        Some(Box::new(store.node.manifest_scopes(view.conversation)));
                    self.policy_started = true;
                }
                let next = self
                    .policy_scopes
                    .as_mut()
                    .expect("policy scope cursor")
                    .next()
                    .transpose()?;
                if self.policy_skip > 0 {
                    if next.is_none() {
                        return Err(LogError::StaleCursor);
                    }
                    self.policy_skip -= 1;
                    return Ok(PreparationStep {
                        rows: 1,
                        ..Default::default()
                    });
                }
                let Some(scope) = next else {
                    self.policy_scopes = None;
                    return Ok(self.stage(Phase::Order));
                };
                self.policy_examined += 1;
                let book = store.node.channels.lock().expect("channels lock");
                if book.state(&scope.parent).is_some() {
                    if !self.policy_local_channel
                        && !book.replica_enabled(scope.parent, &store.node.user_id())
                    {
                        self.replica_shared = false;
                    }
                    if view.policy.network_paused && !view.policy.sibling {
                        self.restricted = true;
                    }
                }
                Ok(PreparationStep {
                    rows: 1,
                    ..Default::default()
                })
            }
            Phase::Order => {
                let log = store.node.log.lock().expect("log lock");
                let repository = log.repository()?;
                let step = repository.order_step(
                    &view.conversation,
                    CausalOrder::Logical,
                    OrderWorkBudget {
                        vertices: budget.rows,
                        edges: budget.edges,
                        bytes: budget.bytes.min(64 * 1024),
                    },
                )?;
                if step.readiness == OrderReadiness::Ready {
                    self.generation =
                        repository.order_generation(&view.conversation, CausalOrder::Logical)?;
                    self.phase = Phase::Authority;
                }
                Ok(PreparationStep {
                    rows: step.seeded + step.ranked + step.cleaned,
                    edges: step.edges,
                    bytes: step.charged_bytes,
                    ready: false,
                })
            }
            Phase::Authority => {
                let kinds = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .raw_kind_revisions(view.conversation)?
                    .ok_or(LogError::StaleCursor)?;
                let known = store
                    .node
                    .channels
                    .lock()
                    .expect("channels lock")
                    .state(&view.conversation)
                    .is_some();
                if kinds[EventKind::MembershipChange as usize] >= 0 || known {
                    let log = store.node.log.lock().expect("log lock");
                    let step = store.node.channel_checkpoints.project_step_with_budget(
                        log.repository()?,
                        &store.node.identity,
                        view.conversation,
                        budget,
                    )?;
                    if step.ready {
                        self.phase = Phase::Classify;
                    }
                    return Ok(PreparationStep {
                        ready: false,
                        ..step
                    });
                }
                Ok(self.stage(Phase::Classify))
            }
            Phase::Classify => {
                self.principal = Some(store.principal(view.conversation, self.replica_shared)?);
                let phase = match self.principal.as_ref().expect("classified principal") {
                    Principal::Denied => Phase::Hash,
                    Principal::Files { .. } => Phase::Files,
                    Principal::Legacy(_) => Phase::Future,
                    Principal::Modern { closing: true, .. } => Phase::ClosingRoots,
                    _ => Phase::Rows,
                };
                Ok(self.stage(phase))
            }
            Phase::Files => {
                let Principal::Files {
                    scopes,
                    scope,
                    parent,
                    examined,
                    skip,
                } = self.principal.as_mut().expect("file principal")
                else {
                    unreachable!()
                };
                if *skip > 0 {
                    if scopes.next().transpose()?.is_none() {
                        return Err(LogError::StaleCursor);
                    }
                    *skip -= 1;
                    return Ok(PreparationStep {
                        rows: 1,
                        ..Default::default()
                    });
                }
                if parent.is_none()
                    && let Some(scope) = scope.as_ref()
                {
                    *parent = Some(Box::new(NetworkSyncStore::open_view(store, &scope.parent)?));
                }
                if let Some(view) = parent.as_ref() {
                    let step = NetworkSyncStore::prepare_view_step(store, view, budget)?;
                    if !step.ready {
                        return Ok(step);
                    }
                    if step.rows > 0 || step.edges > 0 || step.bytes > 0 {
                        return Ok(PreparationStep {
                            ready: false,
                            ..step
                        });
                    }
                    let selected = {
                        let state = view.preparation.lock().expect("view preparation lock");
                        store
                            .node
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .selected_contains(
                                state.selection()?,
                                scope.as_ref().expect("active scope").event,
                            )?
                    };
                    if selected {
                        store
                            .node
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .selection_author(
                                self.selection.as_ref().expect("selection"),
                                scope.as_ref().expect("active scope").author,
                                true,
                            )?;
                    }
                    *parent = None;
                    *scope = None;
                    return Ok(PreparationStep {
                        rows: 1,
                        ..Default::default()
                    });
                }
                if let Some(next) = scopes.next().transpose()? {
                    *examined += 1;
                    if next.parent != view.conversation
                        && !store.node.has_file_scope(next.parent)?
                    {
                        *parent = Some(Box::new(NetworkSyncStore::open_view(store, &next.parent)?));
                        *scope = Some(next);
                    }
                    Ok(PreparationStep {
                        rows: 1,
                        ..Default::default()
                    })
                } else {
                    Ok(self.stage(Phase::Rows))
                }
            }
            Phase::ClosingRoots => {
                let Principal::Modern {
                    projection,
                    accounts,
                    account,
                    after,
                    ..
                } = self.principal.as_mut().expect("modern principal")
                else {
                    unreachable!()
                };
                if *account < accounts.len() {
                    if let Some(id) = accounts[*account] {
                        let page = projection.authority.revocation_page_with_limit(
                            &id,
                            after.as_deref(),
                            budget.rows_for(192)?,
                        )?;
                        store
                            .node
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .mark_selection_ancestors(
                                self.selection.as_ref().expect("selection"),
                                &page.events,
                            )?;
                        let count = page.events.len();
                        *after = page.next;
                        if count == 0 || after.is_none() {
                            *account += 1;
                            *after = None;
                        }
                        return Ok(PreparationStep {
                            rows: count,
                            bytes: count * 192,
                            ..Default::default()
                        });
                    }
                    *account += 1;
                    return Ok(PreparationStep::default());
                }
                self.closing = Some(AncestryProgress {
                    rank: self.generation.map_or(0, |g| g.count) as i64 - 1,
                    parent_after: None,
                });
                Ok(self.stage(Phase::ClosingAncestors))
            }
            Phase::ClosingAncestors => {
                let Some(generation) = self.generation else {
                    return Ok(self.stage(Phase::Rows));
                };
                let step = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .selection_ancestors_step(
                        self.selection.as_ref().expect("selection"),
                        &view.repository,
                        generation.job,
                        self.closing.as_mut().expect("closing cursor"),
                        budget,
                    )?;
                if step.ready {
                    self.phase = Phase::Rows;
                }
                Ok(PreparationStep {
                    ready: false,
                    ..step
                })
            }
            Phase::Future | Phase::Rows => self.row_step(store, view, budget),
            Phase::Erase => {
                let count = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .clear_selected_rows_step(self.selection()?, budget.rows_for(64)?)?;
                if count == 0 {
                    self.metadata = SyncMetadata::default();
                    self.phase = Phase::Hash;
                }
                Ok(PreparationStep {
                    rows: count,
                    bytes: count * 64,
                    ..Default::default()
                })
            }
            Phase::Hash => self.hash_step(store, view, budget),
            Phase::Ready => Ok(PreparationStep {
                ready: true,
                ..Default::default()
            }),
        }
    }
    fn hash_step(
        &mut self,
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        if self.hash.is_none() {
            self.metadata.count += view.pending.len() as u64;
            for event in store
                .pending_channels
                .get(&view.conversation)
                .into_iter()
                .flat_map(|p| &p.events)
            {
                capabilities(&mut self.metadata, event.kind);
            }
            let mut hash = Sha256::new();
            hash.update(self.metadata.count.to_le_bytes());
            self.hash = Some(hash);
        }
        let mut examined = 0;
        let limit = budget.rows.min(budget.bytes / 32);
        if limit == 0 {
            return Err(LogError::PageBudgetTooSmall { required: 32 });
        }
        while examined < limit {
            if self.hash_head.is_none() && !self.hash_eof {
                let ids = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .selected_ids_page(self.selection()?, self.hash_after, 1)?;
                self.hash_head = ids.first().copied();
                self.hash_eof = self.hash_head.is_none();
                examined += ids.len();
                if examined == limit {
                    break;
                }
            }
            let pending = view.pending.get(self.hash_pending).copied();
            if let Some(id) = pending.filter(|id| self.hash_head.is_none_or(|head| *id <= head)) {
                self.hash
                    .as_mut()
                    .expect("hash initialized")
                    .update(id.as_bytes());
                self.hash_pending += 1;
                examined += 1;
            } else if let Some(id) = self.hash_head.take() {
                self.hash
                    .as_mut()
                    .expect("hash initialized")
                    .update(id.as_bytes());
                self.hash_after = Some(id);
            } else {
                self.metadata.fingerprint = Some(
                    self.hash
                        .take()
                        .expect("hash initialized")
                        .finalize()
                        .into(),
                );
                self.phase = Phase::Ready;
                break;
            }
        }
        Ok(PreparationStep {
            ready: self.ready(),
            rows: examined,
            bytes: examined * 32,
            edges: 0,
        })
    }
}
fn capabilities(metadata: &mut SyncMetadata, kind: EventKind) {
    metadata.channel_v5 |= kind == EventKind::ChannelMetadata;
    metadata.channel_v2 |= kind == EventKind::ChannelReplica;
    metadata.channel_v3 |= matches!(
        kind,
        EventKind::ChannelHistoryPolicy | EventKind::ChannelJoinHistory
    );
    metadata.channel_v4 |= matches!(
        kind,
        EventKind::ChannelAccountAuthority | EventKind::ChannelDeviceInheritance
    );
}

impl GuardedSyncStore<'_> {
    fn principal(
        &self,
        conversation: ConversationId,
        replica_shared: bool,
    ) -> Result<Principal, LogError> {
        if self.inbound && !replica_shared {
            return Ok(Principal::Denied);
        }
        let guard = self.node.privacy.state.read().expect("privacy lock");
        let private = guard.as_ref().filter(|s| s.policy.snapshot().invisible);
        let kinds = self
            .node
            .log
            .lock()
            .expect("log lock")
            .repository()?
            .raw_kind_revisions(conversation)?
            .ok_or(LogError::StaleCursor)?;
        let known = self
            .node
            .channels
            .lock()
            .expect("channels lock")
            .state(&conversation)
            .is_some();
        if private.is_none() {
            if kinds[EventKind::MembershipChange as usize] < 0 && !known {
                return Ok(Principal::Public);
            }
            return self.channel_principal(conversation, false, false, guard.as_ref());
        }
        let state = private.expect("private state");
        let me = self.node.identity.public();
        let own_account = self.node.account_id();
        let proof = state
            .proofs
            .by_author(&self.peer.ed25519_pub)
            .filter(|p| p.public() == self.peer);
        let peer_account = proof.as_ref().and_then(Announce::account_id);
        let own_peer = peer_account.as_deref() == Some(own_account.as_str());
        let manual = state
            .policy
            .snapshot()
            .allowed_accounts
            .into_iter()
            .filter(|a| a.source == PermissionSource::Manual)
            .map(|a| a.id)
            .collect::<HashSet<_>>();
        let relay = proof.as_ref().is_some_and(|p| p.post_office)
            && peer_account.as_ref().is_some_and(|a| manual.contains(a));
        let proofs = state.proofs.announcements();
        let control_target = proofs.iter().find(|a| {
            a.account_id()
                .is_some_and(|id| id != own_account && state.policy.allows(&id))
                && super::super::delivery_receipt::delivery_conversation_id(&me, &a.public())
                    == conversation
        });
        if let Some(target) = control_target {
            return Ok(if self.peer == target.public() || relay {
                Principal::Receipt(target.clone())
            } else {
                Principal::Denied
            });
        }
        let target = proofs.iter().find(|a| {
            a.account_id()
                .is_some_and(|id| id == own_account || state.policy.allows(&id))
                && super::super::conversation::dm_conversation_id(&me, &a.public()) == conversation
        });
        if let Some(target) = target {
            return Ok(if self.peer == target.public() || own_peer || relay {
                Principal::Direct([
                    Author::from_ed25519(me.ed25519_pub),
                    Author::from_ed25519(target.ed25519_pub),
                ])
            } else {
                Principal::Denied
            });
        }
        if state.own.post_office
            && peer_account.as_ref().is_some_and(|a| manual.contains(a))
            && let Some(other) = proofs.iter().find(|a| {
                a.public() != self.peer
                    && a.account_id().is_some_and(|a| manual.contains(&a))
                    && super::super::delivery_receipt::delivery_conversation_id(
                        &self.peer,
                        &a.public(),
                    ) == conversation
            })
        {
            return Ok(Principal::RelayReceipt([
                Author::from_ed25519(self.peer.ed25519_pub),
                Author::from_ed25519(other.ed25519_pub),
            ]));
        }
        if self.node.has_file_scope(conversation)? {
            return Ok(Principal::Files {
                scopes: Box::new(self.node.manifest_scopes(conversation)),
                scope: None,
                parent: None,
                examined: 0,
                skip: 0,
            });
        }
        self.channel_principal(conversation, relay, own_peer, Some(state))
    }
    fn channel_principal(
        &self,
        conversation: ConversationId,
        relay: bool,
        own_peer: bool,
        private: Option<&PrivacyState>,
    ) -> Result<Principal, LogError> {
        let projection = self
            .node
            .channel_checkpoints
            .published_projection(conversation)?;
        let Some(projection) = projection.filter(|p| p.authority.current_event().is_some()) else {
            return Ok(Principal::Legacy(LegacyChannel::new(
                self,
                conversation,
                relay,
                own_peer,
            )));
        };
        let authority = &projection.authority;
        let own = self.node.account.public().ed25519_pub;
        let proof = private
            .and_then(|s| s.proofs.by_author(&self.peer.ed25519_pub))
            .or_else(|| {
                if private.is_none() {
                    self.node
                        .roster
                        .lock()
                        .expect("roster lock")
                        .historical_announcement(&self.peer)
                        .cloned()
                } else {
                    None
                }
            })
            .filter(|p| p.public() == self.peer && p.valid_presence());
        let paired = self.node.certified_sibling(&self.peer)
            && proof
                .as_ref()
                .and_then(|p| p.account_cert.as_ref())
                .is_some_and(|c| c.account_ed25519_pub == own);
        let me_allowed = authority.account_grant(&own).is_some();
        let peer_allowed = proof.as_ref().is_some_and(|p| {
            authority.is_member(&self.peer, p)
                || authority.invitation_for_device(&self.peer, p).is_some()
        }) || (paired && me_allowed);
        let custody = private.is_some_and(|s| {
            s.own.post_office
                && proof
                    .as_ref()
                    .and_then(Announce::account_id)
                    .is_some_and(|id| {
                        s.policy
                            .snapshot()
                            .allowed_accounts
                            .iter()
                            .any(|a| a.id == id && a.source == PermissionSource::Manual)
                    })
        });
        let closing = !((me_allowed || custody) && (peer_allowed || relay));
        let mut accounts = [None, None];
        if closing {
            if me_allowed && let Some(proof) = proof.as_ref() {
                accounts[0] = authority.try_registered_account(&self.peer, proof)?;
            }
            if paired
                || ((peer_allowed || relay)
                    && authority
                        .try_registered_account(
                            &self.node.identity.public(),
                            &self.node.own_group_proof(),
                        )?
                        .is_some())
            {
                accounts[1] = Some(own);
            }
        }
        Ok(Principal::Modern {
            projection,
            closing,
            accounts,
            account: 0,
            after: None,
        })
    }
    fn receipt_current(&self, header: &EventHeader) -> bool {
        if header.author.ed25519_pub() != &self.node.identity.public().ed25519_pub {
            return true;
        }
        let binding = self
            .node
            .delivery_control_ids
            .lock()
            .expect("control ids lock")
            .get(&header.id)
            .cloned();
        binding.as_ref().is_some_and(|binding| {
            let guard = self.node.privacy.state.read().expect("privacy lock");
            let proof = if let Some(state) = guard.as_ref() {
                state.proofs.by_author(&binding.device.ed25519_pub)
            } else {
                self.node
                    .roster
                    .lock()
                    .expect("roster lock")
                    .historical_announcement(&binding.device)
                    .cloned()
            };
            proof.is_some_and(|p| {
                p.public() == binding.device
                    && p.account_id() == binding.account
                    && super::super::delivery_receipt::delivery_conversation_id(
                        &self.node.identity.public(),
                        &binding.device,
                    ) == header.conversation
            })
        })
    }
    fn principal_owner_allowed(&self, author: Author) -> bool {
        let guard = self.node.privacy.state.read().expect("privacy lock");
        guard.as_ref().is_none_or(|s| {
            s.proofs.announcements().iter().any(|p| {
                p.ed25519_pub == *author.ed25519_pub()
                    && p.account_id()
                        .is_some_and(|id| id == self.node.account_id() || s.policy.allows(&id))
            })
        })
    }
}
impl Preparation {
    fn advance(&mut self) -> PreparationStep {
        self.active = None;
        self.rank += 1;
        PreparationStep {
            rows: 1,
            bytes: std::mem::size_of::<EventHeader>(),
            ..Default::default()
        }
    }
    fn row_step(
        &mut self,
        store: &GuardedSyncStore<'_>,
        view: &ParticipantView,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.rows_for_bytes(std::mem::size_of::<EventHeader>())?;
        let future = self.phase == Phase::Future;
        let count = self.generation.map_or(0, |g| g.count);
        if self.rank as u64 >= count {
            if future {
                if let Some(Principal::Legacy(legacy)) = self.principal.as_mut() {
                    legacy.finish_future();
                }
                self.rank = 0;
                return Ok(self.stage(Phase::Rows));
            }
            let permitted = !matches!(self.principal.as_ref(),Some(Principal::Legacy(legacy)) if !legacy.permitted());
            return Ok(self.stage(if permitted { Phase::Hash } else { Phase::Erase }));
        }
        if self.active.is_none() {
            let header = store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selection_rank_header(
                    &view.repository,
                    self.generation.expect("ranked source").job,
                    self.rank,
                )?
                .ok_or(LogError::StaleCursor)?;
            self.active = Some(Active {
                header,
                parent_after: None,
                parents_done: false,
                rejected: false,
                receipt: false,
                receipt_checked: false,
                receipt_candidate: false,
                body: None,
                meta: None,
                original_members: 0,
                ancestry: None,
                ancestry_started: false,
                ancestry_ready: false,
                member: 0,
                departure_after: None,
            });
        }
        let active = self.active.as_mut().expect("active header");
        if active.rejected {
            return Ok(self.advance());
        }
        let header = active.header;
        if future
            && !matches!(
                header.kind,
                EventKind::MembershipChange
                    | EventKind::ChannelOwnerTransfer
                    | EventKind::ChannelAccept
                    | EventKind::ChannelLeave
            )
        {
            return Ok(self.advance());
        }
        if !future
            && matches!(self.principal.as_ref(),Some(Principal::Legacy(legacy)) if legacy.skip(header.kind))
        {
            return Ok(self.advance());
        }
        let is_legacy = matches!(self.principal, Some(Principal::Legacy(_)));
        let private = store
            .node
            .privacy
            .state
            .read()
            .expect("privacy lock")
            .as_ref()
            .is_some_and(|s| s.policy.snapshot().invisible);
        let receipt_filter =
            !future && (private || matches!(self.principal, Some(Principal::Public)));
        let legacy_body = is_legacy
            && matches!(
                header.kind,
                EventKind::MembershipChange | EventKind::ChannelOwnerTransfer
            );
        if receipt_filter && !active.receipt_checked {
            let required = if super::super::delivery_receipt::possible_sealed_wire_length(
                header.ciphertext_bytes,
            ) {
                header
                    .parent_count
                    .checked_mul(32)
                    .and_then(|n| n.checked_add(40))
                    .ok_or(LogError::RepositoryCounterExhausted)?
            } else {
                0
            };
            if required > budget.bytes {
                return Err(LogError::PageBudgetTooSmall { required });
            }
            active.receipt_candidate = if required > 0 {
                let prefix = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .ciphertext_prefix(&header, 40)?;
                super::super::delivery_receipt::possible_sealed_wire(
                    header.ciphertext_bytes,
                    &prefix,
                )
            } else {
                false
            };
            active.receipt_checked = true;
            if required > 0 {
                return Ok(PreparationStep {
                    bytes: required,
                    ..Default::default()
                });
            }
        }
        let receipt_candidate = active.receipt_candidate;
        if (legacy_body || receipt_candidate) && active.body.is_none() {
            let required = header
                .stored_bytes
                .checked_mul(3)
                .and_then(|n| n.checked_add(std::mem::size_of::<Event>()))
                .ok_or(LogError::RepositoryCounterExhausted)?;
            if required > budget.bytes {
                return Err(LogError::PageBudgetTooSmall { required });
            }
            let body = store
                .node
                .log
                .lock()
                .expect("log lock")
                .get(&header.id)?
                .ok_or(LogError::StaleCursor)?;
            active.receipt = receipt_candidate
                && super::super::delivery_receipt::valid_sealed_wire(&body.ciphertext);
            if header.kind == EventKind::MembershipChange && active.meta.is_none() {
                active.meta = crate::channel::ChannelMeta::decode(&body.ciphertext);
                active.original_members = active.meta.as_ref().map_or(0, |m| m.members.len());
            }
            active.body = Some(body);
            return Ok(PreparationStep {
                bytes: required,
                ..Default::default()
            });
        }
        if active.receipt && !store.receipt_current(&header) {
            return Ok(self.advance());
        }
        let closed_required =
            !future && (!matches!(self.principal, Some(Principal::Public)) || active.receipt);
        if closed_required && !active.parents_done {
            let step = store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selection_parents_step(
                    self.selection.as_ref().expect("selection"),
                    &view.repository,
                    header.id,
                    active.parent_after,
                    active.receipt,
                    budget.edges.min(budget.rows_for_bytes(64)?),
                )?;
            if !step.closed {
                active.rejected = true;
                return Ok(PreparationStep {
                    edges: step.examined,
                    bytes: step.examined * 64,
                    ..Default::default()
                });
            }
            active.parent_after = step.after;
            active.parents_done = step.complete;
            if !step.complete || step.examined > 0 {
                return Ok(PreparationStep {
                    edges: step.examined,
                    bytes: step.examined * 64,
                    ..Default::default()
                });
            }
        }
        if is_legacy
            && header.kind == EventKind::MembershipChange
            && active.meta.is_some()
            && !active.ancestry_ready
        {
            let selection = self.selection.as_ref().expect("selection");
            if !active.ancestry_started {
                let cleaned = store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .clear_selection_marks_step(selection, budget.rows_for(32)?)?;
                if cleaned > 0 {
                    return Ok(PreparationStep {
                        rows: cleaned,
                        bytes: cleaned * 32,
                        ..Default::default()
                    });
                }
                store
                    .node
                    .log
                    .lock()
                    .expect("log lock")
                    .repository()?
                    .mark_selection_ancestors(selection, &[header.id])?;
                active.ancestry_started = true;
                active.ancestry = Some(AncestryProgress {
                    rank: self.rank,
                    parent_after: None,
                });
            }
            let step = store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selection_ancestors_step(
                    selection,
                    &view.repository,
                    self.generation.expect("ranked source").job,
                    active.ancestry.as_mut().expect("ancestry cursor"),
                    budget,
                )?;
            if step.ready {
                active.ancestry_ready = true;
            }
            return Ok(PreparationStep {
                ready: false,
                ..step
            });
        }
        if let Some(meta) = active.meta.as_mut()
            && active.member < meta.members.len()
        {
            let limit = budget.rows_for(64)?;
            let member = meta.members[active.member].user_id();
            let page = store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .selection_departure_page(
                    self.selection.as_ref().expect("selection"),
                    i64::from(future),
                    &member,
                    active.departure_after,
                    limit,
                )?;
            let blocked = page.iter().any(|(_, ancestor)| !*ancestor);
            if blocked {
                meta.members.remove(active.member);
                active.departure_after = None;
            } else if page.len() < limit {
                active.member += 1;
                active.departure_after = None;
            } else {
                active.departure_after = page.last().map(|(id, _)| *id);
            }
            return Ok(PreparationStep {
                rows: page.len(),
                bytes: page.len() * 64,
                ..Default::default()
            });
        }
        let accepted = match self.principal.as_mut().expect("principal") {
            Principal::Denied => false,
            Principal::Public => true,
            Principal::Direct(authors) => {
                authors.contains(&header.author)
                    && !matches!(
                        header.kind,
                        EventKind::MembershipChange
                            | EventKind::KeyRotation
                            | EventKind::ChannelRename
                            | EventKind::ChannelOwnerTransfer
                            | EventKind::ChannelLeave
                            | EventKind::ChannelAccept
                    )
            }
            Principal::Receipt(target) => {
                header.kind == EventKind::Message
                    && (header.author.ed25519_pub() == &store.node.identity.public().ed25519_pub
                        && store
                            .node
                            .delivery_control_ids
                            .lock()
                            .expect("control ids lock")
                            .get(&header.id)
                            .is_some_and(|binding| {
                                binding.device == target.public()
                                    && binding.account == target.account_id()
                            })
                        || active.body.as_ref().is_some_and(|body| {
                            super::super::delivery_receipt::open_receipt(
                                &store.node.identity,
                                &store.node.account_id(),
                                target,
                                body,
                            )
                            .is_some()
                        }))
            }
            Principal::RelayReceipt(authors) => {
                authors.contains(&header.author)
                    && header.kind == EventKind::Message
                    && active.receipt
            }
            Principal::Files { .. } => {
                header.kind == EventKind::Message
                    && store
                        .node
                        .log
                        .lock()
                        .expect("log lock")
                        .repository()?
                        .selection_author(
                            self.selection.as_ref().expect("selection"),
                            header.author,
                            false,
                        )?
            }
            Principal::Modern {
                projection,
                closing,
                ..
            } => {
                projection.is_authorized(header.id)?
                    && (!*closing
                        || store
                            .node
                            .log
                            .lock()
                            .expect("log lock")
                            .repository()?
                            .selection_is_ancestor(
                                self.selection.as_ref().expect("selection"),
                                header.id,
                            )?)
            }
            Principal::Legacy(legacy) => {
                let meta = active.meta.take().map(|m| (m, active.original_members));
                let (accepted, departure) = if future {
                    (
                        false,
                        legacy.future_step(&header, active.body.as_ref(), meta),
                    )
                } else {
                    legacy.step(
                        &header,
                        active.body.as_ref(),
                        meta,
                        store.principal_owner_allowed(header.author),
                    )
                };
                if let Some(member) = departure {
                    store
                        .node
                        .log
                        .lock()
                        .expect("log lock")
                        .repository()?
                        .selection_departure(
                            self.selection.as_ref().expect("selection"),
                            i64::from(future),
                            &member,
                            header.id,
                        )?;
                }
                accepted
            }
        };
        if accepted {
            let own = header.author.ed25519_pub() == &store.node.identity.public().ed25519_pub;
            let device_policy = store.node.device_sync_policy();
            let selected = match header.kind {
                EventKind::Message => device_policy.history,
                EventKind::FileManifest => device_policy.attachments,
                _ => true,
            };
            if store
                .node
                .log
                .lock()
                .expect("log lock")
                .repository()?
                .insert_selection_row(
                    self.selection.as_ref().expect("selection"),
                    &view.repository,
                    SelectionRow {
                        header,
                        rank: self.rank,
                        candidate: !store.pull_only && (!self.restricted || own),
                        selected,
                        own,
                        receipt: active.receipt,
                    },
                )?
            {
                self.metadata.count += 1;
                capabilities(&mut self.metadata, header.kind);
            }
        }
        Ok(self.advance())
    }
}

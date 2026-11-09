//! Bounded account-membership maintenance owned by ordinary channel recovery.
use super::{
    Node, NodeError,
    channel_account_authority::{AccountAuthorityControl, AccountGrant, MAX_AUTHORITY_BYTES},
    channel_account_bootstrap::{
        AccountGroupRequest, AccountGroupResponse, DeviceControl, ReceiveDistribution,
        inheritance_account_message,
    },
};
use crate::eventlog::LogError;
use crate::{
    discovery::{Announce, PeerRecord},
    eventlog::{ConversationId, EventKind},
    identity::device::PublicIdentity,
};
use std::{
    collections::{BTreeMap, VecDeque},
    net::SocketAddr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[cfg(test)]
pub(in crate::node) type AccountChannelMaintenanceTestHook =
    dyn Fn(ConversationId) -> bool + Send + Sync;

const MAX_MIGRATIONS: usize = 4096;
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct MigrationAuthorization {
    version: u8,
    account: [u8; 32],
    device: PublicIdentity,
    roots: Vec<(ConversationId, crate::eventlog::EventId)>,
}
/// Local consent/provenance, never synchronized or inferred from an adopted identity.
pub(super) struct ChannelMigrationStore {
    file: crate::storage::record_log::EncryptedRecordLog<MigrationAuthorization>,
    record: MigrationAuthorization,
}
impl ChannelMigrationStore {
    pub(super) fn open(
        path: &std::path::Path,
        password: &str,
        account: [u8; 32],
        own: &PublicIdentity,
    ) -> Result<Self, crate::eventlog::LogError> {
        if path.try_exists()? && std::fs::metadata(path)?.len() > 512 * 1024 {
            return Err(LogError::CorruptFile(
                "channel migration authorization exceeds limit".into(),
            ));
        }
        let (file, records) = crate::storage::record_log::EncryptedRecordLog::<
            MigrationAuthorization,
        >::open(path, password, b"MTCMIG")?;
        let mut record = MigrationAuthorization {
            version: 1,
            account,
            device: own.clone(),
            roots: Vec::new(),
        };
        if records.len() > 1 {
            return Err(LogError::CorruptFile(
                "invalid channel migration authorization".into(),
            ));
        }
        if let Some(stored) = records.into_iter().next() {
            if stored.version != 1
                || stored.account != account
                || stored.roots.len() > MAX_MIGRATIONS
                || stored
                    .roots
                    .windows(2)
                    .any(|pair| pair[0].0.as_bytes() >= pair[1].0.as_bytes())
            {
                return Err(LogError::CorruptFile(
                    "channel migration authorization belongs to another profile or is invalid"
                        .into(),
                ));
            }
            if stored.device == *own {
                record = stored;
            }
        }
        Ok(Self { file, record })
    }
    pub(super) fn permits(&self, channel: ConversationId, root: crate::eventlog::EventId) -> bool {
        self.record.roots.contains(&(channel, root))
    }
    fn authorize(
        &mut self,
        roots: Vec<(ConversationId, crate::eventlog::EventId)>,
    ) -> Result<(), NodeError> {
        let mut next = self.record.clone();
        for (channel, root) in roots {
            next.roots.retain(|entry| entry.0 != channel);
            next.roots.push((channel, root));
        }
        next.roots.sort_unstable_by_key(|entry| *entry.0.as_bytes());
        if next.roots.len() > MAX_MIGRATIONS {
            return Err(NodeError::InvalidInput(
                "channel migration authorization limit reached".into(),
            ));
        }
        if next.roots != self.record.roots {
            self.file
                .rewrite(std::slice::from_ref(&next))
                .map_err(NodeError::Log)?;
            self.record = next;
        }
        Ok(())
    }
}

const MAX_PEERS: usize = 512;
const RETRY: Duration = Duration::from_secs(60);
const REFRESH: Duration = Duration::from_secs(300);
struct Capability {
    public: PublicIdentity,
    routes: Vec<SocketAddr>,
    generation: u64,
    supported: bool,
    proof: Option<Announce>,
    account_hint: Option<String>,
    probe_at: Instant,
    poll_at: Instant,
    poll_failed: bool,
    after: Option<ConversationId>,
    pending: VecDeque<ConversationId>,
    binding_poll_at: Instant,
    binding_failed: bool,
    binding_channel: Option<ConversationId>,
    binding_after: Option<String>,
    binding_cursor: usize,
    binding_turn: bool,
}
#[derive(Default)]
pub(super) struct ChannelAccountMaintenance {
    #[cfg(test)]
    projection_reads: usize,
    #[cfg(test)]
    distribution_reads: usize,
    input: Option<[u8; 32]>,
    observed_input: Option<[u8; 32]>,
    run_at: Option<Instant>,
    settled: BTreeMap<[u8; 32], ([u8; 32], Instant)>,
    peers: BTreeMap<String, Capability>,
    peer_cursor: usize,
    channel_cursor: usize,
    refresh_after: BTreeMap<([u8; 32], [u8; 32]), Instant>,
}
impl ChannelAccountMaintenance {
    pub(super) fn deadline(&self) -> Instant {
        self.run_at.unwrap_or_else(Instant::now)
    }

    // Scheduling evidence only: admitted work still checks current signed
    // authority, privacy and source fences. Capture before work, so mutations
    // during a turn cannot be acknowledged by that turn's completion.
    // Watch generations and last-seen timestamps are deliberately excluded.
    fn input(&self, node: &Node) -> Result<[u8; 32], LogError> {
        use sha2::{Digest, Sha256};
        let revision = node.group_projection_revision()?;
        let consent = node
            .channel_migration
            .lock()
            .expect("channel migration lock")
            .record
            .roots
            .clone();
        let mut routes: Vec<_> = node
            .roster
            .lock()
            .expect("roster lock")
            .peers()
            .into_iter()
            .map(|peer| {
                let mut routes: Vec<_> = peer
                    .routes
                    .iter()
                    .map(|route| route.addr)
                    .chain([peer.addr])
                    .collect();
                routes.sort_unstable();
                routes.dedup();
                (peer.public, peer.account_id, peer.post_office, routes)
            })
            .collect();
        routes.sort_unstable_by_key(|peer| peer.0.user_id());
        // Raw cached hints grant nothing. Calling routing_peers here would
        // evaluate shared-group authority before the producer gate.
        let mut cached = node
            .privacy
            .state
            .read()
            .expect("privacy state lock")
            .as_ref()
            .map(|state| state.routes.routes())
            .unwrap_or_default();
        cached.sort_unstable_by_key(|(public, ip)| (public.ed25519_pub, public.x25519_pub, *ip));
        let capabilities: Vec<_> = self
            .peers
            .iter()
            .map(|(id, value)| {
                (
                    id,
                    &value.public,
                    value.generation,
                    value.supported,
                    value.supported && Instant::now() < value.probe_at,
                    &value.proof,
                    &value.account_hint,
                )
            })
            .collect();
        let bytes = bincode::serialize(&(
            revision,
            consent,
            routes,
            cached,
            capabilities,
            node.sibling_availability_revision(),
        ))
        .map_err(|error| LogError::Serialization(error.to_string()))?;
        Ok(Sha256::digest(bytes).into())
    }

    fn next_deadline(&self, own_account: &str) -> Instant {
        let now = Instant::now();
        let mut deadline = now + REFRESH;
        for value in self.peers.values() {
            deadline = deadline.min(value.probe_at);
            if value.supported {
                deadline = deadline.min(value.binding_poll_at);
                if value.account_hint.as_deref() == Some(own_account) {
                    deadline = deadline.min(value.poll_at);
                }
                if !value.pending.is_empty()
                    || value.after.is_some()
                    || value.binding_after.is_some()
                {
                    deadline = deadline.min(now + Duration::from_secs(3));
                }
            }
        }
        for (_, at) in self.settled.values() {
            deadline = deadline.min(*at);
        }
        deadline.max(now + Duration::from_secs(3))
    }

    fn capability(&mut self, peer: &PeerRecord, generation: u64) -> &mut Capability {
        let mut routes: Vec<_> = peer
            .routes
            .iter()
            .map(|route| route.addr)
            .chain([peer.addr])
            .collect();
        routes.sort_unstable();
        routes.dedup();
        let key = peer.public.user_id();
        if !self.peers.contains_key(&key) && self.peers.len() >= MAX_PEERS {
            self.peers.pop_first();
        }
        let value = self.peers.entry(key).or_insert_with(|| Capability {
            public: peer.public.clone(),
            routes: routes.clone(),
            generation,
            supported: false,
            proof: None,
            account_hint: peer.account_id.clone(),
            probe_at: Instant::now(),
            poll_at: Instant::now(),
            poll_failed: false,
            after: None,
            pending: VecDeque::new(),
            binding_poll_at: Instant::now(),
            binding_failed: false,
            binding_channel: None,
            binding_after: None,
            binding_cursor: 0,
            binding_turn: true,
        });
        if value.public != peer.public
            || value.routes != routes
            || value.generation != generation
            || value.account_hint != peer.account_id
        {
            *value = Capability {
                public: peer.public.clone(),
                routes,
                generation,
                supported: false,
                proof: None,
                account_hint: peer.account_id.clone(),
                probe_at: Instant::now(),
                poll_at: Instant::now(),
                poll_failed: false,
                after: None,
                pending: VecDeque::new(),
                binding_poll_at: Instant::now(),
                binding_failed: false,
                binding_channel: None,
                binding_after: None,
                binding_cursor: 0,
                binding_turn: true,
            };
        }
        value
    }
    fn supports(&self, public: &PublicIdentity, generation: u64) -> bool {
        self.peers.get(&public.user_id()).is_some_and(|value| {
            value.public == *public
                && value.supported
                && value.generation == generation
                && Instant::now() < value.probe_at
        })
    }
}
impl Node {
    /// Explicit kept-source consent for a new account commitment over existing legacy groups.
    pub fn authorize_legacy_channel_migration_if(
        &self,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut committed = false;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let roots =
                self.list_channels()?
                    .into_iter()
                    .try_fold(Vec::new(), |mut roots, row| {
                        if let Some(root) = self.legacy_owner_root(row.id)? {
                            roots.push((row.id, root));
                        }
                        Ok::<_, LogError>(roots)
                    })?;
            self.channel_migration
                .lock()
                .expect("channel migration lock")
                .authorize(roots)?;
            committed = true;
            Ok(())
        })?;
        if !committed {
            return Err(NodeError::Authorization(
                "channel migration consent was not authorized".into(),
            ));
        }
        self.channel_recovery_notify.notify_one();
        self.note_ui_change();
        Ok(())
    }
    pub(super) fn record_channel_owner_origin(
        &self,
        channel: ConversationId,
    ) -> Result<(), NodeError> {
        if let Some(root) = self.legacy_owner_root(channel)? {
            self.channel_migration
                .lock()
                .expect("channel migration lock")
                .authorize(vec![(channel, root)])?;
        }
        Ok(())
    }
    fn legacy_owner_root(
        &self,
        channel: ConversationId,
    ) -> Result<Option<crate::eventlog::EventId>, LogError> {
        let projection = self.account_group_projection(channel)?;
        if projection.authority.current_event().is_some()
            || projection
                .state
                .as_ref()
                .is_none_or(|state| state.is_dissolved() || state.owner() != self.user_id())
        {
            return Ok(None);
        }
        Ok(projection
            .accepted
            .iter()
            .find(|member| member.public == self.identity.public())
            .map(|member| member.membership_event))
    }
    /// One peer turn and one local channel turn; the existing recovery task owns all state.
    pub(super) async fn maintain_account_channels(&self, work: &mut ChannelAccountMaintenance) {
        let selected_input = work.input(self).ok();
        if selected_input.is_some()
            && work.input == selected_input
            && Instant::now() < work.deadline()
        {
            return;
        }
        // New semantic inputs invalidate successful inventory comparisons, but
        // ordinary notifications and independent changes never erase failures.
        if selected_input.is_some()
            && work.observed_input.is_some()
            && work.observed_input != selected_input
        {
            let now = Instant::now();
            for value in work.peers.values_mut().filter(|value| value.supported) {
                if !value.poll_failed {
                    value.poll_at = now;
                }
                if !value.binding_failed {
                    value.binding_poll_at = now;
                }
            }
        }
        if selected_input.is_some() {
            work.observed_input = selected_input;
        }
        // Capture before work. Mutations during this turn remain dirty.
        let mut cycle_complete = false;
        let result: Result<(), NodeError> = async {
            let mut peers = self.routing_peers();
            // Capability maintenance serves existing group relationships and siblings,
            // not every discovery entry. Unrelated probes compete with ordinary DMs
            // and must not initiate a pull connection before contact authorization.
            let mut selected = Vec::new();
            for peer in peers {
                if !peer.post_office
                    && (peer.account_id.as_deref() == Some(self.account_id().as_str())
                        || self.group_member(&peer.public)?
                        || self.group_departure_peer(&peer.public))
                {
                    selected.push(peer);
                }
            }
            peers = selected;
            work.peers
                .retain(|_, value| peers.iter().any(|peer| peer.public == value.public));
            peers.sort_unstable_by_key(|peer| peer.public.user_id());
            // Seed only cheap bounded metadata for every admitted candidate;
            // an unvisited new peer must retain its initial probe deadline.
            let generation = self.privacy.generation.load(Ordering::Acquire);
            for peer in &peers {
                work.capability(peer, generation);
            }
            if !peers.is_empty() {
                let peer = &peers[work.peer_cursor % peers.len()];
                work.peer_cursor = work.peer_cursor.wrapping_add(1);
                let value = work.capability(peer, self.privacy.generation.load(Ordering::Acquire));
                if Instant::now() >= value.probe_at {
                    value.proof = self
                        .query_account_groups_verified(peer, AccountGroupRequest::Capability)
                        .await
                        .and_then(|(response, proof)| {
                            (matches!(
                                response,
                                AccountGroupResponse::Capability { version: 1, .. }
                            ) && proof.public() == peer.public
                                && proof.valid_presence()
                                && proof.account_id().is_some()
                                && proof.account_id() == peer.account_id
                                && !proof.post_office)
                                .then_some(proof)
                        });
                    value.supported = value.proof.is_some();
                    value.probe_at = Instant::now() + if value.supported { REFRESH } else { RETRY };
                } else if value.supported && self.pull_group_binding_page(peer, value).await? {
                    // A relay page consumes this peer turn; sibling route recovery
                    // remains independent and resumes on its next selected turn.
                } else if value.supported
                    && peer.account_id.as_deref() == Some(self.account_id().as_str())
                {
                    if value.pending.is_empty() && Instant::now() >= value.poll_at {
                        if let Some(AccountGroupResponse::Groups { channels, next }) = self
                            .query_account_groups(
                                peer,
                                AccountGroupRequest::Groups {
                                    account_id: self.account_id(),
                                    after: value.after,
                                },
                            )
                            .await
                        {
                            value.poll_failed = false;
                            value.pending = channels.into();
                            value.after = next;
                            value.poll_at = Instant::now()
                                + if next.is_some() {
                                    Duration::ZERO
                                } else {
                                    REFRESH
                                };
                        } else {
                            value.poll_failed = true;
                            value.poll_at = Instant::now() + RETRY;
                        }
                    }
                    let generation = value.generation;
                    if let Some(channel) = value.pending.pop_front() {
                        let pulled = tokio::time::timeout(Duration::from_secs(15), async {
                            let mut connection =
                                self.privacy_sync_dial(peer.addr, &peer.public).await.ok()?;
                            let store = self.pull_sync_store(connection.peer_identity());
                            super::session::request_round_paged(&mut connection, &store, channel)
                                .await
                                .ok()?;
                            self.recycle_sync_channel(connection);
                            self.process_channel(channel);
                            Some(())
                        })
                        .await;
                        let maintained = self.maintain_one_account_channel(channel, work).await;
                        if (!matches!(pulled, Ok(Some(()))) || !maintained)
                            && let Some(value) = work.peers.get_mut(&peer.public.user_id())
                            && value.generation == generation
                        {
                            // This bounded inventory slot remains work until its
                            // signed controls are ready, even before list_channels
                            // can expose the newly recovered group.
                            value.pending.push_back(channel);
                        }
                    }
                }
            }
            let channels = self.list_channels()?;
            work.settled
                .retain(|id, _| channels.iter().any(|channel| channel.id.as_bytes() == id));
            cycle_complete = channels.is_empty();
            if !channels.is_empty() {
                let channel = channels[work.channel_cursor % channels.len()].id;
                work.channel_cursor = work.channel_cursor.wrapping_add(1);
                let ready = self.maintain_one_account_channel(channel, work).await;
                let completed_input = work.input(self).ok();
                let now = Instant::now();
                cycle_complete = ready
                    && completed_input.is_some()
                    && channels.iter().all(|row| {
                        work.settled
                            .get(row.id.as_bytes())
                            .is_some_and(|(input, until)| {
                                Some(*input) == completed_input && *until > now
                            })
                    });
            }

            Ok(())
        }
        .await;
        work.run_at = Some(
            if result.is_ok()
                && cycle_complete
                && selected_input.is_some()
                && work.input(self).ok() == selected_input
                && !work.peers.values().any(|peer| {
                    peer.supported
                        && (!peer.pending.is_empty()
                            || peer.after.is_some()
                            || peer.binding_after.is_some())
                })
            {
                work.input = selected_input;
                work.next_deadline(&self.account_id())
            } else {
                work.input = None;
                Instant::now() + Duration::from_secs(3)
            },
        );
        if let Err(error) = result {
            log::warn!("maintain_account_channels deferred after storage failure: {error}");
        }
    }
    async fn pull_group_binding_page(
        &self,
        peer: &PeerRecord,
        work: &mut Capability,
    ) -> Result<bool, NodeError> {
        if !work.binding_turn {
            work.binding_turn = true;
            return Ok(false);
        }
        if Instant::now() < work.binding_poll_at {
            return Ok(false);
        }
        let Some(proof) = work.proof.as_ref() else {
            return Ok(false);
        };
        let mut groups = Vec::new();
        for row in self.list_channels()? {
            if self.binding_transition_group_member(row.id, &peer.public, proof)? {
                groups.push(row.id);
            }
        }
        groups.sort_unstable_by_key(|id| *id.as_bytes());
        if groups.is_empty() {
            work.binding_failed = false;
            work.binding_poll_at = Instant::now() + REFRESH;
            work.binding_channel = None;
            work.binding_after = None;
            return Ok(false);
        }
        if work
            .binding_channel
            .is_none_or(|channel| !groups.contains(&channel))
        {
            work.binding_channel = Some(groups[work.binding_cursor % groups.len()]);
            work.binding_after = None;
        }
        let channel = work.binding_channel.expect("selected shared group");
        work.binding_turn = false;
        let requested_after = work.binding_after.clone();
        match self
            .query_account_groups(
                peer,
                AccountGroupRequest::BindingTransitions {
                    channel,
                    after: requested_after.clone(),
                },
            )
            .await
        {
            Some(AccountGroupResponse::BindingTransitions { patches, next })
                if patches.iter().all(|patch| {
                    requested_after
                        .as_ref()
                        .is_none_or(|after| patch.announce.public().user_id() > *after)
                }) =>
            {
                work.binding_failed = false;
                for patch in patches {
                    let _ = self.apply_relayed_account_transition(&patch, channel);
                }
                work.binding_after = next;
                if work.binding_after.is_none() {
                    work.binding_channel = None;
                    work.binding_cursor = work.binding_cursor.wrapping_add(1);
                    work.binding_poll_at = Instant::now() + REFRESH;
                }
            }
            _ => {
                // Older Capability1 peers may close on the additive request.
                // Keep their existing group migration and route support enabled.
                work.binding_channel = None;
                work.binding_after = None;
                work.binding_failed = true;
                work.binding_poll_at = Instant::now() + RETRY;
            }
        }
        Ok(true)
    }
    async fn maintain_one_account_channel(
        &self,
        channel: ConversationId,
        work: &mut ChannelAccountMaintenance,
    ) -> bool {
        #[cfg(test)]
        {
            let hook = self
                .account_channel_maintenance_test_hook
                .lock()
                .unwrap()
                .clone();
            if let Some(hook) = hook
                && !hook(channel)
            {
                // Defer only this mutation. The same recovery task must still
                // prepare metadata and service its other channel work.
                return false;
            }
        }
        let selected_input = work.input(self).ok();
        if let Some(input) = selected_input
            && work
                .settled
                .get(channel.as_bytes())
                .is_some_and(|(previous, until)| *previous == input && Instant::now() < *until)
        {
            return true;
        }
        let policy = self.privacy.gate.read().await;
        let result = {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            self.commit_account_maintenance(channel, work)
        };
        drop(policy);
        if result.is_ok()
            && let Some(input) = selected_input
        {
            let now = Instant::now();
            let until = work
                .refresh_after
                .iter()
                .filter(|((id, _), at)| *id == *channel.as_bytes() && **at > now)
                .map(|(_, at)| *at)
                .min()
                .unwrap_or(now + REFRESH);
            if work.settled.len() >= MAX_MIGRATIONS
                && !work.settled.contains_key(channel.as_bytes())
            {
                work.settled.pop_first();
            }
            work.settled.insert(*channel.as_bytes(), (input, until));
        }
        match result {
            Ok(Some(members)) => {
                self.distribute_channel(channel, &members).await;
                true
            }
            Ok(None) => true,
            Err(NodeError::Log(LogError::StaleCursor)) => false,
            Err(error) => {
                log::warn!("account channel maintenance deferred: {error}");
                false
            }
        }
    }
    fn commit_account_maintenance(
        &self,
        channel: ConversationId,
        work: &mut ChannelAccountMaintenance,
    ) -> Result<Option<Vec<PublicIdentity>>, NodeError> {
        self.commit_account_maintenance_with_budget(
            channel,
            work,
            &mut crate::eventlog::sync::network::PreparationBudget::default(),
        )
    }
    fn commit_account_maintenance_with_budget(
        &self,
        channel: ConversationId,
        work: &mut ChannelAccountMaintenance,
        preparation: &mut crate::eventlog::sync::network::PreparationBudget,
    ) -> Result<Option<Vec<PublicIdentity>>, NodeError> {
        // Reuse one bounded admission allowance for the selected channel and
        // every locally appended control tail. Pending before any append remains
        // retryable; an accepted control retains its explicit fanout outcome.
        self.prepare_channel_admission(channel, preparation)?;
        let own = self.own_group_proof();
        let me = self.identity.public();
        #[cfg(test)]
        {
            work.projection_reads += 1;
        }
        let mut projection = self.account_group_projection(channel)?;
        let Some(state) = projection.state.as_ref() else {
            return Ok(None);
        };
        if state.is_dissolved() {
            return Ok(None);
        }
        let mut changed = false;
        if projection.authority.current_event().is_none() {
            if state.owner() != self.user_id() || !projection.invited.is_empty() {
                return Ok(None);
            }
            let Some(root) = projection
                .accepted
                .iter()
                .find(|member| member.public == me)
                .map(|member| member.membership_event)
            else {
                return Ok(None);
            };
            if !self
                .channel_migration
                .lock()
                .expect("channel migration lock")
                .permits(channel, root)
            {
                return Ok(None);
            }
            let routing = self.routing_peers();
            let mut bindings = Vec::new();
            let mut grants = BTreeMap::new();
            for member in projection
                .accepted
                .iter()
                .filter(|member| state.is_accepted_member(&member.public.user_id()))
            {
                let proof = if member.public == me {
                    Some(own.clone())
                } else {
                    let peer = routing.iter().find(|peer| peer.public == member.public);
                    let cached = work.peers.get(&member.public.user_id());
                    if !work.supports(
                        &member.public,
                        self.privacy.generation.load(Ordering::Acquire),
                    ) || peer.is_some_and(|peer| {
                        cached.is_none_or(|cached| cached.account_hint != peer.account_id)
                    }) {
                        return Ok(None);
                    }
                    cached.and_then(|cached| cached.proof.clone())
                };
                let Some(proof) = proof.filter(|proof| {
                    proof.valid_presence() && !proof.post_office && proof.public() == member.public
                }) else {
                    return Ok(None);
                };
                let Some(cert) = proof.account_cert.as_ref() else {
                    return Ok(None);
                };
                grants
                    .entry(cert.account_ed25519_pub)
                    .or_insert_with(|| AccountGrant {
                        incarnation: {
                            let mut bytes = [0; 16];
                            rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut bytes);
                            bytes
                        },
                        pending: false,
                        original: proof.clone(),
                        membership_event: member.membership_event,
                        acceptance_event: member.acceptance_event,
                    });
                bindings.push(proof);
            }
            let control = AccountAuthorityControl {
                version: 1,
                account_signature: Vec::new(),
                previous: None,
                issuer: own.clone(),
                owner_account: self.account.public().ed25519_pub,
                grants: grants.into_values().collect(),
                revoked: Vec::new(),
                legacy_bindings: bindings,
            };
            let payload = control.signed_encode(channel, &self.account);
            if payload.len() > MAX_AUTHORITY_BYTES {
                return Ok(None);
            }
            self.append_event(channel, EventKind::ChannelAccountAuthority, payload)?;
            match self.prepare_channel_admission(channel, preparation) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => {
                    // The control is already durable. Hand its recipients to
                    // the existing async fanout, which finishes selected
                    // preparation after releasing membership/privacy guards.
                    return Ok(projection
                        .state
                        .as_ref()
                        .map(|state| state.members().to_vec()));
                }
                Err(error) => return Err(error.into()),
            }
            projection = self.account_group_projection(channel)?;
            changed = true;
        }
        let Some(authority) = projection.authority.current_event() else {
            return Ok(None);
        };
        let state = projection
            .state
            .as_ref()
            .expect("authority has channel state");
        if !state.is_member(&self.user_id()) {
            let Some(grant) = projection
                .authority
                .account_grant(&self.account.public().ed25519_pub)
                .and_then(AccountGrant::id)
            else {
                return Ok(None);
            };
            let control = DeviceControl::Inherit {
                authority,
                grant,
                device: own,
                account_signature: self
                    .account
                    .sign(&inheritance_account_message(channel, authority, grant, &me))
                    .to_vec(),
            };
            self.append_event(
                channel,
                EventKind::ChannelDeviceInheritance,
                control.encode(),
            )?;
            match self.prepare_channel_admission(channel, preparation) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => {
                    // The control is already durable. Hand its recipients to
                    // the existing async fanout, which finishes selected
                    // preparation after releasing membership/privacy guards.
                    return Ok(projection
                        .state
                        .as_ref()
                        .map(|state| state.members().to_vec()));
                }
                Err(error) => return Err(error.into()),
            }
            projection = self.account_group_projection(channel)?;
            if !projection
                .state
                .as_ref()
                .is_some_and(|state| state.is_member(&self.user_id()))
            {
                return Ok(None);
            }
            changed = true;
        }
        // An inherited genuine account invitation remains pending until accepted.
        // Derive repair from durable authority/sender state so a budget yield
        // after inheritance cannot permanently skip sender installation.
        let state = projection
            .state
            .as_ref()
            .expect("authority has channel state");
        if state.is_accepted_member(&self.user_id())
            && projection
                .authority
                .inherited_device_proofs()
                .into_iter()
                .any(|(_, _, proof)| proof.public() == me)
            && self
                .channel_senders
                .lock()
                .expect("sender store lock")
                .current(&channel, state.epoch())?
                .is_none()
        {
            self.commit_my_sender_key(channel, state.members())?;
            match self.prepare_channel_admission(channel, preparation) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => {
                    // The control is already durable. Hand its recipients to
                    // the existing async fanout, which finishes selected
                    // preparation after releasing membership/privacy guards.
                    return Ok(projection
                        .state
                        .as_ref()
                        .map(|state| state.members().to_vec()));
                }
                Err(error) => return Err(error.into()),
            }
            projection = self.account_group_projection(channel)?;
            changed = true;
        }
        if self.share_account_receive_keys(channel, &projection, work, preparation)? {
            changed = true;
        }
        Ok(if changed {
            projection.state.map(|state| state.members().to_vec())
        } else {
            None
        })
    }
    fn share_account_receive_keys(
        &self,
        channel: ConversationId,
        projection: &super::channel::ChannelMembershipProjection,
        work: &mut ChannelAccountMaintenance,
        preparation: &mut crate::eventlog::sync::network::PreparationBudget,
    ) -> Result<bool, NodeError> {
        let Some(state) = projection.state.as_ref().filter(|state| {
            state.is_accepted_member(&self.user_id())
                && state.member_account_id(&self.user_id()) == Some(self.account_id().as_str())
        }) else {
            return Ok(false);
        };
        let Some(authority) = projection.authority.current_event() else {
            return Ok(false);
        };
        if projection
            .authority
            .grant_for_device(&self.identity.public(), &self.own_group_proof())
            .is_none()
        {
            return Ok(false);
        }
        let mut recipients = Vec::new();
        for (grant, inheritance, proof) in projection.authority.inherited_device_proofs() {
            if proof.public() != self.identity.public()
                && proof.account_id().as_deref() == Some(self.account_id().as_str())
                && state.is_accepted_member(&proof.user_id)
                && projection.is_authorized(inheritance)?
            {
                recipients.push((grant, inheritance, proof));
            }
        }
        if recipients.is_empty() {
            return Ok(false);
        }
        let source = self
            .channel_checkpoints
            .source_binding(channel)?
            .ok_or(LogError::StaleCursor)?;
        let mut snapshots = Vec::new();
        for member in state
            .members()
            .iter()
            .filter(|member| state.is_accepted_member(&member.user_id()))
        {
            #[cfg(test)]
            {
                work.distribution_reads += 1;
            }
            let distribution = if member.user_id() == self.user_id() {
                self.channel_senders
                    .lock()
                    .expect("sender store lock")
                    .current(&channel, state.epoch())?
                    .map(zeroize::Zeroizing::new)
                    .map(|bytes| {
                        crate::channel::sender_key::SenderKey::deserialize(&bytes)
                            .map(|sender| sender.distribution())
                            .ok_or_else(|| {
                                LogError::CorruptFile("invalid current channel sender".into())
                            })
                    })
                    .transpose()?
            } else {
                self.channel_checkpoints.receiver_distribution(
                    &source,
                    &member.user_id(),
                    state.epoch(),
                )?
            };
            if let Some(distribution) = distribution {
                snapshots.push(ReceiveDistribution {
                    author: member.clone(),
                    epoch: state.epoch(),
                    distribution,
                });
            }
        }
        let mut selected = None;
        for (grant, inheritance, proof) in recipients {
            let recipient = proof.public();
            let shipped = self
                .channel_checkpoints
                .shipped_distributions(&source, state, grant, &recipient)?;
            let key = (*channel.as_bytes(), recipient.ed25519_pub);
            let refresh = work
                .refresh_after
                .get(&key)
                .is_none_or(|deadline| Instant::now() >= *deadline);
            if snapshots
                .iter()
                .any(|value| snapshot_needed(value, &recipient, &shipped, refresh))
            {
                selected = Some((inheritance, grant, recipient, shipped, refresh));
                break;
            }
        }
        self.channel_checkpoints.check_source(&source)?;
        let Some((inheritance, grant, recipient, shipped, refresh)) = selected else {
            return Ok(false);
        };
        let values: Vec<_> = snapshots
            .into_iter()
            .filter(|value| {
                value.epoch == state.epoch()
                    && snapshot_needed(value, &recipient, &shipped, refresh)
            })
            .collect();
        let mut changed = false;
        let mut source = source;
        for chunk in values.chunks(64).take(4) {
            let Some(bytes) = ReceiveDistribution::encode_batch(chunk) else {
                continue;
            };
            let bytes = zeroize::Zeroizing::new(bytes);
            let sealed = crate::dm::seal(&self.identity, &recipient.x25519_pub, &bytes)
                .map_err(|_| NodeError::Channel("receive-key sealing failed".into()))?;
            let control = DeviceControl::ReceiveKeys {
                authority,
                inheritance,
                grant,
                recipient: recipient.clone(),
                epoch: state.epoch(),
                authors: chunk.iter().map(|item| item.author.clone()).collect(),
                positions: chunk.iter().map(|item| item.distribution.n).collect(),
                sealed,
            };
            self.append_channel_control_from_source(
                &source,
                EventKind::ChannelDeviceInheritance,
                control.encode(),
            )?;
            changed = true;
            match self.prepare_channel_admission(channel, preparation) {
                Ok(()) => {}
                Err(LogError::StaleCursor) => return Ok(true),
                Err(error) => return Err(error.into()),
            }
            let next = self.account_group_projection(channel)?;
            if next.authority.current_event() != Some(authority)
                || !next
                    .authority
                    .grants()
                    .iter()
                    .any(|candidate| candidate.id() == Some(grant))
                || !next.state.as_ref().is_some_and(|current| {
                    current.epoch() == state.epoch()
                        && current.is_accepted_member(&recipient.user_id())
                        && current.is_accepted_member(&self.user_id())
                })
            {
                break;
            }
            source = self
                .channel_checkpoints
                .source_binding(channel)?
                .ok_or(LogError::StaleCursor)?;
        }
        if changed {
            let key = (*channel.as_bytes(), recipient.ed25519_pub);
            if work.refresh_after.len() >= MAX_PEERS && !work.refresh_after.contains_key(&key) {
                work.refresh_after.pop_first();
            }
            work.refresh_after
                .insert(key, Instant::now() + Duration::from_secs(30));
        }
        Ok(changed)
    }
}
fn snapshot_needed(
    value: &ReceiveDistribution,
    recipient: &PublicIdentity,
    shipped: &BTreeMap<String, u32>,
    refresh: bool,
) -> bool {
    value.author != *recipient
        && shipped.get(&value.author.user_id()).is_none_or(|position| {
            refresh
                && value.distribution.n.saturating_sub(*position)
                    > crate::channel::sender_key::MAX_SKIP
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::{Announce, Roster},
        identity::{account::Account, device::DeviceIdentity},
    };
    use std::sync::Arc;
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::Mutex,
    };
    use tokio::{net::TcpListener, sync::mpsc, task::JoinHandle};

    struct Group {
        _dir: tempfile::TempDir,
        nodes: Vec<Arc<Node>>,
        servers: Vec<JoinHandle<()>>,
    }
    impl Drop for Group {
        fn drop(&mut self) {
            for server in &self.servers {
                server.abort();
            }
        }
    }
    impl Group {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let shared = Account::generate().secret_bytes();
            let mut nodes = Vec::new();
            let mut listeners = Vec::new();
            let mut addresses: Vec<SocketAddr> = Vec::new();
            for index in 0..4 {
                let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                addresses.push(listener.local_addr().unwrap());
                listeners.push(listener);
                let (dm, _) = mpsc::unbounded_channel();
                let (channel, _) = mpsc::unbounded_channel();
                let (file, _) = mpsc::unbounded_channel();
                let account = if index == 1 || index == 2 {
                    Account::from_secret_bytes(shared)
                } else {
                    Account::generate()
                };
                let node_dir = dir.path().join(index.to_string());
                std::fs::create_dir(&node_dir).unwrap();
                let node = Node::open_with_account(
                    DeviceIdentity::generate(),
                    account,
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &node_dir.join("events.log"),
                    &node_dir.join("sent.log"),
                    "pw",
                )
                .unwrap();
                node.set_post_office_enabled(false);
                nodes.push(node);
            }
            for from in 0..4 {
                for to in 0..4 {
                    if from != to {
                        nodes[from].roster.lock().unwrap().update(
                            &Announce::new_with_account(
                                &nodes[to].identity,
                                &nodes[to].account,
                                "member",
                                addresses[to].port(),
                            ),
                            IpAddr::V4(Ipv4Addr::LOCALHOST),
                            &nodes[from].user_id(),
                        );
                    }
                }
            }
            for (owner, other) in [(&nodes[1], &nodes[2]), (&nodes[2], &nodes[1])] {
                owner
                    .account_devices
                    .lock()
                    .unwrap()
                    .insert(super::super::account_devices::AccountDevice {
                        public: other.identity.public(),
                        certificate: owner.account.certify(&other.identity.public().ed25519_pub),
                    })
                    .unwrap();
            }
            let servers = listeners
                .into_iter()
                .zip(&nodes)
                .map(|(listener, node)| {
                    let node = Arc::clone(node);
                    tokio::spawn(async move {
                        let mut connections = tokio::task::JoinSet::new();
                        loop {
                            let (stream, _) = listener.accept().await.unwrap();
                            let node = Arc::clone(&node);
                            connections.spawn(async move {
                                if let Ok(channel) = node.privacy_accept(stream).await {
                                    node.serve_connection(channel).await;
                                }
                            });
                            while connections.try_join_next().is_some() {}
                        }
                    })
                })
                .collect();
            Self {
                _dir: dir,
                nodes,
                servers,
            }
        }
    }

    #[tokio::test]
    async fn group_member_relays_account_transition_while_original_device_is_offline() {
        use crate::{
            discovery::visibility::DiscoveryVisibility,
            identity::account_transition::AccountBindingTransition,
        };
        let group = Group::new().await;
        let relay = &group.nodes[0];
        let moved = &group.nodes[1];
        let receiver = &group.nodes[3];
        let channel = relay
            .create_channel(
                "relay scope",
                vec![moved.identity.public(), receiver.identity.public()],
            )
            .await
            .unwrap();
        let members = [
            relay.identity.public(),
            moved.identity.public(),
            receiver.identity.public(),
        ];
        for _ in 0..40 {
            if receiver
                .account_group_projection(channel)
                .unwrap()
                .state
                .as_ref()
                .is_some_and(|state| {
                    members
                        .iter()
                        .all(|member| state.is_accepted_member(&member.user_id()))
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let old = Account::generate();
        let before = Announce::new_with_account(&moved.identity, &old, "Before linking", 1234);
        let after = moved.signed_announce("After linking", 1234);
        let transition =
            AccountBindingTransition::create(&old, &moved.account, &moved.identity, None);
        for index in [0, 3] {
            let node = &group.nodes[index];
            let port = group.nodes[(index + 1) % group.nodes.len()]
                .roster
                .lock()
                .unwrap()
                .get(&node.user_id())
                .unwrap()
                .addr
                .port();
            node.configure_privacy(
                &group._dir.path().join(index.to_string()),
                "pw",
                &node.signed_announce("member", port),
                Arc::new(DiscoveryVisibility::new(true)),
            )
            .unwrap();
            let mut guard = node.privacy.state.write().unwrap();
            let state = guard.as_mut().unwrap();
            state.proofs.record(&before).unwrap();
            if index == 0 {
                state
                    .proofs
                    .record_transition(&after, std::slice::from_ref(&transition))
                    .unwrap();
            }
        }
        // No connection to the changed device is possible during recovery.
        group.servers[1].abort();
        receiver
            .roster
            .lock()
            .unwrap()
            .forget(&moved.identity.public());
        assert!(
            receiver
                .privacy
                .state
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .proofs
                .accepts_binding(&before)
        );
        let relay_peer = receiver
            .roster
            .lock()
            .unwrap()
            .get(&relay.user_id())
            .unwrap()
            .clone();
        let response = receiver
            .query_account_groups(
                &relay_peer,
                AccountGroupRequest::BindingTransitions {
                    channel,
                    after: None,
                },
            )
            .await;
        let Some(AccountGroupResponse::BindingTransitions { patches, next }) = response else {
            panic!("an accepted member must receive the existing group's public transition page");
        };
        assert!(next.is_none());
        assert_eq!(patches.len(), 1);
        assert_eq!(patches[0].announce.public(), moved.identity.public());
        let mut work = ChannelAccountMaintenance::default();
        for _ in 0..8 {
            receiver.maintain_account_channels(&mut work).await;
            if receiver
                .privacy
                .state
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .proofs
                .accepts_binding(&after)
            {
                break;
            }
        }
        {
            let guard = receiver.privacy.state.read().unwrap();
            let proofs = &guard.as_ref().unwrap().proofs;
            assert!(proofs.accepts_binding(&after));
            assert!(!proofs.accepts_binding(&before));
        }
        assert!(
            receiver
                .roster
                .lock()
                .unwrap()
                .get(&moved.user_id())
                .is_none(),
            "relayed historical proof cannot invent online presence"
        );
        assert!(
            !relay
                .binding_transition_group_member(
                    channel,
                    &group.nodes[2].identity.public(),
                    &group.nodes[2].own_group_proof()
                )
                .unwrap(),
            "a certified sibling without accepted device membership cannot query another device's group"
        );
        let wrong_x = DeviceIdentity::from_secret_bytes(
            receiver.identity.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        assert!(
            !relay
                .binding_transition_group_member(
                    channel,
                    &wrong_x.public(),
                    &Announce::new_with_account(&wrong_x, &receiver.account, "wrong X", 1234)
                )
                .unwrap()
        );
        assert!(
            !relay
                .binding_transition_group_member(
                    ConversationId::new([255; 32]),
                    &receiver.identity.public(),
                    &receiver.own_group_proof()
                )
                .unwrap()
        );
        group.servers[2].abort();
        relay
            .add_channel_member(channel, group.nodes[2].identity.public())
            .await
            .unwrap();
        let projection = relay.account_group_projection(channel).unwrap();
        let state = projection.state.unwrap();
        assert!(state.is_member(&group.nodes[2].user_id()));
        assert!(!state.is_accepted_member(&group.nodes[2].user_id()));
        assert!(
            !relay
                .binding_transition_group_member(
                    channel,
                    &group.nodes[2].identity.public(),
                    &group.nodes[2].own_group_proof()
                )
                .unwrap(),
            "pending invitation cannot authorize relay access"
        );
        relay
            .remove_channel_member(channel, &receiver.user_id())
            .await
            .unwrap();
        assert!(
            !relay
                .binding_transition_group_member(
                    channel,
                    &receiver.identity.public(),
                    &receiver.own_group_proof()
                )
                .unwrap(),
            "removed membership cannot retain relay access"
        );
    }

    #[tokio::test]
    async fn new_account_group_membership_and_ownership_inherit_over_certified_sibling_sync() {
        use crate::node::AccountGroupRole;
        let group = Group::new().await;
        let owner = &group.nodes[0];
        // The account resolver selects a stable available device, not the
        // fixture's arbitrary array index. Accept on that real invitation route.
        let (source, target) = if group.nodes[1].user_id() < group.nodes[2].user_id() {
            (&group.nodes[1], &group.nodes[2])
        } else {
            (&group.nodes[2], &group.nodes[1])
        };
        let member_group = owner
            .create_account_channel_invited_if(
                "Account membership",
                vec![source.account_id()],
                |commit| commit(),
            )
            .await
            .unwrap();
        source.accept_channel_invite(member_group).await.unwrap();
        let owned_group = source
            .create_account_channel_invited_if(
                "Account ownership",
                vec![group.nodes[3].account_id()],
                |commit| commit(),
            )
            .await
            .unwrap();
        // Only the accepted sibling is reachable; neither group owner approves
        // another device. Discover and pull through the real certified protocol.
        group.servers[0].abort();
        for node in &group.nodes[1..] {
            node.roster.lock().unwrap().forget(&owner.identity.public());
        }
        let mut target_work = ChannelAccountMaintenance::default();
        let mut source_work = ChannelAccountMaintenance::default();
        for _ in 0..16 {
            target.maintain_account_channels(&mut target_work).await;
            source.maintain_account_channels(&mut source_work).await;
        }
        assert_eq!(
            target.account_group_access(member_group).unwrap().role,
            AccountGroupRole::Member
        );
        assert_eq!(
            target.account_group_access(owned_group).unwrap().role,
            AccountGroupRole::Owner
        );
        assert!(target.channel_can_manage(owned_group).unwrap());
        assert_eq!(
            target
                .channel_member_account_sets(member_group)
                .unwrap()
                .0
                .len(),
            2
        );
        assert!(
            target
                .account_group_projection(member_group)
                .unwrap()
                .authority
                .invitation_for_device(&target.identity.public(), &target.own_group_proof())
                .is_none()
        );
    }

    #[tokio::test]
    async fn late_initial_receive_keys_repair_an_already_present_message_through_history() {
        use bincode::Options;
        let group = Group::new().await;
        let source = &group.nodes[1];
        let target = &group.nodes[2];
        let channel = source
            .create_account_channel_invited_if(
                "late receive keys",
                vec![group.nodes[3].account_id()],
                |commit| commit(),
            )
            .await
            .unwrap();
        source
            .send_channel_message(channel, b"before inheritance")
            .await
            .unwrap();
        let mut target_work = ChannelAccountMaintenance::default();
        for _ in 0..16 {
            target.maintain_account_channels(&mut target_work).await;
            if target
                .account_group_projection(channel)
                .unwrap()
                .state
                .is_some_and(|state| state.is_accepted_member(&target.user_id()))
            {
                break;
            }
        }
        assert!(
            target
                .account_group_projection(channel)
                .unwrap()
                .state
                .unwrap()
                .is_accepted_member(&target.user_id())
        );
        let absent = {
            let binding = target
                .channel_checkpoints
                .source_binding(channel)
                .unwrap()
                .unwrap();
            let epoch = target
                .channel_checkpoints
                .current_state(channel)
                .unwrap()
                .unwrap()
                .epoch();
            let mut distribution = target
                .channel_checkpoints
                .receiver_distribution(&binding, &source.user_id(), epoch)
                .unwrap();
            let absent = distribution.is_none();
            if let Some(value) = distribution.as_mut() {
                zeroize::Zeroize::zeroize(&mut value.chain_key);
            }
            absent
        };
        assert!(absent);
        // This is the ordinary account history worker, without channel recovery:
        // the source's first ReceiveKeys publication remains deliberately pending.
        let wake_ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_wake = Arc::clone(&wake_ready);
        let sibling = target.identity.public();
        *source.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, event| {
            if peer == sibling && event.starts_with("wake_send_ack revision=") {
                observed_wake.store(true, Ordering::Release);
            }
        }));
        let history = tokio::spawn(Arc::clone(target).run_account_history_loop());
        let source_history = tokio::spawn(Arc::clone(source).run_account_history_loop());
        let source_wake = tokio::spawn(Arc::clone(source).run_account_wake_loop());
        struct AbortHistory(Vec<tokio::task::AbortHandle>);
        impl Drop for AbortHistory {
            fn drop(&mut self) {
                for task in &self.0 {
                    task.abort();
                }
            }
        }
        let _history_cleanup = AbortHistory(vec![
            history.abort_handle(),
            source_history.abort_handle(),
            source_wake.abort_handle(),
        ]);
        tokio::time::timeout(Duration::from_secs(8), async {
            while !wake_ready.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the real source hint owner must complete its first authenticated sweep");
        let old_history = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if target
                    .channel_history(channel, 20)
                    .unwrap()
                    .iter()
                    .any(|row| row.text == b"before inheritance")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        if old_history.is_err() {
            for task in [history, source_history, source_wake] {
                task.abort();
                let _ = tokio::time::timeout(Duration::from_secs(3), task)
                    .await
                    .expect("history fixture owners must retire after abort");
            }
            panic!("initial real history must arrive");
        }
        let old_message = source
            .channel_history(channel, 20)
            .unwrap()
            .into_iter()
            .find(|row| row.text == b"before inheritance")
            .unwrap()
            .id;
        let held = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let held_hook = Arc::clone(&held);
        let release_hook = Arc::clone(&release);
        let recipient = target.identity.public();
        *source.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |peer, frame| {
            let matching = peer == recipient
                && matches!(frame,
                super::super::account_sync_wire::AccountHistoryFrame::ChannelHeader {
                    channel: current, event_id, ..
                } if current == *channel.as_bytes() && event_id != *old_message.as_bytes());
            if matching {
                held_hook.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            let release = Arc::clone(&release_hook);
            Box::pin(async move {
                if matching {
                    // Closing the gate releases every in-flight response,
                    // including legitimate retries of the same header.
                    let _ = release.acquire().await;
                }
            })
        }));
        struct ReleaseHistory(Arc<tokio::sync::Semaphore>);
        impl Drop for ReleaseHistory {
            fn drop(&mut self) {
                self.0.close();
            }
        }
        let _release_cleanup = ReleaseHistory(Arc::clone(&release));
        source
            .send_channel_message(channel, b"after inheritance before first keys")
            .await
            .unwrap();
        let message = source
            .channel_history(channel, 20)
            .unwrap()
            .into_iter()
            .find(|row| row.text == b"after inheritance before first keys")
            .unwrap()
            .id;
        let (epoch, author, position): (u64, String, u32) = {
            let log = target.log.lock().unwrap();
            let event = log
                .get(&message)
                .unwrap()
                .expect("actual encrypted message arrived before initial keys");
            let size = u16::from_be_bytes(event.ciphertext[..2].try_into().unwrap()) as usize;
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .deserialize(&event.ciphertext[2..2 + size])
                .unwrap()
        };
        assert!(
            target
                .account_group_projection(channel)
                .unwrap()
                .is_authorized(message)
                .unwrap()
        );
        assert_eq!(author, source.user_id());
        let header_held = tokio::time::timeout(Duration::from_secs(8), async {
            while !held.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            header_held.is_ok(),
            "ordinary history request must reach the exact response gate"
        );
        assert!(
            !target
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|row| row.id == message),
            "plaintext must remain absent until the real history header is released"
        );
        let mut source_work = ChannelAccountMaintenance::default();
        for _ in 0..4 {
            source.maintain_account_channels(&mut source_work).await;
        }
        let snapshots: Vec<_> = source
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .into_iter()
            .filter_map(|event| match DeviceControl::decode(&event.ciphertext) {
                Some(DeviceControl::ReceiveKeys {
                    recipient,
                    epoch: snapshot_epoch,
                    authors,
                    positions,
                    ..
                }) if recipient == target.identity.public() && snapshot_epoch == epoch => authors
                    .into_iter()
                    .zip(positions)
                    .find(|(identity, _)| identity.user_id() == author)
                    .map(|(_, n)| n),
                _ => None,
            })
            .collect();
        assert!(!snapshots.is_empty());
        assert!(
            snapshots.iter().all(|n| *n > position),
            "first real snapshot must be later than the existing ciphertext"
        );
        let keys_arrived = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let selected = (|| -> Result<bool, LogError> {
                    let Some(binding) = target.channel_checkpoints.source_binding(channel)? else {
                        return Ok(false);
                    };
                    let Some(mut distribution) = target
                        .channel_checkpoints
                        .receiver_distribution(&binding, &author, epoch)?
                    else {
                        return Ok(false);
                    };
                    let found = distribution.n > position;
                    zeroize::Zeroize::zeroize(&mut distribution.chain_key);
                    Ok(found)
                })();
                let arrived = match selected {
                    Ok(found) => found,
                    Err(LogError::StaleCursor) => false,
                    Err(error) => panic!("selected late receive-key lookup failed: {error}"),
                };
                if arrived {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(
            keys_arrived.is_ok(),
            "actual signed late keys must arrive while history remains held"
        );
        assert!(
            !target
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|row| row.id == message)
        );
        *source.account_session_test_hook.lock().unwrap() = None;
        release.close();
        let repaired = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if target
                    .channel_history(channel, 20)
                    .unwrap()
                    .iter()
                    .any(|row| {
                        row.id == message && row.text == b"after inheritance before first keys"
                    })
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        for task in [history, source_history, source_wake] {
            task.abort();
            let _ = tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .expect("history fixture owners must retire after abort");
        }
        *source.account_client_test_hook.lock().unwrap() = None;
        assert!(
            repaired.is_ok(),
            "history must repair an authorized ciphertext whose first receive snapshot is too new"
        );
    }

    #[tokio::test]
    async fn account_maintenance_inherits_and_exchanges_messages_with_owner_offline() {
        let group = Group::new().await;
        let owner = &group.nodes[0];
        let source = &group.nodes[1];
        let target = &group.nodes[2];
        let other = &group.nodes[3];
        let channel = owner
            .create_channel(
                "three accounts",
                vec![source.identity.public(), other.identity.public()],
            )
            .await
            .unwrap();
        let mut owner_work = ChannelAccountMaintenance::default();
        for _ in 0..12 {
            owner.maintain_account_channels(&mut owner_work).await;
        }
        assert!(
            owner
                .account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_some(),
            "compatible fully bound legacy group receives an owner-signed account grant"
        );
        source
            .send_channel_message(channel, b"before inheritance")
            .await
            .unwrap();
        group.servers[0].abort();
        for node in &group.nodes[1..] {
            node.roster.lock().unwrap().forget(&owner.identity.public());
        }
        let mut source_work = ChannelAccountMaintenance::default();
        let mut target_work = ChannelAccountMaintenance::default();
        for _ in 0..16 {
            target.maintain_account_channels(&mut target_work).await;
            source.maintain_account_channels(&mut source_work).await;
        }
        let state = target
            .account_group_projection(channel)
            .unwrap()
            .state
            .expect("inherited group is visible without its owner online");
        assert!(state.is_accepted_member(&target.user_id()));
        assert_eq!(
            target.channel_member_account_sets(channel).unwrap().0.len(),
            3
        );
        let shared_authors = || {
            source
                .log
                .lock()
                .unwrap()
                .events(&channel)
                .unwrap()
                .into_iter()
                .filter(|event| event.author.ed25519_pub() == &source.identity.public().ed25519_pub)
                .filter_map(|event| match DeviceControl::decode(&event.ciphertext) {
                    Some(DeviceControl::ReceiveKeys { authors, .. }) => Some(authors),
                    _ => None,
                })
                .flatten()
                .map(|author| author.user_id())
                .collect::<Vec<_>>()
        };
        assert!(
            !shared_authors().contains(&other.user_id()),
            "not-yet-learned receive chain is absent from initial bootstrap"
        );
        target
            .send_channel_message(channel, b"from independently enrolled device")
            .await
            .unwrap();
        source
            .send_channel_message(channel, b"after inheritance")
            .await
            .unwrap();
        other
            .send_channel_message(channel, b"other account still participates")
            .await
            .unwrap();
        tokio::task::yield_now().await;
        for _ in 0..4 {
            source.maintain_account_channels(&mut source_work).await;
        }
        assert!(
            shared_authors().contains(&other.user_id()),
            "late-learned receive chain gets a durable incremental bootstrap"
        );
        let shared_count = shared_authors().len();
        source_work = ChannelAccountMaintenance::default();
        for _ in 0..4 {
            source.maintain_account_channels(&mut source_work).await;
        }
        assert_eq!(
            shared_authors().len(),
            shared_count,
            "restarting the coordinator uses durable author coverage and does not republish keys"
        );
        let projection = source.account_group_projection(channel).unwrap();
        let before = source
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        for _ in 0..3 {
            assert!(
                !source
                    .share_account_receive_keys(
                        channel,
                        &projection,
                        &mut source_work,
                        &mut crate::eventlog::sync::network::PreparationBudget::default(),
                    )
                    .unwrap()
            );
        }
        assert_eq!(
            source
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "unchanged current receiver sharing must not reopen signed history"
        );
        assert!(
            source
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|entry| entry.text == b"from independently enrolled device")
        );
        assert!(
            target
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|entry| entry.text == b"after inheritance")
        );
        assert!(
            target
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|entry| entry.text == b"other account still participates")
        );
        assert!(
            !target
                .channel_history(channel, 20)
                .unwrap()
                .iter()
                .any(|entry| entry.text == b"before inheritance")
        );
    }
    #[tokio::test]
    async fn account_maintenance_defers_unknown_capability_conflicting_binding_and_pending_legacy()
    {
        let group = Group::new().await;
        let owner = &group.nodes[0];
        let channel = owner
            .create_channel(
                "legacy",
                vec![
                    group.nodes[1].identity.public(),
                    group.nodes[3].identity.public(),
                ],
            )
            .await
            .unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let generation = owner.privacy.generation.load(Ordering::Acquire);
        let first = owner.routing_peer(&group.nodes[1].user_id()).unwrap();
        let value = work.capability(&first, generation);
        value.supported = true;
        value.proof = Some(Announce::new_with_account(
            &group.nodes[1].identity,
            &group.nodes[1].account,
            "source",
            first.addr.port(),
        ));
        value.probe_at = Instant::now() + REFRESH;
        assert!(
            owner
                .commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_none()
        );
        assert!(
            owner
                .account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_none()
        );
        let peer = owner.routing_peer(&group.nodes[3].user_id()).unwrap();
        let value = work.capability(&peer, generation);
        value.supported = true;
        value.proof = Some(Announce::new_with_account(
            &group.nodes[3].identity,
            &group.nodes[3].account,
            "other",
            peer.addr.port(),
        ));
        value.probe_at = Instant::now() + REFRESH;
        let changed = Announce::new_with_account(
            &group.nodes[3].identity,
            &Account::generate(),
            "changed account",
            peer.addr.port(),
        );
        owner
            .roster
            .lock()
            .unwrap()
            .update(&changed, peer.addr.ip(), &owner.user_id());
        assert!(
            owner
                .commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_none()
        );
        assert!(
            owner
                .account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_none(),
            "a changed UDP account claim cannot replace the authenticated capability binding"
        );
        let pending = owner
            .create_channel_invited("pending", vec![group.nodes[1].identity.public()])
            .await
            .unwrap();
        assert!(
            owner
                .commit_account_maintenance(pending, &mut work)
                .unwrap()
                .is_none()
        );
        assert!(
            owner
                .account_group_projection(pending)
                .unwrap()
                .authority
                .current_event()
                .is_none()
        );
    }

    #[tokio::test]
    async fn account_maintenance_capability_is_bounded_and_invalidated_by_full_key_route_and_privacy_generation()
     {
        let group = Group::new().await;
        let owner = &group.nodes[0];
        let mut peer = owner.routing_peer(&group.nodes[1].user_id()).unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let value = work.capability(&peer, 1);
        value.probe_at = Instant::now() + RETRY;
        let deadline = value.probe_at;
        assert_eq!(
            work.capability(&peer, 1).probe_at,
            deadline,
            "repeated discovery cannot bypass unsupported cooldown"
        );
        peer.addr.set_port(peer.addr.port().wrapping_add(1));
        assert!(
            work.capability(&peer, 1).probe_at < deadline,
            "new route gets a bounded fresh probe"
        );
        let value = work.capability(&peer, 1);
        value.supported = true;
        value.probe_at = Instant::now() + REFRESH;
        assert!(work.supports(&peer.public, 1));
        assert!(!work.supports(&peer.public, 2));
        assert!(!work.capability(&peer, 2).supported);
        let value = work.capability(&peer, 2);
        value.supported = true;
        value.probe_at = Instant::now() + REFRESH;
        peer.public.x25519_pub[0] ^= 1;
        assert!(
            !work.capability(&peer, 2).supported,
            "same Ed key with changed X key is a different full device identity"
        );
        for _ in 0..MAX_PEERS + 1 {
            peer.public = DeviceIdentity::generate().public();
            work.capability(&peer, 2);
        }
        assert_eq!(work.peers.len(), MAX_PEERS);
    }
    #[tokio::test]
    async fn account_maintenance_unknown_legacy_owner_needs_explicit_source_consent() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let channel = crate::channel::new_channel_id();
        let meta = crate::channel::ChannelMeta {
            name: "pre-upgrade legacy".into(),
            members: vec![node.identity.public()],
            owner: node.user_id(),
            epoch: 0,
        };
        node.append_event(channel, EventKind::MembershipChange, meta.encode())
            .unwrap();
        node.process_channel(channel);
        crate::node::test_support::settle_group_metadata(node, channel);
        let mut work = ChannelAccountMaintenance::default();
        assert!(
            node.commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_none(),
            "same current account and device UID alone are not migration consent"
        );
        assert!(
            node.authorize_legacy_channel_migration_if(|_| Ok(()))
                .is_err()
        );
        assert!(
            node.commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_none()
        );
        node.authorize_legacy_channel_migration_if(|commit| commit())
            .unwrap();
        assert!(
            node.commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_some()
        );
        assert!(
            node.account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_some()
        );
    }

    #[tokio::test]
    async fn account_maintenance_pending_tail_keeps_the_accepted_control_fanout() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let channel = crate::channel::new_channel_id();
        let members = vec![node.identity.public()];
        node.append_event(
            channel,
            EventKind::MembershipChange,
            crate::channel::ChannelMeta {
                name: "bounded maintenance fanout".into(),
                members: members.clone(),
                owner: node.user_id(),
                epoch: 0,
            }
            .encode(),
        )
        .unwrap();
        crate::node::test_support::settle_group_metadata(node, channel);
        node.authorize_legacy_channel_migration_if(|commit| commit())
            .unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let mut budget = crate::eventlog::sync::network::PreparationBudget {
            rows: 2,
            ..Default::default()
        };
        let fanout = node
            .commit_account_maintenance_with_budget(channel, &mut work, &mut budget)
            .unwrap();
        assert_eq!(fanout, Some(members));
        assert!(
            matches!(
                node.account_group_projection(channel),
                Err(LogError::StaleCursor)
            ),
            "the accepted authority must retain its fanout when this turn cannot finish its tail"
        );
        let accepted = node
            .log
            .lock()
            .unwrap()
            .events(&channel)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelAccountAuthority)
            .map(|event| event.id)
            .collect::<Vec<_>>();
        assert_eq!(accepted.len(), 1);
        node.await_channel_metadata(channel).await.unwrap();
        assert!(
            node.commit_account_maintenance(channel, &mut work)
                .unwrap()
                .is_none(),
            "unchanged maintenance must not schedule another network fanout"
        );
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .events(&channel)
                .unwrap()
                .into_iter()
                .filter(|event| event.kind == EventKind::ChannelAccountAuthority)
                .map(|event| event.id)
                .collect::<Vec<_>>(),
            accepted
        );
    }

    #[tokio::test]
    async fn settled_account_channel_maintenance_is_quiet_and_changed_source_retries() {
        fn queued(node: &Node, channel: ConversationId) -> bool {
            node.storage
                .database()
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT EXISTS(SELECT 1 FROM channel_runtime_work WHERE channel=?1)",
                        [channel.as_bytes().as_slice()],
                        |r| r.get(0),
                    )?)
                })
                .unwrap()
        }
        fn drain(node: &Node, channel: ConversationId) {
            for _ in 0..64 {
                node.process_channel_reception_inner(channel, "live")
                    .unwrap();
                if !queued(node, channel) {
                    break;
                }
            }
            assert!(
                !queued(node, channel),
                "fixture must retire its actual reception lane"
            );
        }
        fn assert_quiet(
            node: &Node,
            channel: ConversationId,
            work: &mut ChannelAccountMaintenance,
        ) {
            assert!(
                !queued(node, channel),
                "measure fully drained demand, not queue deduplication"
            );
            let database = node.storage.database();
            let changes = database.read(|c| Ok(c.total_changes())).unwrap();
            let commits = database.successful_write_commits();
            let reads = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts();
            for _ in 0..8 {
                assert!(
                    node.commit_account_maintenance(channel, work)
                        .unwrap()
                        .is_none()
                );
                node.process_channel_reception_inner(channel, "live")
                    .unwrap();
                assert!(!queued(node, channel));
            }
            assert_eq!(
                database.read(|c| Ok(c.total_changes())).unwrap(),
                changes,
                "settled account maintenance must not recreate and retire its durable lane"
            );
            assert_eq!(database.successful_write_commits(), commits);
            assert_eq!(
                node.log
                    .lock()
                    .unwrap()
                    .repository()
                    .unwrap()
                    .body_read_counts(),
                reads
            );
        }

        let group = Group::new().await;
        let node = &group.nodes[0];
        let channel = node
            .create_channel("quiet account maintenance", vec![])
            .await
            .unwrap();
        let mut work = ChannelAccountMaintenance::default();
        node.commit_account_maintenance(channel, &mut work).unwrap();
        node.await_channel_metadata(channel).await.unwrap();
        assert!(
            node.account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_some()
        );
        drain(node, channel);
        assert_quiet(node, channel, &mut work);

        // A real source tail must still leave durable demand when this turn has
        // no preparation allowance, then recover through the same entry point.
        for _ in 0..8 {
            node.append_event(channel, EventKind::Message, vec![0; 64])
                .unwrap();
        }
        let mut exhausted = crate::eventlog::sync::network::PreparationBudget {
            rows: 0,
            ..Default::default()
        };
        assert!(matches!(
            node.commit_account_maintenance_with_budget(channel, &mut work, &mut exhausted),
            Err(NodeError::Log(LogError::StaleCursor))
        ));
        assert!(
            queued(node, channel),
            "unfinished changed source must retain durable recovery demand"
        );
        node.await_channel_metadata(channel).await.unwrap();
        drain(node, channel);
        assert_quiet(node, channel, &mut work);
    }

    #[tokio::test]
    async fn unchanged_channel_maintenance_does_not_rehydrate_projection_or_distributions() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let channel = node.create_channel("idle producer", vec![]).await.unwrap();
        let mut work = ChannelAccountMaintenance::default();
        for _ in 0..4 {
            node.maintain_account_channels(&mut work).await;
            node.await_channel_metadata(channel).await.unwrap();
        }
        let hydrations = node.channel_checkpoints.projection_hydrations();
        let before = (work.projection_reads, work.distribution_reads);
        assert!(
            before.0 > 0,
            "fixture must execute the real projection owner"
        );
        for _ in 0..8 {
            node.maintain_account_channels(&mut work).await;
            assert!(node.maintain_one_account_channel(channel, &mut work).await);
        }
        assert!(
            work.deadline().saturating_duration_since(Instant::now()) >= Duration::from_secs(290),
            "quiet maintenance must park at sparse recovery, not a hot tick"
        );
        assert_eq!(
            (work.projection_reads, work.distribution_reads),
            before,
            "unchanged producer must not hydrate projection or sender distributions"
        );
        assert_eq!(
            node.channel_checkpoints.projection_hydrations(),
            hydrations,
            "measure all published projection hydration, not only commit calls"
        );
        node.append_event(channel, EventKind::Message, vec![0; 64])
            .unwrap();
        assert!(node.maintain_one_account_channel(channel, &mut work).await);
        assert!(
            work.projection_reads > before.0,
            "source changes must invalidate scheduling evidence"
        );
    }

    #[tokio::test]
    async fn failed_capability_retains_pending_inventory_without_a_short_retry_loop() {
        let group = Group::new().await;
        let node = &group.nodes[1];
        let channel = node
            .create_channel("cooled pending inventory", vec![])
            .await
            .unwrap();
        node.await_channel_metadata(channel).await.unwrap();
        let peer = node.routing_peer(&group.nodes[2].user_id()).unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let value = work.capability(&peer, node.privacy.generation.load(Ordering::Acquire));
        value.supported = false;
        value.probe_at = Instant::now() + RETRY;
        value.pending.push_back(channel);
        for _ in 0..4 {
            node.maintain_account_channels(&mut work).await;
        }
        assert_eq!(work.peers[&peer.public.user_id()].pending.len(), 1);
        assert!(
            work.input.is_some(),
            "failed capability must park retained inventory until its probe deadline"
        );
        assert!(
            work.deadline().saturating_duration_since(Instant::now()) >= Duration::from_secs(50)
        );
    }

    #[tokio::test]
    async fn unvisited_new_peer_retains_its_initial_probe_deadline() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let channel = node
            .create_channel(
                "new maintenance peer",
                vec![
                    group.nodes[1].identity.public(),
                    group.nodes[3].identity.public(),
                ],
            )
            .await
            .unwrap();
        node.await_channel_metadata(channel).await.unwrap();
        let mut peers: Vec<_> = node
            .routing_peers()
            .into_iter()
            .filter(|peer| node.group_member(&peer.public).unwrap())
            .collect();
        peers.sort_unstable_by_key(|peer| peer.public.user_id());
        assert!(peers.len() >= 2);
        let mut work = ChannelAccountMaintenance::default();
        let existing = work.capability(&peers[0], node.privacy.generation.load(Ordering::Acquire));
        existing.probe_at = Instant::now() + REFRESH;
        node.maintain_account_channels(&mut work).await;
        assert!(
            work.peers.contains_key(&peers[1].public.user_id()),
            "new unvisited peer must own its initial deadline"
        );
        assert!(
            work.deadline().saturating_duration_since(Instant::now()) <= Duration::from_secs(3)
        );
    }

    #[tokio::test]
    async fn private_sibling_authentication_invalidates_once_without_public_roster_changes() {
        let group = Group::new().await;
        let node = &group.nodes[1];
        let source = &group.nodes[2];
        let address = node.routing_peer(&source.user_id()).unwrap().addr;
        for (index, owner, port) in [(1, node, 1), (2, source, address.port())] {
            owner
                .configure_privacy(
                    &group._dir.path().join(index.to_string()),
                    "pw",
                    &owner.signed_announce("private sibling", port),
                    Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
                )
                .unwrap();
        }
        source.set_invisible(true).await.unwrap();
        let proof = source.signed_announce("private sibling", address.port());
        {
            let mut guard = node.privacy.state.write().unwrap();
            let state = guard.as_mut().unwrap();
            state.proofs.record(&proof).unwrap();
            state
                .routes
                .record(&source.identity.public(), address.ip())
                .unwrap();
        }
        *node.roster.lock().unwrap() = Roster::default();
        let work = ChannelAccountMaintenance::default();
        let before = work.input(node).unwrap();
        let mut changes = node.subscribe_sibling_availability();
        drop(
            node.privacy_dial(address, &source.identity.public())
                .await
                .unwrap(),
        );
        assert!(
            changes.has_changed().unwrap(),
            "real private authentication must wake the owner"
        );
        changes.borrow_and_update();
        let available = work.input(node).unwrap();
        assert_ne!(
            available, before,
            "the new watch must invalidate the producer gate"
        );
        assert!(node.roster.lock().unwrap().peers().is_empty());
        drop(
            node.privacy_dial(address, &source.identity.public())
                .await
                .unwrap(),
        );
        assert!(
            !changes.has_changed().unwrap(),
            "unchanged authentication renewal must coalesce"
        );
        assert_eq!(work.input(node).unwrap(), available);
    }

    #[tokio::test]
    async fn deferred_channel_is_not_acknowledged_by_another_channels_success() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let deferred = node.create_channel("deferred", vec![]).await.unwrap();
        let ready = node.create_channel("ready", vec![]).await.unwrap();
        node.await_channel_metadata(deferred).await.unwrap();
        node.await_channel_metadata(ready).await.unwrap();
        *node.account_channel_maintenance_test_hook.lock().unwrap() =
            Some(Arc::new(move |channel| channel != deferred));
        let mut work = ChannelAccountMaintenance::default();
        for _ in 0..8 {
            node.maintain_account_channels(&mut work).await;
        }
        assert!(
            work.settled.contains_key(ready.as_bytes()),
            "the other channel must complete real maintenance"
        );
        assert!(!work.settled.contains_key(deferred.as_bytes()));
        assert!(
            work.input.is_none(),
            "one ready channel cannot acknowledge another channel's deferred work"
        );
        assert!(
            work.deadline().saturating_duration_since(Instant::now()) <= Duration::from_secs(3)
        );
        *node.account_channel_maintenance_test_hook.lock().unwrap() = None;
        for _ in 0..8 {
            node.maintain_account_channels(&mut work).await;
        }
        assert!(
            work.settled.contains_key(deferred.as_bytes()),
            "removing the hold must recover retained work"
        );
        assert!(work.input.is_some());
    }

    #[tokio::test]
    async fn invisible_cached_group_routes_do_not_hydrate_stamp_inputs() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let peer = &group.nodes[1];
        let address = node.routing_peer(&peer.user_id()).unwrap().addr;
        let channel = node
            .create_channel("cached authority", vec![peer.identity.public()])
            .await
            .unwrap();
        node.await_channel_metadata(channel).await.unwrap();
        node.configure_privacy(
            &group._dir.path().join("0"),
            "pw",
            &node.signed_announce("owner", 1),
            Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node.set_invisible(true).await.unwrap();
        *node.roster.lock().unwrap() = Roster::default();
        {
            let mut state = node.privacy.state.write().unwrap();
            let state = state.as_mut().unwrap();
            let proof = peer.signed_announce("cached member", address.port());
            state.proofs.record(&proof).unwrap();
            state.routes.record(&proof.public(), address.ip()).unwrap();
        }
        let before = node.channel_checkpoints.state_hydrations();
        assert!(
            node.cached_routing_peers()
                .iter()
                .any(|candidate| candidate.public == peer.identity.public()),
            "fixture must exercise a genuine cached shared-group grant"
        );
        assert!(
            node.channel_checkpoints.state_hydrations() > before,
            "negative control must show filtered routing hydrates authority"
        );
        let counts = (
            node.channel_checkpoints.projection_hydrations(),
            node.channel_checkpoints.state_hydrations(),
        );
        let work = ChannelAccountMaintenance::default();
        let input = work.input(node).unwrap();
        for _ in 0..8 {
            assert_eq!(work.input(node).unwrap(), input);
        }
        assert_eq!(
            (
                node.channel_checkpoints.projection_hydrations(),
                node.channel_checkpoints.state_hydrations()
            ),
            counts,
            "a scheduling stamp must not authorize raw cached hints"
        );
    }

    #[tokio::test]
    async fn unselected_cached_peer_cannot_retain_an_expired_maintenance_deadline() {
        let group = Group::new().await;
        let node = &group.nodes[0];
        let peer = node.routing_peer(&group.nodes[3].user_id()).unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let value = work.capability(&peer, node.privacy.generation.load(Ordering::Acquire));
        value.supported = true;
        value.probe_at = Instant::now() - RETRY;
        value.binding_poll_at = Instant::now() - RETRY;
        // The cached capability predates the current absence of a shared grant.
        assert!(!node.group_member(&peer.public).unwrap());
        for _ in 0..4 {
            node.maintain_account_channels(&mut work).await;
        }
        assert!(
            work.peers.is_empty(),
            "unselected full identities must retire their deadlines"
        );
        assert!(
            work.deadline().saturating_duration_since(Instant::now()) >= Duration::from_secs(290)
        );
    }

    #[tokio::test]
    async fn semantic_maintenance_invalidation_preserves_failed_inventory_cooldowns() {
        let group = Group::new().await;
        let node = &group.nodes[1];
        let peer = node.routing_peer(&group.nodes[2].user_id()).unwrap();
        let mut work = ChannelAccountMaintenance::default();
        let value = work.capability(&peer, node.privacy.generation.load(Ordering::Acquire));
        value.supported = true;
        value.probe_at = Instant::now() + REFRESH;
        value.poll_at = Instant::now() + RETRY;
        value.binding_poll_at = Instant::now() + RETRY;
        value.poll_failed = true;
        value.binding_failed = true;
        let deadlines = (value.poll_at, value.binding_poll_at);
        work.observed_input = Some(work.input(node).unwrap());
        work.input = work.observed_input;
        work.run_at = Some(Instant::now() + REFRESH);
        node.privacy
            .projection_revision
            .fetch_add(1, Ordering::SeqCst);
        node.maintain_account_channels(&mut work).await;
        let value = &work.peers[&peer.public.user_id()];
        assert_eq!(
            (value.poll_at, value.binding_poll_at),
            deadlines,
            "semantic changes cannot erase a failed exchange's cooldown"
        );
        assert!(value.poll_failed && value.binding_failed);
    }

    #[test]
    fn account_migration_consent_survives_reopen_and_never_rebinds_account_or_full_device() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.log");
        let account = Account::generate().public().ed25519_pub;
        let identity = DeviceIdentity::generate();
        let channel = crate::channel::new_channel_id();
        let root = crate::eventlog::EventId::new([9; 32]);
        {
            let mut store =
                ChannelMigrationStore::open(&path, "pw", account, &identity.public()).unwrap();
            store.authorize(vec![(channel, root)]).unwrap();
        }
        let store = ChannelMigrationStore::open(&path, "pw", account, &identity.public()).unwrap();
        assert!(store.permits(channel, root));
        drop(store);
        let foreign = DeviceIdentity::generate();
        let foreign_store =
            ChannelMigrationStore::open(&path, "pw", account, &foreign.public()).unwrap();
        assert!(
            !foreign_store.permits(channel, root),
            "a restored marker never authorizes a different device"
        );
        drop(foreign_store);
        assert!(
            ChannelMigrationStore::open(
                &path,
                "pw",
                Account::generate().public().ed25519_pub,
                &identity.public()
            )
            .is_err()
        );
        let adopted = ChannelMigrationStore::open(
            &dir.path().join("different-account.log"),
            "pw",
            Account::generate().public().ed25519_pub,
            &identity.public(),
        )
        .unwrap();
        assert!(
            !adopted.permits(channel, root),
            "account adoption opens an uninitialized account-scoped consent store"
        );
    }
    #[tokio::test]
    async fn account_maintenance_initial_cutover_uses_current_noise_binding_not_forged_udp_or_historical_guess()
     {
        let group = Group::new().await;
        let owner = &group.nodes[0];
        let source = &group.nodes[1];
        let channel = owner
            .create_channel(
                "new account commitment",
                vec![source.identity.public(), group.nodes[3].identity.public()],
            )
            .await
            .unwrap();
        let peer = owner.routing_peer(&source.user_id()).unwrap();
        let attacker = Account::generate();
        let forged = Announce::new_with_account(
            &source.identity,
            &attacker,
            "stolen Ed claim",
            peer.addr.port(),
        );
        owner
            .roster
            .lock()
            .unwrap()
            .update(&forged, peer.addr.ip(), &owner.user_id());
        let mut work = ChannelAccountMaintenance::default();
        for _ in 0..12 {
            owner.maintain_account_channels(&mut work).await;
        }
        assert!(
            owner
                .account_group_projection(channel)
                .unwrap()
                .authority
                .current_event()
                .is_none(),
            "matching Noise full keys must not authenticate a DIFFERENT UDP account certificate"
        );
        let current = Announce::new_with_account(
            &source.identity,
            &source.account,
            "legitimate current binding",
            peer.addr.port(),
        );
        owner
            .roster
            .lock()
            .unwrap()
            .update(&current, peer.addr.ip(), &owner.user_id());
        assert!(
            owner
                .roster
                .lock()
                .unwrap()
                .historical_announcement(&source.identity.public())
                .is_none(),
            "legacy history is intentionally ambiguous"
        );
        for _ in 0..12 {
            owner.maintain_account_channels(&mut work).await;
        }
        let projection = owner.account_group_projection(channel).unwrap();
        assert!(
            projection.authority.current_event().is_some(),
            "explicit owner/new-creation consent can choose a NEW authenticated current account binding"
        );
        assert!(
            projection
                .authority
                .grants()
                .iter()
                .any(|grant| grant.original.public() == source.identity.public()
                    && grant.original.account_id() == Some(source.account_id()))
        );
        assert!(
            !projection
                .authority
                .grants()
                .iter()
                .any(|grant| grant.original.account_id() == Some(attacker.account_id()))
        );
    }
    #[test]
    fn account_receive_refresh_is_bounded_preserves_routine_unread_and_ignores_equal_or_older_positions()
     {
        let author = DeviceIdentity::generate().public();
        let recipient = DeviceIdentity::generate().public();
        let mut distribution = ReceiveDistribution {
            author: author.clone(),
            epoch: 1,
            distribution: crate::channel::sender_key::SenderKeyDistribution {
                chain_key: [7; 32],
                n: 0,
            },
        };
        let mut shipped = BTreeMap::new();
        assert!(
            snapshot_needed(&distribution, &recipient, &shipped, false),
            "missing author is immediately bootstrapped even during refresh cooldown"
        );
        shipped.insert(author.user_id(), 0);
        assert!(
            !snapshot_needed(&distribution, &recipient, &shipped, true),
            "unchanged snapshots never form heartbeat traffic"
        );
        distribution.distribution.n = crate::channel::sender_key::MAX_SKIP;
        assert!(
            !snapshot_needed(&distribution, &recipient, &shipped, true),
            "routine unread within ratchet window is left to ordinary replay"
        );
        distribution.distribution.n += 1;
        assert!(
            !snapshot_needed(&distribution, &recipient, &shipped, false),
            "long-gap repair obeys bounded refresh cadence"
        );
        assert!(snapshot_needed(&distribution, &recipient, &shipped, true));
        shipped.insert(author.user_id(), distribution.distribution.n);
        assert!(!snapshot_needed(&distribution, &recipient, &shipped, true));
        distribution.distribution.n -= 1;
        assert!(
            !snapshot_needed(&distribution, &recipient, &shipped, true),
            "older replays cannot restart a key refresh loop"
        );
    }
}

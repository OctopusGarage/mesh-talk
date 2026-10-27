//! Bounded account-membership maintenance owned by ordinary channel recovery.
use super::{
    Node, NodeError,
    channel_account_authority::{AccountAuthorityControl, AccountGrant, MAX_AUTHORITY_BYTES},
    channel_account_bootstrap::{
        AccountGroupRequest, AccountGroupResponse, DeviceControl, ReceiveDistribution,
        inheritance_account_message,
    },
};
use crate::{
    discovery::{Announce, PeerRecord},
    eventlog::{ConversationId, EventKind},
    identity::device::PublicIdentity,
};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    net::SocketAddr,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

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
        use crate::eventlog::LogError;
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
    after: Option<ConversationId>,
    pending: VecDeque<ConversationId>,
    binding_poll_at: Instant,
    binding_channel: Option<ConversationId>,
    binding_after: Option<String>,
    binding_cursor: usize,
    binding_turn: bool,
}
#[derive(Default)]
pub(super) struct ChannelAccountMaintenance {
    peers: BTreeMap<String, Capability>,
    peer_cursor: usize,
    channel_cursor: usize,
    refresh_after: BTreeMap<([u8; 32], [u8; 32]), Instant>,
}
impl ChannelAccountMaintenance {
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
            after: None,
            pending: VecDeque::new(),
            binding_poll_at: Instant::now(),
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
                after: None,
                pending: VecDeque::new(),
                binding_poll_at: Instant::now(),
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
            let roots = self
                .list_channels()
                .into_iter()
                .filter_map(|row| self.legacy_owner_root(row.id).map(|root| (row.id, root)))
                .collect();
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
        Ok(())
    }
    pub(super) fn record_channel_owner_origin(
        &self,
        channel: ConversationId,
    ) -> Result<(), NodeError> {
        if let Some(root) = self.legacy_owner_root(channel) {
            self.channel_migration
                .lock()
                .expect("channel migration lock")
                .authorize(vec![(channel, root)])?;
        }
        Ok(())
    }
    fn legacy_owner_root(&self, channel: ConversationId) -> Option<crate::eventlog::EventId> {
        let projection = self.account_group_projection(channel);
        if projection.authority.current_event().is_some()
            || projection
                .state
                .as_ref()
                .is_none_or(|state| state.is_dissolved() || state.owner() != self.user_id())
        {
            return None;
        }
        projection
            .accepted
            .iter()
            .find(|member| member.public == self.identity.public())
            .map(|member| member.membership_event)
    }
    /// One peer turn and one local channel turn; the existing recovery task owns all state.
    pub(super) async fn maintain_account_channels(&self, work: &mut ChannelAccountMaintenance) {
        let mut peers = self.routing_peers();
        // Capability maintenance serves existing group relationships and siblings,
        // not every discovery entry. Unrelated probes compete with ordinary DMs
        // and must not initiate a pull connection before contact authorization.
        peers.retain(|peer| {
            !peer.post_office
                && (peer.account_id.as_deref() == Some(self.account_id().as_str())
                    || self.group_member(&peer.public)
                    || self.group_departure_peer(&peer.public))
        });
        peers.sort_unstable_by_key(|peer| peer.public.user_id());
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
            } else if value.supported && self.pull_group_binding_page(peer, value).await {
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
                        value.pending = channels.into();
                        value.after = next;
                        value.poll_at = Instant::now()
                            + if next.is_some() {
                                Duration::ZERO
                            } else {
                                Duration::from_secs(30)
                            };
                    } else {
                        value.poll_at = Instant::now() + RETRY;
                    }
                }
                if let Some(channel) = value.pending.pop_front() {
                    let _ = tokio::time::timeout(Duration::from_secs(15), async {
                        let mut connection =
                            self.privacy_sync_dial(peer.addr, &peer.public).await.ok()?;
                        let store = self.pull_sync_store(connection.peer_identity());
                        super::session::request_round(&mut connection, &store, channel)
                            .await
                            .ok()?;
                        self.recycle_sync_channel(connection);
                        self.process_channel(channel);
                        Some(())
                    })
                    .await;
                    self.maintain_one_account_channel(channel, work).await;
                }
            }
        }
        let channels = self.list_channels();
        if !channels.is_empty() {
            let channel = channels[work.channel_cursor % channels.len()].id;
            work.channel_cursor = work.channel_cursor.wrapping_add(1);
            self.maintain_one_account_channel(channel, work).await;
        }
    }
    async fn pull_group_binding_page(&self, peer: &PeerRecord, work: &mut Capability) -> bool {
        if !work.binding_turn {
            work.binding_turn = true;
            return false;
        }
        if Instant::now() < work.binding_poll_at {
            return false;
        }
        let Some(proof) = work.proof.as_ref() else {
            return false;
        };
        let mut groups: Vec<_> = self
            .list_channels()
            .into_iter()
            .filter(|row| self.binding_transition_group_member(row.id, &peer.public, proof))
            .map(|row| row.id)
            .collect();
        groups.sort_unstable_by_key(|id| *id.as_bytes());
        if groups.is_empty() {
            work.binding_poll_at = Instant::now() + Duration::from_secs(30);
            work.binding_channel = None;
            work.binding_after = None;
            return false;
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
                for patch in patches {
                    let _ = self.apply_relayed_account_transition(&patch, channel);
                }
                work.binding_after = next;
                if work.binding_after.is_none() {
                    work.binding_channel = None;
                    work.binding_cursor = work.binding_cursor.wrapping_add(1);
                    work.binding_poll_at = Instant::now() + Duration::from_secs(30);
                }
            }
            _ => {
                // Older Capability1 peers may close on the additive request.
                // Keep their existing group migration and route support enabled.
                work.binding_channel = None;
                work.binding_after = None;
                work.binding_poll_at = Instant::now() + RETRY;
            }
        }
        true
    }
    async fn maintain_one_account_channel(
        &self,
        channel: ConversationId,
        work: &mut ChannelAccountMaintenance,
    ) {
        let policy = self.privacy.gate.read().await;
        let result = {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            self.commit_account_maintenance(channel, work)
        };
        drop(policy);
        match result {
            Ok(Some(members)) => self.distribute_channel(channel, &members).await,
            Ok(None) => {}
            Err(_) => log::warn!("account channel maintenance deferred"),
        }
    }
    fn commit_account_maintenance(
        &self,
        channel: ConversationId,
        work: &mut ChannelAccountMaintenance,
    ) -> Result<Option<Vec<PublicIdentity>>, NodeError> {
        let own = self.own_group_proof();
        let me = self.identity.public();
        let mut projection = self.account_group_projection(channel);
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
            for member in &projection.accepted {
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
            self.process_channel(channel);
            projection = self.account_group_projection(channel);
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
            self.process_channel(channel);
            projection = self.account_group_projection(channel);
            let Some(state) = projection
                .state
                .as_ref()
                .filter(|state| state.is_member(&self.user_id()))
            else {
                return Ok(None);
            };
            // An inherited genuine account invitation remains pending until accepted.
            // Pending membership never installs a sender or shares receive material.
            if state.is_accepted_member(&self.user_id()) {
                self.distribute_my_sender_key(channel, state.members())?;
            }
            changed = true;
        }
        if self.share_account_receive_keys(channel, &projection, work)? {
            changed = true;
            self.process_channel(channel);
        }
        Ok(changed
            .then(|| {
                self.account_group_projection(channel)
                    .state
                    .map(|state| state.members().to_vec())
            })
            .flatten())
    }
    fn share_account_receive_keys(
        &self,
        channel: ConversationId,
        projection: &super::channel::ChannelMembershipProjection,
        work: &mut ChannelAccountMaintenance,
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
        let snapshots = {
            let book = self.channels.lock().expect("channels lock");
            let Some(current) = book.state(&channel) else {
                return Ok(false);
            };
            current
                .current_receive_distributions()
                .into_iter()
                .filter_map(|(author, epoch, mut distribution)| {
                    if let Some(public) = state.members().iter().find(|member| {
                        member.user_id() == author && state.is_accepted_member(&author)
                    }) {
                        Some(ReceiveDistribution {
                            author: public.clone(),
                            epoch,
                            distribution,
                        })
                    } else {
                        zeroize::Zeroize::zeroize(&mut distribution.chain_key);
                        None
                    }
                })
                .collect::<Vec<_>>()
        };
        let log = self.log.lock().expect("log lock");
        let events = log.events(&channel);
        let mut examined = HashSet::new();
        let mut selected = None;
        for (inheritance, grant, proof) in events
            .iter()
            .filter(|event| projection.authorized.contains(&event.id))
            .filter_map(|event| {
                DeviceControl::signed_inheritance(event)
                    .map(|(_, grant, proof)| (event.id, grant, proof))
            })
        {
            let recipient = proof.public();
            if recipient == self.identity.public()
                || proof.account_id().as_deref() != Some(self.account_id().as_str())
                || !state.is_accepted_member(&proof.user_id)
                || projection
                    .authority
                    .grant_for_device(&recipient, &proof)
                    .and_then(AccountGrant::id)
                    != Some(grant)
            {
                continue;
            }
            if !examined.insert(recipient.user_id()) {
                continue;
            }
            let mut shipped = BTreeMap::<String, u32>::new();
            for (author, position) in events
                .iter()
                .filter(|event| {
                    event.author.ed25519_pub() == &self.identity.public().ed25519_pub
                        && projection.authorized.contains(&event.id)
                })
                .filter_map(|event| match DeviceControl::decode(&event.ciphertext) {
                    Some(DeviceControl::ReceiveKeys {
                        grant: prior,
                        recipient: target,
                        epoch,
                        authors,
                        positions,
                        ..
                    }) if prior == grant && target == recipient && epoch == state.epoch() => {
                        Some(authors.into_iter().zip(positions))
                    }
                    _ => None,
                })
                .flatten()
            {
                shipped
                    .entry(author.user_id())
                    .and_modify(|old| *old = (*old).max(position))
                    .or_insert(position);
            }
            let key = (*channel.as_bytes(), recipient.ed25519_pub);
            let refresh = work
                .refresh_after
                .get(&key)
                .is_none_or(|deadline| Instant::now() >= *deadline);
            if snapshots.iter().any(|value| {
                value.epoch == state.epoch()
                    && snapshot_needed(value, &recipient, &shipped, refresh)
            }) {
                selected = Some((inheritance, grant, recipient, shipped, refresh));
                break;
            }
        }
        drop(events);
        drop(log);
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
            self.append_event(
                channel,
                EventKind::ChannelDeviceInheritance,
                control.encode(),
            )?;
            changed = true;
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
            !relay.binding_transition_group_member(
                channel,
                &group.nodes[2].identity.public(),
                &group.nodes[2].own_group_proof()
            ),
            "a certified sibling without accepted device membership cannot query another device's group"
        );
        let wrong_x = DeviceIdentity::from_secret_bytes(
            receiver.identity.secret_bytes().0,
            DeviceIdentity::generate().secret_bytes().1,
        );
        assert!(!relay.binding_transition_group_member(
            channel,
            &wrong_x.public(),
            &Announce::new_with_account(&wrong_x, &receiver.account, "wrong X", 1234)
        ));
        assert!(!relay.binding_transition_group_member(
            ConversationId::new([255; 32]),
            &receiver.identity.public(),
            &receiver.own_group_proof()
        ));
        group.servers[2].abort();
        relay
            .add_channel_member(channel, group.nodes[2].identity.public())
            .await
            .unwrap();
        let projection = relay.account_group_projection(channel);
        let state = projection.state.unwrap();
        assert!(state.is_member(&group.nodes[2].user_id()));
        assert!(!state.is_accepted_member(&group.nodes[2].user_id()));
        assert!(
            !relay.binding_transition_group_member(
                channel,
                &group.nodes[2].identity.public(),
                &group.nodes[2].own_group_proof()
            ),
            "pending invitation cannot authorize relay access"
        );
        relay
            .remove_channel_member(channel, &receiver.user_id())
            .await
            .unwrap();
        assert!(
            !relay.binding_transition_group_member(
                channel,
                &receiver.identity.public(),
                &receiver.own_group_proof()
            ),
            "removed membership cannot retain relay access"
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
            .state
            .expect("inherited group is visible without its owner online");
        assert!(state.is_accepted_member(&target.user_id()));
        assert_eq!(target.channel_member_account_sets(channel).0.len(), 3);
        let shared_authors = || {
            source
                .log
                .lock()
                .unwrap()
                .events(&channel)
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
                .authority
                .current_event()
                .is_some()
        );
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
        let projection = owner.account_group_projection(channel);
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

//! Serialized durable privacy decisions and authenticated disclosure boundaries.
use super::peer_directory::PeerDirectory;
use super::*;
use crate::discovery::{visibility::DiscoveryVisibility, Announce};
use crate::identity::device::PublicIdentity;
use crate::transport::{SecureChannel, TransportError, VerifiedPeer};
use std::{
    io,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, RwLock,
    },
};
use tokio::net::TcpStream;

pub(in crate::node) struct PrivacyState {
    pub policy: PrivacyPolicy,
    pub proofs: PeerDirectory,
    pub own: Announce,
    pub visibility: Arc<DiscoveryVisibility>,
    pub routes: super::privacy_routes::RouteCache,
    pub file_scopes: std::collections::HashMap<crate::eventlog::ConversationId, Vec<ManifestScope>>,
}
#[derive(Clone, PartialEq, Eq)]
pub(in crate::node) struct ManifestScope {
    pub parent: crate::eventlog::ConversationId,
    pub author: crate::eventlog::Author,
    pub event: crate::eventlog::EventId,
}
#[derive(Default)]
pub(in crate::node) struct PrivacyControl {
    pub state: RwLock<Option<PrivacyState>>,
    pub gate: Arc<tokio::sync::RwLock<()>>,
    pub generation: AtomicU64,
    pub route_probe_cursor: AtomicU64,
}
fn denied() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "privacy operation denied")
}
impl Node {
    /// Configure before starting any network tasks. Unconfigured SDK constructors
    /// keep their legacy public behavior. Corrupt durable policy/proofs return errors.
    pub fn configure_privacy(
        &self,
        directory: &Path,
        password: &str,
        own: &Announce,
        visibility: Arc<DiscoveryVisibility>,
    ) -> io::Result<()> {
        if !own.verify()
            || own.public() != self.identity.public()
            || own.account_id() != Some(self.account_id())
            || own.tcp_port == 0
            || own.name.len() > 1024
        {
            return Err(denied());
        }
        let policy = PrivacyPolicy::open(&directory.join("privacy.policy"), password)?;
        visibility.initialize_public(!policy.snapshot().invisible)?;
        let mut proofs = PeerDirectory::open(&directory.join("peer-proofs"), password)?;
        proofs.record_own(own, &self.identity.public())?;
        let routes =
            super::privacy_routes::RouteCache::open(&directory.join("peer-routes"), password)?;
        let mut file_scopes: std::collections::HashMap<_, Vec<ManifestScope>> = Default::default();
        {
            let records = self
                .received_files
                .lock()
                .expect("received files lock not poisoned");
            let log = self.log.lock().expect("log lock not poisoned");
            let events: std::collections::HashMap<_, _> = log
                .conversations()
                .into_iter()
                .flat_map(|c| {
                    log.events(&c)
                        .into_iter()
                        .filter(|e| e.kind == crate::eventlog::EventKind::FileManifest)
                        .map(|e| (e.id, e.clone()))
                        .collect::<Vec<_>>()
                })
                .collect();
            for conversation in records.conversations() {
                for record in records.entries(&conversation) {
                    if let (Some(manifest), Some(event)) = (
                        crate::file::decode_manifest(&record.plaintext),
                        events.get(&record.event_id),
                    ) {
                        file_scopes
                            .entry(manifest.file_conv())
                            .or_default()
                            .push(ManifestScope {
                                parent: event.conversation_id,
                                author: event.author,
                                event: event.id,
                            });
                    }
                }
            }
        }
        let next = PrivacyState {
            policy,
            proofs,
            own: own.clone(),
            visibility,
            routes,
            file_scopes,
        };
        let mut state = self.privacy.state.write().map_err(|_| denied())?;
        if state.is_some() {
            return Err(denied());
        }
        *state = Some(next);
        Ok(())
    }
    pub fn privacy_snapshot(&self) -> PrivacySnapshot {
        self.privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .map(|s| s.policy.snapshot())
            .unwrap_or(PrivacySnapshot {
                version: 1,
                invisible: false,
                allowed_accounts: Vec::new(),
            })
    }
    pub async fn set_invisible(&self, invisible: bool) -> io::Result<()> {
        self.set_invisible_if(invisible, || Ok(())).await
    }
    pub async fn set_invisible_if(
        &self,
        invisible: bool,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        let _operation = self.privacy.gate.write().await;
        let (visibility, previous) = {
            let state = self.privacy.state.read().map_err(|_| denied())?;
            let state = state.as_ref().ok_or_else(denied)?;
            (state.visibility.clone(), state.policy.snapshot().invisible)
        };
        if previous == invisible {
            return authorize();
        }
        if invisible {
            visibility.set_public(false).await;
        }
        let result = {
            let mut state = self.privacy.state.write().map_err(|_| denied())?;
            authorize().and_then(|_| {
                state
                    .as_mut()
                    .ok_or_else(denied)?
                    .policy
                    .set_invisible(invisible)
            })
        };
        if let Err(error) = result {
            if invisible {
                visibility.set_public(!previous).await;
            }
            return Err(error);
        }
        self.privacy.generation.fetch_add(1, Ordering::SeqCst);
        if !invisible {
            visibility.set_public(true).await;
        }
        Ok(())
    }
    pub async fn set_allowed(&self, account: &str, allowed: bool) -> io::Result<()> {
        self.set_allowed_if(account, allowed, || Ok(())).await
    }
    pub async fn set_allowed_if(
        &self,
        account: &str,
        allowed: bool,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        self.change_allowed(account, allowed, PermissionSource::Manual, authorize)
            .await?;
        if allowed {
            self.publish_presence(account).await;
        }
        Ok(())
    }
    pub async fn initiate_contact(&self, account: &str) -> io::Result<()> {
        self.initiate_contact_if(account, || Ok(())).await
    }
    pub async fn initiate_contact_if(
        &self,
        account: &str,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        self.change_allowed(account, true, PermissionSource::Initiated, authorize)
            .await?;
        self.publish_presence(account).await;
        Ok(())
    }
    async fn change_allowed(
        &self,
        account: &str,
        allowed: bool,
        source: PermissionSource,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        let _operation = self.privacy.gate.write().await;
        let announcements = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcements();
        let endpoints = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .peers();
        let mut guard = self.privacy.state.write().map_err(|_| denied())?;
        let Some(state) = guard.as_mut() else {
            return authorize();
        };
        let before = state.policy.snapshot();
        if allowed {
            let matches: Vec<_> = announcements
                .into_iter()
                .filter(|a| a.account_id().as_deref() == Some(account) && a.verify())
                .collect();
            let cached = state
                .proofs
                .announcements()
                .into_iter()
                .find(|a| a.account_id().as_deref() == Some(account));
            let name = matches
                .first()
                .or(cached.as_ref())
                .map(|a| a.name.clone())
                .or_else(|| {
                    before
                        .allowed_accounts
                        .iter()
                        .find(|a| a.id == account)
                        .map(|a| a.name.clone())
                })
                .ok_or_else(denied)?;
            authorize()?;
            for proof in &matches {
                let binding_changed =
                    state.proofs.account_for(&proof.public()) != proof.account_id();
                state.proofs.record_explicit_account(proof)?;
                if binding_changed {
                    // Invalidate even if a later policy save fails or the new
                    // account already had an independent permission entry.
                    self.privacy.generation.fetch_add(1, Ordering::SeqCst);
                }
                if let Some(endpoint) = endpoints.iter().find(|p| p.public == proof.public()) {
                    state.routes.record(&endpoint.public, endpoint.addr.ip())?;
                }
            }
            state.policy.grant(account, &name, source)?;
        } else {
            authorize()?;
            state.policy.revoke(account)?;
        }
        if before != state.policy.snapshot() {
            self.privacy.generation.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
    async fn publish_presence(&self, account: &str) {
        if self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .is_none()
        {
            return;
        }
        let targets = self.routing_peers();
        // Bound the whole best-effort publication, not N times the handshake
        // deadline. Cancellation drops the current dial and its operation guard;
        // no detached tasks outlive the runtime or undo the committed grant.
        let _ = tokio::time::timeout(super::transport::HANDSHAKE_TIMEOUT, async {
            for target in targets
                .into_iter()
                .filter(|p| p.account_id.as_deref() == Some(account))
            {
                // Authentication alone conveys our signed listening presence.
                let _ = self.privacy_dial(target.addr, &target.public).await;
            }
        })
        .await;
    }
    pub(in crate::node) async fn initiate_device(&self, public: &PublicIdentity) -> io::Result<()> {
        if self.privacy.state.read().map_err(|_| denied())?.is_none() {
            return Ok(());
        }
        let account = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(public)
            .and_then(Announce::account_id);
        let account = account.or_else(|| {
            self.privacy
                .state
                .read()
                .ok()?
                .as_ref()?
                .proofs
                .account_for(public)
        });
        let Some(account) = account else {
            // Public legacy discovery remains usable, but cannot create a
            // permission without a verified account proof. Private mode denies.
            return if self.privacy_snapshot().invisible {
                Err(denied())
            } else {
                Ok(())
            };
        };
        self.initiate_contact(&account).await
    }
    pub(in crate::node) fn account_allowed(
        &self,
        public: &PublicIdentity,
        announcement: Option<&Announce>,
    ) -> bool {
        let state = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        let Some(state) = state.as_ref() else {
            return true;
        };
        if !state.policy.snapshot().invisible {
            return true;
        }
        let account = state.proofs.account_for(public).or_else(|| {
            announcement
                .filter(|a| a.public() == *public && a.verify())
                .and_then(Announce::account_id)
        });
        account.is_some_and(|a| a == self.account_id() || state.policy.allows(&a))
    }
    pub(in crate::node) fn group_member(&self, public: &PublicIdentity) -> bool {
        let snapshots: Vec<_> = {
            let book = self.channels.lock().expect("channels lock not poisoned");
            book.channel_ids()
                .into_iter()
                .filter_map(|id| {
                    let state = book.state(&id)?;
                    Some((
                        id,
                        crate::channel::ChannelMeta {
                            name: state.name().to_owned(),
                            members: state.members().to_vec(),
                            epoch: state.epoch(),
                            owner: state.owner().to_owned(),
                        },
                    ))
                })
                .collect()
        };
        // The book pins the owner, but may not yet have processed a newly
        // appended removal. Reuse the channel's epoch/tie-break rules over the
        // actual validated log before disclosing even signed listening presence.
        let log = self.log.lock().expect("log lock not poisoned");
        snapshots.into_iter().any(|(id, snapshot)| {
            let mut current = crate::channel::ChannelState::from_meta(id, snapshot);
            for event in log.events(&id) {
                if event.kind == crate::eventlog::EventKind::MembershipChange
                    && event.author.user_id() == current.owner()
                {
                    if let Some(meta) = crate::channel::ChannelMeta::decode(&event.ciphertext) {
                        current.apply_meta(meta);
                    }
                }
            }
            current.members().contains(public)
                && current.members().contains(&self.identity.public())
        })
    }
    pub(in crate::node) fn known_account_allowed(&self, public: &PublicIdentity) -> bool {
        let announce = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(public)
            .cloned();
        self.account_allowed(public, announce.as_ref())
    }
    pub(in crate::node) fn remember_manifest_scope(
        &self,
        file: crate::eventlog::ConversationId,
        parent: crate::eventlog::ConversationId,
        author: crate::eventlog::Author,
        event: crate::eventlog::EventId,
    ) {
        if file == parent {
            return;
        }
        let mut state = self
            .privacy
            .state
            .write()
            .expect("privacy lock not poisoned");
        if let Some(state) = state.as_mut() {
            let scopes = state.file_scopes.entry(file).or_default();
            let scope = ManifestScope {
                parent,
                author,
                event,
            };
            if !scopes.contains(&scope) {
                scopes.push(scope);
            }
        }
    }
    pub(in crate::node) fn update_presence(&self, own: &Announce) -> io::Result<()> {
        if !own.verify()
            || own.public() != self.identity.public()
            || own.account_id() != Some(self.account_id())
            || own.tcp_port == 0
            || own.name.len() > 1024
        {
            return Err(denied());
        }
        let mut state = self.privacy.state.write().map_err(|_| denied())?;
        if let Some(state) = state.as_mut() {
            state.proofs.record_own(own, &self.identity.public())?;
            state.own = own.clone();
        }
        Ok(())
    }
    pub(in crate::node) fn relay_allowed(&self, public: &PublicIdentity) -> bool {
        let announce = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(public)
            .cloned();
        let state = self
            .privacy
            .state
            .read()
            .expect("privacy lock not poisoned");
        let Some(state) = state.as_ref().filter(|s| s.policy.snapshot().invisible) else {
            return true;
        };
        let account = state.proofs.account_for(public).or_else(|| {
            announce
                .as_ref()
                .filter(|a| a.verify())
                .and_then(Announce::account_id)
        });
        account.is_some_and(|id| {
            id == self.account_id()
                || state
                    .policy
                    .snapshot()
                    .allowed_accounts
                    .iter()
                    .any(|a| a.id == id && a.source == PermissionSource::Manual)
        })
    }
    fn admitted(&self, peer: &VerifiedPeer) -> bool {
        {
            let state = self
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_ref() {
                if let (Some(old), Some(new)) = (
                    state.proofs.by_author(&peer.public_identity().ed25519_pub),
                    peer.announcement(),
                ) {
                    if old.public() != new.public() || old.account_id() != new.account_id() {
                        return false;
                    }
                }
            }
        }
        self.account_allowed(peer.public_identity(), peer.announcement())
            || self.group_member(peer.public_identity())
    }
    fn own_presence(&self) -> Option<Announce> {
        self.privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .map(|s| s.own.clone())
    }
    pub(in crate::node) fn remember_peer(
        &self,
        public: &PublicIdentity,
        announce: Option<&Announce>,
        ip: std::net::IpAddr,
    ) -> Result<(), TransportError> {
        let ip = crate::transport::net::canonical_peer_ip(ip);
        let Some(announce) = announce else {
            return Ok(());
        };
        if announce.public() != *public || !announce.verify() {
            return Err(TransportError::IdentityMismatch);
        }
        let group_member = self.group_member(public);
        {
            let mut state = self
                .privacy
                .state
                .write()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_mut() {
                if announce
                    .account_id()
                    .is_some_and(|a| a == self.account_id() || state.policy.allows(&a))
                    || group_member
                {
                    state
                        .proofs
                        .record(announce)
                        .map_err(|_| TransportError::AdmissionDenied)?;
                    state
                        .routes
                        .record(public, ip)
                        .map_err(|_| TransportError::AdmissionDenied)?;
                }
            }
        }
        self.roster
            .lock()
            .expect("roster lock not poisoned")
            .update(announce, ip, &self.user_id());
        Ok(())
    }
    fn guard_channel(&self, channel: &mut SecureChannel<TcpStream>, generation: u64) {
        let control = self.privacy.clone();
        channel.set_io_admission(
            control.gate.clone(),
            Arc::new(move || control.generation.load(Ordering::SeqCst) == generation),
        );
    }
    pub(in crate::node) async fn privacy_dial(
        &self,
        addr: std::net::SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        let mut addr = addr;
        addr.set_ip(crate::transport::net::canonical_peer_ip(addr.ip()));
        let own = self.own_presence();
        let _operation = self.privacy.gate.read().await;
        let generation = self.privacy.generation.load(Ordering::SeqCst);
        let announce = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(expected)
            .cloned();
        if !self.account_allowed(expected, announce.as_ref()) && !self.group_member(expected) {
            return Err(TransportError::AdmissionDenied);
        }
        let mut channel = tokio::time::timeout(super::transport::HANDSHAKE_TIMEOUT, async {
            let stream = TcpStream::connect(addr).await?;
            stream.set_nodelay(true)?;
            SecureChannel::connect_with_presence(
                stream,
                &self.identity,
                Some(expected),
                own.as_ref(),
            )
            .await
        })
        .await
        .map_err(|_| TransportError::Noise("dial timed out".into()))??;
        if let Some(new) = channel.peer_announcement() {
            let state = self
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_ref() {
                if let Some(old) = state.proofs.by_author(&expected.ed25519_pub) {
                    if old.public() != new.public() || old.account_id() != new.account_id() {
                        return Err(TransportError::AdmissionDenied);
                    }
                }
            }
        }
        self.remember_peer(
            channel.peer_identity(),
            channel.peer_announcement(),
            addr.ip(),
        )?;
        self.guard_channel(&mut channel, generation);
        Ok(channel)
    }
    pub(in crate::node) async fn privacy_accept(
        &self,
        stream: TcpStream,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        let ip = stream.peer_addr()?.ip();
        let own = self.own_presence();
        let _operation = self.privacy.gate.read().await;
        let generation = self.privacy.generation.load(Ordering::SeqCst);
        let mut channel = tokio::time::timeout(
            super::transport::HANDSHAKE_TIMEOUT,
            SecureChannel::accept_with_presence(stream, &self.identity, own.as_ref(), |peer| {
                self.admitted(peer)
            }),
        )
        .await
        .map_err(|_| TransportError::Noise("accept timed out".into()))??;
        self.remember_peer(channel.peer_identity(), channel.peer_announcement(), ip)?;
        self.guard_channel(&mut channel, generation);
        Ok(channel)
    }
}

#[cfg(test)]
#[path = "privacy_runtime_tests.rs"]
mod tests;

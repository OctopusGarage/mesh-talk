//! Serialized durable privacy decisions and authenticated disclosure boundaries.
use super::peer_directory::PeerDirectory;
use super::*;
use crate::discovery::{Announce, visibility::DiscoveryVisibility};
use crate::identity::device::PublicIdentity;
use crate::transport::{SecureChannel, TransportError, VerifiedPeer};
use std::{
    io,
    path::Path,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::net::TcpStream;

pub(in crate::node) struct PrivacyState {
    pub policy: PrivacyPolicy,
    pub proofs: PeerDirectory,
    pub own: Announce,
    pub visibility: Arc<DiscoveryVisibility>,
    pub routes: super::privacy_routes::RouteCache,
    pub sent_manifest_scopes: crate::storage::record_log::EncryptedRecordLog<StoredManifestScope>,
    pub scope_repair_needed: bool,
    pub legacy_scope_ids: std::collections::HashSet<crate::eventlog::EventId>,
    pub file_scopes: std::collections::HashMap<crate::eventlog::ConversationId, Vec<ManifestScope>>,
}
#[derive(Clone, PartialEq, Eq)]
pub(in crate::node) struct ManifestScope {
    pub parent: crate::eventlog::ConversationId,
    pub author: crate::eventlog::Author,
    pub event: crate::eventlog::EventId,
}
/// Local encrypted metadata only: never included in message history or wire frames.
#[derive(serde::Serialize, serde::Deserialize)]
pub(in crate::node) struct StoredManifestScope {
    file: crate::eventlog::ConversationId,
    parent: crate::eventlog::ConversationId,
    author: crate::eventlog::Author,
    event: crate::eventlog::EventId,
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
    /// Restrict public discovery to enumerated physical LAN interfaces. Known,
    /// explicitly authorized contacts can still use existing TCP routes.
    pub fn physical_lan_only(&self) -> bool {
        self.physical_lan_only.load(Ordering::Acquire)
    }

    /// Startup-only policy. Call before configure_privacy and network launch.
    pub fn initialize_physical_lan_only(&self, enabled: bool) {
        self.physical_lan_only.store(enabled, Ordering::Release);
    }

    pub async fn set_physical_lan_only(&self, enabled: bool) {
        self.set_physical_lan_only_if(enabled, |commit| commit())
            .await
            .unwrap();
    }

    /// Revalidate host authority after waiting for active discovery sends.
    pub async fn set_physical_lan_only_if(
        &self,
        enabled: bool,
        mut authorize: impl FnMut(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<()> {
        let _policy_guard = self.privacy.gate.write().await;
        let visibility = self
            .privacy
            .state
            .read()
            .expect("privacy state lock not poisoned")
            .as_ref()
            .map(|state| Arc::clone(&state.visibility));
        let mut commit_node = || {
            if self.physical_lan_only() == enabled {
                return Ok(());
            }
            self.physical_lan_only.store(enabled, Ordering::Release);
            self.privacy.generation.fetch_add(1, Ordering::SeqCst);
            self.sessions.clear();
            self.clear_bridge_introductions();
            self.bridge_notify.notify_one();
            Ok(())
        };
        if let Some(visibility) = visibility {
            visibility
                .set_physical_lan_only_if(enabled, |commit_visibility| {
                    authorize(&mut || {
                        commit_visibility()?;
                        commit_node()
                    })
                })
                .await
        } else {
            authorize(&mut commit_node)
        }
    }

    /// Configure before starting any network tasks. Unconfigured SDK constructors
    /// keep their legacy public behavior. Corrupt durable policy/proofs return errors.
    pub fn configure_privacy(
        &self,
        directory: &Path,
        password: &str,
        own: &Announce,
        visibility: Arc<DiscoveryVisibility>,
    ) -> io::Result<()> {
        if !own.valid_presence()
            || own.public() != self.identity.public()
            || own.account_id() != Some(self.account_id())
        {
            return Err(denied());
        }
        let policy = PrivacyPolicy::open(&directory.join("privacy.policy"), password)?;
        visibility.initialize_public(!policy.snapshot().invisible)?;
        visibility.initialize_physical_lan_only(self.physical_lan_only())?;
        let mut proofs = PeerDirectory::open(&directory.join("peer-proofs"), password)?;
        proofs.record_own(own, &self.identity.public())?;
        for device in self.linked_devices() {
            proofs.authorize_linked_device(device.clone(), &self.account_id())?;
            self.roster
                .lock()
                .expect("roster lock not poisoned")
                .authorize_linked_account(&device.public, &device.certificate);
        }
        let routes =
            super::privacy_routes::RouteCache::open(&directory.join("peer-routes"), password)?;
        let (mut sent_manifest_scopes, stored_scopes) =
            crate::storage::record_log::EncryptedRecordLog::<StoredManifestScope>::open(
                &directory.join("sent-manifest-scopes.log"),
                password,
                b"MTFSC1",
            )
            .map_err(io::Error::other)?;
        // The shared record log accepts a torn trailing append. Normalize it before
        // further writes so later valid scopes cannot land behind the torn bytes.
        sent_manifest_scopes
            .rewrite(&stored_scopes)
            .map_err(io::Error::other)?;
        let mut file_scopes: std::collections::HashMap<_, Vec<ManifestScope>> = Default::default();
        let mut legacy_scope_ids = std::collections::HashSet::new();
        if stored_scopes.len() > super::delivery_store::DeliveryLimits::default().completed_receipts
        {
            return Err(io::Error::other("legacy file scope capacity exhausted"));
        }
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
            for scope in stored_scopes {
                if scope.file != scope.parent
                    && events.get(&scope.event).is_some_and(|event| {
                        event.conversation_id == scope.parent && event.author == scope.author
                    })
                {
                    let scopes = file_scopes.entry(scope.file).or_default();
                    let restored = ManifestScope {
                        parent: scope.parent,
                        author: scope.author,
                        event: scope.event,
                    };
                    if !scopes.contains(&restored) {
                        legacy_scope_ids.insert(restored.event);
                        scopes.push(restored);
                    }
                }
            }
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
            sent_manifest_scopes,
            scope_repair_needed: false,
            legacy_scope_ids,
            file_scopes,
        };
        let mut state = self.privacy.state.write().map_err(|_| denied())?;
        if state.is_some() {
            return Err(denied());
        }
        *state = Some(next);
        drop(state);
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        self.reseed_live_file_book(&delivery);
        Ok(())
    }
    /// Return current local policy; unconfigured SDK nodes report the public default.
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
    /// Persist visibility and synchronize disclosure boundaries on a configured node.
    pub async fn set_invisible(&self, invisible: bool) -> io::Result<()> {
        self.set_invisible_if(invisible, || Ok(())).await
    }
    /// Recheck authorization after acquiring the policy gate, before durable mutation.
    /// Enabling invisibility first drains public announcements; failures restore visibility.
    pub async fn set_invisible_if(
        &self,
        invisible: bool,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        self.set_invisible_guarded(invisible, |commit| {
            authorize()?;
            commit()
        })
        .await
    }

    /// Await network/policy serialization, then let host authority enclose the
    /// synchronous policy commit and invalidation. No state lock is held when
    /// authority is acquired, and no host guard crosses a network await.
    /// The callback must execute the supplied commit exactly once while its
    /// authority is held and propagate the commit result; reject without calling
    /// the commit when authority is absent.
    pub async fn set_invisible_guarded(
        &self,
        invisible: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()> + Send,
    ) -> io::Result<()> {
        let _operation = self.privacy.gate.write().await;
        let (visibility, previous) = {
            let state = self.privacy.state.read().map_err(|_| denied())?;
            let state = state.as_ref().ok_or_else(denied)?;
            (state.visibility.clone(), state.policy.snapshot().invisible)
        };
        if previous == invisible {
            return authorize(&mut || Ok(()));
        }
        if invisible {
            visibility.set_public(false).await;
        }
        let result = authorize(&mut || {
            self.privacy
                .state
                .write()
                .map_err(|_| denied())?
                .as_mut()
                .ok_or_else(denied)?
                .policy
                .set_invisible(invisible)?;
            self.privacy.generation.fetch_add(1, Ordering::SeqCst);
            if invisible {
                self.clear_bridge_introductions();
            }
            Ok(())
        });
        if let Err(error) = result {
            if invisible {
                visibility.set_public(!previous).await;
            }
            return Err(error);
        }
        if !invisible {
            visibility.set_public(true).await;
            self.bridge_notify.notify_one();
        }
        Ok(())
    }
    /// Persist a manual grant/revocation; a grant also publishes private return presence.
    pub async fn set_allowed(&self, account: &str, allowed: bool) -> io::Result<()> {
        self.set_allowed_if(account, allowed, || Ok(())).await
    }
    /// Invoke authorization under the serialized policy gate before changing permission.
    /// Revocation invalidates prior channels without deleting local history.
    pub async fn set_allowed_if(
        &self,
        account: &str,
        allowed: bool,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        self.set_allowed_guarded(account, allowed, |commit| {
            authorize()?;
            commit()
        })
        .await
    }

    /// Enclose the complete durable grant/revocation after acquiring the policy
    /// gate. Grant validation precedes authority, matching the legacy `_if` API.
    /// The callback follows the commit contract of [`Self::set_invisible_guarded`].
    pub async fn set_allowed_guarded(
        &self,
        account: &str,
        allowed: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()> + Send,
    ) -> io::Result<()> {
        {
            let _operation = self.privacy.gate.write().await;
            self.change_allowed_locked_guarded(
                account,
                allowed,
                PermissionSource::Manual,
                authorize,
            )?;
        }
        if allowed {
            self.publish_presence(account).await;
        }
        Ok(())
    }
    /// Explicitly grant reply permission durably before publishing private presence.
    /// Background operations must not call this user-intent boundary.
    pub async fn initiate_contact(&self, account: &str) -> io::Result<()> {
        self.initiate_contact_if(account, || Ok(())).await
    }
    /// The enqueue acceptance path commits permission locally; its outbox owns
    /// transport scheduling, including publication of signed return presence.
    #[cfg(test)]
    pub(in crate::node) async fn initiate_contact_locally(&self, account: &str) -> io::Result<()> {
        self.change_allowed(account, true, PermissionSource::Initiated, || Ok(()))
            .await
    }
    /// Await policy serialization without holding the host session. Then enclose
    /// the complete synchronous local grant in the owner's operation guard.
    pub(in crate::node) async fn initiate_contact_locally_if(
        &self,
        account: &str,
        authorize: &mut impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let _operation = self.privacy.gate.write().await;
        authorize(&mut || {
            self.change_allowed_locked(account, true, PermissionSource::Initiated, || Ok(()))
                .map_err(|error| NodeError::Log(crate::eventlog::LogError::Io(error)))
        })
    }
    /// Recheck the active owner under the policy gate before committing an initiated grant.
    /// A prior manual grant is retained rather than demoted to initiated permission.
    pub async fn initiate_contact_if(
        &self,
        account: &str,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        self.initiate_contact_guarded(account, |commit| {
            authorize()?;
            commit()
        })
        .await
    }

    /// Enclose an initiated grant before best-effort private presence publication.
    /// The callback follows the commit contract of [`Self::set_invisible_guarded`].
    pub async fn initiate_contact_guarded(
        &self,
        account: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()> + Send,
    ) -> io::Result<()> {
        {
            let _operation = self.privacy.gate.write().await;
            self.change_allowed_locked_guarded(
                account,
                true,
                PermissionSource::Initiated,
                authorize,
            )?;
        }
        self.publish_presence(account).await;
        Ok(())
    }
    #[cfg(test)]
    async fn change_allowed(
        &self,
        account: &str,
        allowed: bool,
        source: PermissionSource,
        authorize: impl FnOnce() -> io::Result<()> + Send,
    ) -> io::Result<()> {
        let _operation = self.privacy.gate.write().await;
        self.change_allowed_locked(account, allowed, source, authorize)
    }
    /// Caller holds the privacy gate. Keep the legacy authorization/validation
    /// ordering; owner enqueue wraps this entire body before taking state locks.
    fn change_allowed_locked(
        &self,
        account: &str,
        allowed: bool,
        source: PermissionSource,
        authorize: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        self.change_allowed_locked_guarded(account, allowed, source, |commit| {
            authorize()?;
            commit()
        })
    }

    /// Caller holds the policy gate. Validate with a short state read, release
    /// it before host authorization, then acquire writes inside the commit.
    /// This retains session -> privacy-state lock order used by local enqueue.
    fn change_allowed_locked_guarded(
        &self,
        account: &str,
        allowed: bool,
        source: PermissionSource,
        authorize: impl FnOnce(&mut dyn FnMut() -> io::Result<()>) -> io::Result<()>,
    ) -> io::Result<()> {
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
        let matches: Vec<_> = announcements
            .into_iter()
            .filter(|a| a.account_id().as_deref() == Some(account) && a.verify())
            .collect();
        let prepared = {
            let guard = self.privacy.state.read().map_err(|_| denied())?;
            guard
                .as_ref()
                .map(|state| {
                    let before = state.policy.snapshot();
                    let name = if allowed {
                        let cached = state
                            .proofs
                            .announcements()
                            .into_iter()
                            .find(|a| a.account_id().as_deref() == Some(account));
                        Some(
                            matches
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
                                .ok_or_else(denied)?,
                        )
                    } else {
                        None
                    };
                    Ok::<_, io::Error>((before, name))
                })
                .transpose()?
        };
        let Some((before, name)) = prepared else {
            return authorize(&mut || Ok(()));
        };
        authorize(&mut || {
            let mut guard = self.privacy.state.write().map_err(|_| denied())?;
            let state = guard.as_mut().ok_or_else(denied)?;
            if let Some(name) = name.as_ref() {
                for proof in &matches {
                    let binding_changed =
                        state.proofs.account_for(&proof.public()) != proof.account_id();
                    state.proofs.record_explicit_account(proof)?;
                    if binding_changed {
                        // Invalidate even when a later policy save fails.
                        self.privacy.generation.fetch_add(1, Ordering::SeqCst);
                    }
                    if let Some(endpoint) = endpoints.iter().find(|p| p.public == proof.public()) {
                        state.routes.record(&endpoint.public, endpoint.addr.ip())?;
                    }
                }
                state.policy.grant(account, name, source.clone())?;
            } else {
                state.policy.revoke(account)?;
            }
            if before != state.policy.snapshot() {
                self.privacy.generation.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
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
    pub(in crate::node) async fn initiate_device_locally_if(
        &self,
        public: &PublicIdentity,
        authorize: &mut (
                 impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send
             ),
    ) -> Result<(), NodeError> {
        self.initiate_device_authorized(public, false, authorize)
            .await
    }

    pub(in crate::node) async fn initiate_device_if(
        &self,
        public: &PublicIdentity,
        authorize: &mut (
                 impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send
             ),
    ) -> Result<(), NodeError> {
        self.initiate_device_authorized(public, true, authorize)
            .await
    }

    async fn initiate_device_authorized(
        &self,
        public: &PublicIdentity,
        publish: bool,
        authorize: &mut (
                 impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send
             ),
    ) -> Result<(), NodeError> {
        let account = self
            .roster
            .lock()
            .expect("roster lock")
            .announcement(public)
            .and_then(Announce::account_id)
            .or_else(|| {
                self.privacy
                    .state
                    .read()
                    .ok()?
                    .as_ref()?
                    .proofs
                    .account_for(public)
            });
        if let Some(account) = account {
            self.initiate_contact_locally_if(&account, authorize)
                .await?;
            if publish {
                self.publish_presence(&account).await;
            }
            Ok(())
        } else {
            authorize(&mut || {
                if self.privacy_snapshot().invisible {
                    Err(NodeError::Authorization(
                        "private device has no verified account".into(),
                    ))
                } else {
                    Ok(())
                }
            })
        }
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
        let channel_ids = {
            let book = self.channels.lock().expect("channels lock not poisoned");
            book.channel_ids()
        };
        // Admission must use the durable, signed membership controls, including
        // leaves and transfers that the live book has not processed yet.
        let log = self.log.lock().expect("log lock not poisoned");
        channel_ids.into_iter().any(|id| {
            let events = log.events(&id);
            crate::node::channel::ChannelBook::projected_members(id, &events).is_some_and(
                |members| members.contains(public) && members.contains(&self.identity.public()),
            )
        })
    }

    pub(in crate::node) fn group_departure_peer(&self, public: &PublicIdentity) -> bool {
        let book = self.channels.lock().expect("channels lock not poisoned");
        book.departure_link(&self.user_id(), &public.user_id())
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
    /// Keep all fanout scopes durable without duplicating the account history bubble.
    pub(in crate::node) fn remember_sent_manifest_scope(
        &self,
        file: crate::eventlog::ConversationId,
        parent: crate::eventlog::ConversationId,
        author: crate::eventlog::Author,
        event: crate::eventlog::EventId,
    ) -> Result<(), crate::eventlog::LogError> {
        if file == parent {
            return Ok(());
        }
        let mut state = self
            .privacy
            .state
            .write()
            .map_err(|_| crate::eventlog::LogError::Io(denied()))?;
        if let Some(state) = state.as_mut() {
            let scope = ManifestScope {
                parent,
                author,
                event,
            };
            if !state
                .file_scopes
                .get(&file)
                .is_some_and(|scopes| scopes.contains(&scope))
            {
                if state.legacy_scope_ids.len()
                    >= super::delivery_store::DeliveryLimits::default().completed_receipts
                {
                    return Err(crate::eventlog::LogError::CorruptFile(
                        "legacy file scope capacity exhausted".into(),
                    ));
                }
                if state.scope_repair_needed {
                    let valid = state
                        .file_scopes
                        .iter()
                        .flat_map(|(file, scopes)| {
                            scopes.iter().map(|scope| StoredManifestScope {
                                file: *file,
                                parent: scope.parent,
                                author: scope.author,
                                event: scope.event,
                            })
                        })
                        .collect::<Vec<_>>();
                    state.sent_manifest_scopes.rewrite(&valid)?;
                    state.scope_repair_needed = false;
                }
                if let Err(error) = state.sent_manifest_scopes.append(&StoredManifestScope {
                    file,
                    parent,
                    author,
                    event,
                }) {
                    state.scope_repair_needed = true;
                    return Err(error);
                }
                state.legacy_scope_ids.insert(event);
                state.file_scopes.entry(file).or_default().push(scope);
            }
        }
        Ok(())
    }
    pub(in crate::node) fn update_presence(&self, own: &Announce) -> io::Result<()> {
        if !own.valid_presence()
            || own.public() != self.identity.public()
            || own.account_id() != Some(self.account_id())
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
        if !self
            .post_office_enabled
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return false;
        }
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
            if let Some(state) = state.as_ref()
                && let Some(new) = peer.announcement()
                && !state.proofs.accepts_binding(new)
            {
                return false;
            }
        }
        self.account_allowed(peer.public_identity(), peer.announcement())
            || self.group_member(peer.public_identity())
            || self.group_departure_peer(peer.public_identity())
    }
    // This bit describes disclosure on this connection. Physical-LAN-only
    // peers stay visible to their actual LAN neighbors while overlay contacts
    // receive private presence. Use the connected endpoint, including IPv6 scope.
    fn publicly_visible_at(&self, endpoint: std::net::SocketAddr) -> bool {
        if self.privacy_snapshot().invisible {
            return false;
        }
        if !self.physical_lan_only() {
            return true;
        }
        self.privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .is_some_and(|state| state.visibility.accepts_source(endpoint))
    }

    fn own_presence(&self) -> Option<Announce> {
        self.privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .map(|s| s.own.clone())
    }
    #[cfg(test)]
    pub(in crate::node) fn remember_peer(
        &self,
        public: &PublicIdentity,
        announce: Option<&Announce>,
        ip: std::net::IpAddr,
    ) -> Result<(), TransportError> {
        self.remember_peer_with_visibility(public, announce, ip, true)
    }

    pub(in crate::node) fn remember_peer_with_visibility(
        &self,
        public: &PublicIdentity,
        announce: Option<&Announce>,
        ip: std::net::IpAddr,
        publicly_visible: bool,
    ) -> Result<(), TransportError> {
        let ip = crate::transport::net::canonical_peer_ip(ip);
        let Some(announce) = announce else {
            return Ok(());
        };
        if announce.public() != *public || !announce.verify() {
            return Err(TransportError::IdentityMismatch);
        }
        let group_member = self.group_member(public) || self.group_departure_peer(public);
        #[cfg(test)]
        if let Some(hook) = self.remember_peer_hook.lock().unwrap().take() {
            hook();
        }
        {
            let mut state = self
                .privacy
                .state
                .write()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_mut() {
                if announce.account_id().is_some() {
                    state
                        .proofs
                        .record(announce)
                        .map_err(|_| TransportError::AdmissionDenied)?;
                }
                if !state.policy.snapshot().invisible
                    || announce
                        .account_id()
                        .is_some_and(|a| a == self.account_id() || state.policy.allows(&a))
                    || group_member
                {
                    state
                        .routes
                        .record(public, ip)
                        .map_err(|_| TransportError::AdmissionDenied)?;
                }
            }
        }
        let mut roster = self.roster.lock().expect("roster lock not poisoned");
        if publicly_visible {
            roster.update_authenticated(announce, ip, &self.user_id());
            roster.mark_authenticated(public, std::net::SocketAddr::new(ip, announce.tcp_port));
        } else {
            roster.forget(public);
        }
        Ok(())
    }

    /// Persist a verified discovery snapshot outside the roster lock. Historical
    /// proofs grant no permissions and never restore online presence.
    pub(in crate::node) fn cache_discovered_peers(&self) -> io::Result<()> {
        self.cached_peer_snapshot().map(|_| ())
    }

    fn discovery_snapshot(&self) -> (Vec<Announce>, Vec<crate::discovery::PeerRecord>) {
        let roster = self.roster.lock().expect("roster lock not poisoned");
        (roster.announcements(), roster.peers())
    }

    /// Return the captured peers whose certified identities were durably cached.
    /// Public accountless legacy peers retain verified in-memory compatibility;
    /// they are not persisted. Later arrivals belong to the next query.
    pub fn cached_peer_snapshot(&self) -> io::Result<Vec<crate::discovery::PeerRecord>> {
        Self::persist_discovery_snapshot(&self.privacy, self.discovery_snapshot())
    }

    /// Async hosts persist the captured snapshot on the blocking pool. No roster
    /// guard or Node reference is carried into the filesystem operation.
    pub async fn cached_peer_snapshot_async(
        &self,
    ) -> io::Result<Vec<crate::discovery::PeerRecord>> {
        let snapshot = self.discovery_snapshot();
        let privacy = self.privacy.clone();
        let work = self
            .runtime_work
            .admit()
            .ok_or_else(|| io::Error::other("node runtime retired"))?;
        #[cfg(test)]
        let hook = self.peer_snapshot_hook.lock().unwrap().take();
        tokio::task::spawn_blocking(move || {
            let _work = work;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            Self::persist_discovery_snapshot(&privacy, snapshot)
        })
        .await
        .map_err(|_| io::Error::other("peer cache task failed"))?
    }

    fn persist_discovery_snapshot(
        privacy: &PrivacyControl,
        (proofs, peers): (Vec<Announce>, Vec<crate::discovery::PeerRecord>),
    ) -> io::Result<Vec<crate::discovery::PeerRecord>> {
        let mut guard = privacy.state.write().map_err(|_| denied())?;
        let Some(state) = guard.as_mut() else {
            return Ok(peers);
        };
        let mut successful = Vec::new();
        for proof in proofs {
            if proof.account_id().is_none() {
                if !state.policy.snapshot().invisible
                    && proof.valid_presence()
                    && state.proofs.by_author(&proof.ed25519_pub).is_none()
                {
                    successful.push(proof);
                }
                continue;
            }
            if let Err(error) = state.proofs.record(&proof) {
                // Rejected bindings are ineligible peers; failed persistence is
                // an unavailable snapshot, never a successful empty roster.
                if error.kind() != io::ErrorKind::InvalidData {
                    return Err(error);
                }
                log::warn!("discovered peer proof was not cached");
                continue;
            }
            if let Some(peer) = peers.iter().find(|p| p.public == proof.public())
                && (if peer.routes.is_empty() {
                    vec![peer.addr.ip()]
                } else {
                    peer.routes
                        .iter()
                        .filter(|route| {
                            route.source != crate::discovery::roster::RouteSource::PostOffice
                        })
                        .map(|route| route.addr.ip())
                        .collect()
                })
                .into_iter()
                .any(|ip| state.routes.record(&peer.public, ip).is_err())
            {
                return Err(io::Error::other("discovered peer route was not cached"));
            }
            successful.push(proof);
        }
        Ok(peers
            .into_iter()
            .filter(|peer| {
                successful.iter().any(|proof| {
                    proof.public() == peer.public && proof.account_id() == peer.account_id
                })
            })
            .collect())
    }
    pub(in crate::node) fn guard_channel(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        generation: u64,
    ) {
        let control = self.privacy.clone();
        channel.set_host_generation(generation);
        channel.set_io_admission(
            control.gate.clone(),
            Arc::new(move || control.generation.load(Ordering::SeqCst) == generation),
        );
    }
    pub(in crate::node) fn privacy_route_candidates(
        &self,
        addr: std::net::SocketAddr,
        expected: &PublicIdentity,
        announce: Option<&crate::discovery::Announce>,
    ) -> Vec<std::net::SocketAddr> {
        let mut candidates = Vec::with_capacity(10);
        if let Some(preferred) = self
            .preferred_routes
            .lock()
            .expect("routes lock not poisoned")
            .get(&expected.user_id())
            .copied()
        {
            candidates.push(preferred);
        }
        let mut current_ports = std::collections::HashMap::new();
        let mut supplied_relay_address = false;
        if let Some(peer) = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .get(&expected.user_id())
            .filter(|peer| peer.public == *expected)
            .cloned()
        {
            let mut routes = peer.routes;
            supplied_relay_address = routes.iter().any(|route| {
                route.source == crate::discovery::roster::RouteSource::PostOffice
                    && route.addr == addr
            }) && !routes.iter().any(|route| {
                route.source != crate::discovery::roster::RouteSource::PostOffice
                    && route.addr == addr
            });
            routes
                .retain(|route| route.source != crate::discovery::roster::RouteSource::PostOffice);
            current_ports.extend(
                routes
                    .iter()
                    .map(|route| (route.addr.ip(), route.addr.port())),
            );
            routes.sort_by_key(|route| route.last_authenticated);
            candidates.extend(routes.into_iter().rev().map(|route| route.addr));
        }
        if let Ok(state) = self.privacy.state.read()
            && let Some(state) = state.as_ref()
        {
            let port = announce.map_or(addr.port(), |proof| proof.tcp_port);
            candidates.extend(
                state
                    .routes
                    .routes()
                    .into_iter()
                    .filter(|(public, _)| public == expected)
                    .map(|(_, ip)| std::net::SocketAddr::new(ip, port)),
            );
        }
        for candidate in &mut candidates {
            candidate.set_ip(crate::transport::net::canonical_peer_ip(candidate.ip()));
        }
        // A fresh signed announce replaces an old listening port on the same IP.
        // A successful dial cached for that IP must not silently resurrect the
        // superseded port; other IP paths remain available for fallback.
        candidates.retain(|candidate| {
            current_ports
                .get(&candidate.ip())
                .is_none_or(|port| *port == candidate.port())
        });
        // The caller may supply a fresh one-off endpoint (for example, a
        // pairing listener) before its signed port reaches the roster.
        if !supplied_relay_address {
            candidates.insert(0, addr);
        }
        let mut unique = std::collections::HashSet::new();
        candidates.retain(|candidate| unique.insert(*candidate));
        candidates.truncate(10);
        candidates
    }

    pub(in crate::node) async fn privacy_dial(
        &self,
        addr: std::net::SocketAddr,
        expected: &PublicIdentity,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        #[cfg(test)]
        let mut timing = super::delivery_runtime::TestTiming::new("dial-own-presence");
        let permits = self.sessions.outbound().await?;
        let mut addr = addr;
        addr.set_ip(crate::transport::net::canonical_peer_ip(addr.ip()));
        #[cfg(test)]
        timing.phase("dial-privacy-gate");
        let _operation = self.privacy.gate.read().await;
        let own = self.own_presence();
        #[cfg(test)]
        timing.phase("dial-admission");
        let generation = self.privacy.generation.load(Ordering::SeqCst);
        let announce = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(expected)
            .cloned();
        if !self.account_allowed(expected, announce.as_ref())
            && !self.group_member(expected)
            && !self.group_departure_peer(expected)
        {
            return Err(TransportError::AdmissionDenied);
        }
        let candidates = self.privacy_route_candidates(addr, expected, announce.as_ref());
        let mut last_error = TransportError::Noise("no peer route available".into());
        let mut successful = None;
        for candidate in candidates {
            let attempt = tokio::time::timeout(Duration::from_secs(3), async {
                #[cfg(test)]
                timing.phase("dial-tcp-connect");
                let stream = TcpStream::connect(candidate).await?;
                #[cfg(test)]
                {
                    timing.socket(&stream);
                    timing.phase("dial-noise-presence");
                }
                stream.set_nodelay(true)?;
                crate::transport::net::set_tcp_keepalive(&stream);
                let publicly_visible = self.publicly_visible_at(stream.peer_addr()?);
                SecureChannel::connect_with_visibility(
                    stream,
                    &self.identity,
                    Some(expected),
                    own.as_ref(),
                    publicly_visible,
                )
                .await
            })
            .await;
            match attempt {
                Ok(Ok(channel)) => {
                    successful = Some((candidate, channel));
                    break;
                }
                Ok(Err(error)) => last_error = error,
                Err(_) => last_error = TransportError::Noise("dial timed out".into()),
            }
        }
        let requested_addr = addr;
        let (addr, mut channel) = successful.ok_or(last_error)?;
        channel.retain_connection_permits(permits);
        #[cfg(test)]
        timing.phase("dial-proof-validation");
        if let Some(new) = channel.peer_announcement() {
            let state = self
                .privacy
                .state
                .read()
                .expect("privacy lock not poisoned");
            if let Some(state) = state.as_ref()
                && !state.proofs.accepts_binding(new)
            {
                return Err(TransportError::AdmissionDenied);
            }
        }
        #[cfg(test)]
        timing.phase("dial-remember-peer");
        self.remember_peer_with_visibility(
            channel.peer_identity(),
            channel.peer_announcement(),
            addr.ip(),
            channel.peer_publicly_visible(),
        )?;
        self.preferred_routes
            .lock()
            .expect("routes lock not poisoned")
            .insert(expected.user_id(), addr);
        self.guard_channel(&mut channel, generation);
        self.note_dialed_route(requested_addr, &channel);
        #[cfg(test)]
        timing.finish();
        Ok(channel)
    }
    #[cfg(test)]
    pub(in crate::node) async fn privacy_accept(
        &self,
        stream: TcpStream,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        let permits = self.sessions.inbound().await?;
        self.privacy_accept_with_permits(stream, permits).await
    }

    pub(in crate::node) async fn privacy_accept_with_permits(
        &self,
        stream: TcpStream,
        permits: Vec<tokio::sync::OwnedSemaphorePermit>,
    ) -> Result<SecureChannel<TcpStream>, TransportError> {
        #[cfg(test)]
        let mut timing = super::delivery_runtime::TestTiming::new("accept-tcp-ready");
        #[cfg(test)]
        timing.socket(&stream);
        let endpoint = stream.peer_addr()?;
        let ip = endpoint.ip();
        stream.set_nodelay(true)?;
        crate::transport::net::set_tcp_keepalive(&stream);
        #[cfg(test)]
        timing.phase("accept-own-presence");
        #[cfg(test)]
        timing.phase("accept-privacy-gate");
        let _operation = self.privacy.gate.read().await;
        let own = self.own_presence();
        let publicly_visible = self.publicly_visible_at(endpoint);
        #[cfg(test)]
        timing.phase("accept-noise-presence");
        let generation = self.privacy.generation.load(Ordering::SeqCst);
        let mut channel = tokio::time::timeout(
            super::transport::HANDSHAKE_TIMEOUT,
            SecureChannel::accept_with_visibility(
                stream,
                &self.identity,
                own.as_ref(),
                publicly_visible,
                |peer| {
                    self.admitted(peer)
                        || self.pairing_retry_allowed(peer.public_identity(), peer.announcement())
                },
            ),
        )
        .await
        .map_err(|_| TransportError::Noise("accept timed out".into()))??;
        #[cfg(test)]
        timing.phase("accept-remember-peer");
        channel.retain_connection_permits(permits);
        if self.pairing_retry_allowed(channel.peer_identity(), channel.peer_announcement()) {
            channel.restrict_to_pairing();
        } else {
            self.remember_peer_with_visibility(
                channel.peer_identity(),
                channel.peer_announcement(),
                ip,
                channel.peer_publicly_visible(),
            )?;
        }
        self.guard_channel(&mut channel, generation);
        #[cfg(test)]
        timing.finish();
        Ok(channel)
    }
}

#[cfg(test)]
#[path = "privacy_runtime_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "session_pool_tests.rs"]
mod session_pool_tests;

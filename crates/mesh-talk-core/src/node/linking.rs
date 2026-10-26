//! Multi-device pairing: linking codes, the pairing exchange, and account backfill. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::account::Account;
use crate::identity::device::PublicIdentity;
use crate::node::pairing::PendingLink;
use crate::transport::{SecureChannel, TransportError};
use tokio::net::TcpStream;

/// A pairing code is valid for this long after it's shown (defense-in-depth TTL).
const PAIRING_TTL_MS: u64 = 120_000;
/// Wrong proofs before a pending code is invalidated (anti-brute-force, belt-and-braces).
const MAX_PAIRING_ATTEMPTS: u32 = 5;
const MAX_PAIRING_BACKFILL_RECORDS: usize = 4_096;
const MAX_PAIRING_BACKFILL_BYTES: usize = 64 * 1024 * 1024;

/// Includes admission/permit waits and staggered route attempts. Individual
/// authentication attempts retain the transport owner's adaptive deadlines.
const PAIRING_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// Pairing stages are distinct from ordinary delivery and mailbox custody.
#[derive(Debug)]
pub enum LinkingError {
    NoDirectRoute,
    Connection(TransportError),
    ResponseTimeout,
    /// Clean EOF before any response frame; only this permits legacy negotiation.
    Closed,
    UpgradeRequired,
    InvalidResponse,
    Protocol(TransportError),
}

impl std::fmt::Display for LinkingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoDirectRoute => write!(
                f,
                "device linking needs a direct connection over a shared LAN or reachable VPN; a post office cannot carry pairing"
            ),
            Self::Connection(error) => write!(
                f,
                "could not connect directly to the pairing device: {error}"
            ),
            Self::ResponseTimeout => write!(
                f,
                "pairing response timed out; keep the other device open and generate a fresh code before trying again"
            ),
            Self::Closed => write!(
                f,
                "the other device closed the pairing request; check its current code and upgrade it before trying again"
            ),
            Self::UpgradeRequired => write!(
                f,
                "the source device does not support the required account linking data; prepare account access on it, check the code and upgrade it before trying again"
            ),
            Self::InvalidResponse => {
                write!(f, "the other device returned an invalid pairing response")
            }
            Self::Protocol(error) => write!(f, "pairing exchange failed: {error}"),
        }
    }
}

impl std::error::Error for LinkingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connection(error) | Self::Protocol(error) => Some(error),
            _ => None,
        }
    }
}

impl Node {
    /// Enter "link a device" mode: generate a one-time code to display. The next valid
    /// pairing request from a device proving this code is served the account secret.
    pub fn start_linking(&self) -> String {
        let mut guard = self.pending_link.lock().expect("pending_link mutex");
        // Idempotent: if an unexpired code is already pending (e.g. the UI double-fired
        // the button), return it rather than invalidating the code the user may already
        // be reading aloud. A fresh code requires stop_linking (or letting it expire).
        if let Some(pl) = guard.as_ref()
            && now_millis().saturating_sub(pl.created_at_ms) <= PAIRING_TTL_MS
        {
            return pl.code.as_hex();
        }
        let code = PairingCode::generate();
        let hex = code.as_hex();
        *guard = Some(PendingLink {
            code,
            created_at_ms: now_millis(),
            attempts: 0,
        });
        hex
    }

    /// Leave linking mode (clear any pending code).
    pub fn stop_linking(&self) {
        *self.pending_link.lock().expect("pending_link mutex") = None;
    }

    /// Joiner side: dial `addr` (the linker, pinned to `peer_public`), prove `code_hex`,
    /// and receive the account secret + a certificate for this device.
    pub async fn link_to_device(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
    ) -> Result<LinkedAccount, NodeError> {
        self.link_to_device_if(addr, peer_public, code_hex, |commit| commit())
            .await
    }

    pub async fn link_to_device_if(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<LinkedAccount, NodeError> {
        self.link_to_device_version_if(
            addr,
            peer_public,
            code_hex,
            false,
            |_| Ok(()),
            &mut authorize,
        )
        .await
    }
    pub async fn link_to_device_with_account_access_checked_if(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
        preflight: impl FnMut(&super::AccountAccessSnapshot) -> Result<(), NodeError> + Send,
        authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send,
    ) -> Result<LinkedAccount, NodeError> {
        self.link_to_device_version_if(addr, peer_public, code_hex, true, preflight, authorize)
            .await
    }
    async fn link_to_device_version_if(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code_hex: &str,
        access_required: bool,
        mut preflight: impl FnMut(&super::AccountAccessSnapshot) -> Result<(), NodeError> + Send,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<LinkedAccount, NodeError> {
        let code = PairingCode::from_hex(code_hex)
            .ok_or_else(|| NodeError::Channel("invalid pairing code".into()))?;
        self.initiate_device_if(peer_public, &mut authorize).await?;
        let (wanted, wanted_revision) = self.device_sync_policy_snapshot();
        let (mut channel, resp, created_at_ms, access, profile) = self
            .pairing_response(addr, peer_public, &code, wanted.history, access_required)
            .await
            .map_err(|error| {
                if access_required
                    && matches!(error, LinkingError::Closed | LinkingError::InvalidResponse)
                {
                    NodeError::Linking(LinkingError::UpgradeRequired)
                } else {
                    NodeError::Linking(error)
                }
            })?;
        // Verify the returned cert really binds THIS device to THAT account.
        if !resp.cert.verify()
            || resp.cert.device_ed25519_pub != self.identity.public().ed25519_pub
            || resp.cert.account_ed25519_pub != resp.account_ed25519_pub
        {
            return Err(NodeError::Channel("pairing cert invalid".into()));
        }
        if access_required {
            let expected_account = self
                .roster
                .lock()
                .expect("roster mutex")
                .get(&peer_public.user_id())
                .filter(|peer| peer.public == *peer_public)
                .and_then(|peer| peer.account_id.clone());
            if expected_account.is_some_and(|id| id != resp.cert.account_id()) {
                return Err(NodeError::Linking(LinkingError::InvalidResponse));
            }
            let snapshot = access
                .as_ref()
                .ok_or(NodeError::Linking(LinkingError::UpgradeRequired))?;
            if !snapshot.valid_for(&resp.account_ed25519_pub) {
                return Err(NodeError::Linking(LinkingError::InvalidResponse));
            }
            if self
                .account_access
                .lock()
                .expect("account access mutex")
                .current(&snapshot.account_id())
                .is_some_and(|current| {
                    current.revision() > snapshot.revision()
                        || (current.revision() == snapshot.revision() && current != *snapshot)
                })
            {
                return Err(NodeError::Linking(LinkingError::InvalidResponse));
            }
            // Check host collisions before any target account credential is installed.
            preflight(snapshot)?;
        }
        // Receive the linker's account-history backfill (records until an empty frame),
        // so this device starts populated. Best-effort: stop on any recv error.
        let backfill = receive_pairing_backfill(&mut channel, wanted.history, || {
            self.device_sync_policy_snapshot().1 == wanted_revision
        })
        .await?;
        let account =
            Account::from_secret_bytes_with_created_at(resp.account_secret, created_at_ms)
                .ok_or_else(|| NodeError::Channel("invalid account creation metadata".into()))?;
        // Defense-in-depth: the cert binds `account_ed25519_pub`; make sure the secret we were
        // handed actually derives that same key, so a buggy/malicious linker can't pair us onto
        // a cert for one account while seeding a secret for another.
        if account.public().ed25519_pub != resp.account_ed25519_pub {
            return Err(NodeError::Channel(
                "pairing account secret does not match the certified account key".into(),
            ));
        }
        if profile.as_ref().is_some_and(|profile| {
            !profile.verify()
                || profile.account_ed25519_pub != resp.account_ed25519_pub
                || profile.created_at_ms != created_at_ms
        }) {
            return Err(NodeError::Linking(LinkingError::InvalidResponse));
        }
        if let Some(profile) = profile {
            authorize(&mut || {
                self.profiles
                    .lock()
                    .expect("profiles mutex")
                    .record(profile.clone())
                    .map_err(NodeError::Log)?;
                Ok(())
            })?;
        }
        if let Some(snapshot) = access {
            let mut persisted = false;
            authorize(&mut || {
                self.account_access
                    .lock()
                    .expect("account access mutex")
                    .merge(snapshot.clone())
                    .map_err(NodeError::Log)?;
                persisted = true;
                Ok(())
            })?;
            if !persisted {
                return Err(NodeError::Authorization(
                    "account access installation was not authorized".into(),
                ));
            }
        }
        if wanted.history
            && self.device_sync_policy_snapshot().1 == wanted_revision
            && self.device_sync_policy().history
        {
            authorize(&mut || {
                let _commit = self
                    .account_sync_commit
                    .lock()
                    .expect("account sync commit mutex not poisoned");
                if self.device_sync_policy_snapshot().1 != wanted_revision {
                    return Err(NodeError::Authorization(
                        "account sync policy changed".into(),
                    ));
                }
                self.import_account_backfill_for_account(&account.account_id(), &backfill)
            })?;
        }
        Ok(LinkedAccount {
            secret: resp.account_secret,
            created_at_ms,
            account_id: account.account_id(),
        })
    }

    /// Metadata-bearing V3 and credential-bearing V4 never downgrade on failure.
    async fn pairing_response(
        &self,
        addr: std::net::SocketAddr,
        peer_public: &PublicIdentity,
        code: &PairingCode,
        history: bool,
        access_required: bool,
    ) -> Result<
        (
            SecureChannel<TcpStream>,
            PairingResponse,
            Option<u64>,
            Option<super::AccountAccessSnapshot>,
            Option<super::AccountProfile>,
        ),
        LinkingError,
    > {
        let announce = self
            .roster
            .lock()
            .expect("roster lock not poisoned")
            .announcement(peer_public)
            .cloned();
        if self
            .privacy_route_candidates(addr, peer_public, announce.as_ref())
            .is_empty()
        {
            return Err(LinkingError::NoDirectRoute);
        }
        let mut channel = tokio::time::timeout(
            PAIRING_CONNECT_TIMEOUT,
            self.privacy_dial(addr, peer_public),
        )
        .await
        .map_err(|_| {
            LinkingError::Connection(TransportError::Io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "direct connection timed out",
            )))
        })?
        .map_err(LinkingError::Connection)?;
        let budget = self
            .sessions
            .control_timeout(peer_public, channel.tcp_peer_addr().unwrap_or(addr));
        let response = pairing_exchange(
            &mut channel,
            &self.identity.public(),
            peer_public,
            code,
            history,
            access_required,
            budget,
        )
        .await?;
        Ok((channel, response.0, response.1, response.2, response.3))
    }

    /// Recovery after a joiner disconnects before saving its adopted secret.
    /// This permits only an inbound, pairing-only handshake; a valid current
    /// one-time code is still required before any account data is released.
    pub(in crate::node) fn pairing_retry_allowed(
        &self,
        public: &PublicIdentity,
        announce: Option<&crate::discovery::Announce>,
    ) -> bool {
        let pending = self.pending_link.lock().expect("pending_link mutex");
        if pending
            .as_ref()
            .is_none_or(|pl| now_millis().saturating_sub(pl.created_at_ms) > PAIRING_TTL_MS)
        {
            return false;
        }
        if announce.is_none() {
            return true;
        }
        let Some(announce) = announce.filter(|a| a.public() == *public && a.valid_presence())
        else {
            return false;
        };
        self.privacy
            .state
            .read()
            .expect("privacy lock not poisoned")
            .as_ref()
            .is_some_and(|state| state.proofs.retired_link_binding(announce))
    }

    /// Linker side: handle a pairing request on an inbound (authenticated) channel.
    /// Releases the account secret only if a code is pending AND the request both
    /// proves it and binds the authenticated channel peer. Clears the code on success.
    pub(in crate::node) async fn serve_pairing_negotiated(
        self: &std::sync::Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        req: PairingRequest,
        selected: Option<bool>,
        metadata: bool,
    ) {
        self.serve_pairing_version(channel, req, selected, metadata, false)
            .await;
    }
    pub(in crate::node) async fn serve_pairing_access(
        self: &std::sync::Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        req: PairingRequest,
        history: bool,
    ) {
        self.serve_pairing_version(channel, req, Some(history), true, true)
            .await;
    }
    async fn serve_pairing_version(
        self: &std::sync::Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        req: PairingRequest,
        selected: Option<bool>,
        metadata: bool,
        access_required: bool,
    ) {
        let access = if access_required {
            match self.account_access_snapshot() {
                Some(access) => Some(access),
                None => return,
            }
        } else {
            None
        };
        // Bind the proof to the Noise-authenticated peer.
        if req.joiner != *channel.peer_identity() {
            return;
        }
        let my_ed = self.identity.public().ed25519_pub;
        let verifies = |code: &PairingCode| {
            if access_required {
                code.verify_access(
                    &my_ed,
                    &req.joiner.ed25519_pub,
                    selected.unwrap_or(false),
                    &req.tag,
                )
            } else {
                match selected {
                    Some(history) if metadata => {
                        code.verify_metadata(&my_ed, &req.joiner.ed25519_pub, history, &req.tag)
                    }
                    Some(history) => {
                        code.verify_selected(&my_ed, &req.joiner.ed25519_pub, history, &req.tag)
                    }
                    None => code.verify(&my_ed, &req.joiner.ed25519_pub, &req.tag),
                }
            }
        };
        // Decide under the lock: reject if there's no pending code, it has expired, or the
        // proof is wrong (counting the attempt and burning the code after too many).
        {
            let _operation = self.privacy.gate.read().await;
            if !channel.io_admitted()
                || (!self.account_allowed(channel.peer_identity(), channel.peer_announcement())
                    && !self.pairing_retry_allowed(
                        channel.peer_identity(),
                        channel.peer_announcement(),
                    ))
            {
                return;
            }
            let mut guard = self.pending_link.lock().expect("pending_link mutex");
            let Some(pl) = guard.as_mut() else {
                return;
            };
            if now_millis().saturating_sub(pl.created_at_ms) > PAIRING_TTL_MS {
                *guard = None; // expired — invalidate
                return;
            }
            if !verifies(&pl.code) {
                pl.attempts += 1;
                if pl.attempts >= MAX_PAIRING_ATTEMPTS {
                    *guard = None; // too many wrong guesses — burn the code
                }
                return;
            }
        }
        // Serialize durable enrollment with privacy changes. A second request
        // authenticated before the transition cannot enroll under a stale guard.
        let transition = self.privacy.gate.write().await;
        if !channel.io_admitted() {
            return;
        }
        let cert = {
            let mut pending = self.pending_link.lock().expect("pending_link mutex");
            if pending.as_ref().is_none_or(|pl| {
                now_millis().saturating_sub(pl.created_at_ms) > PAIRING_TTL_MS
                    || !verifies(&pl.code)
            }) {
                return;
            }
            let cert = self.account.certify(&req.joiner.ed25519_pub);
            if self
                .account_devices
                .lock()
                .expect("account_devices mutex not poisoned")
                .insert(super::account_devices::AccountDevice {
                    public: req.joiner.clone(),
                    certificate: cert.clone(),
                })
                .is_err()
            {
                return;
            }
            // Enrollment is durable first: after a crash, privacy initialization
            // reconstructs this exact permitted transition from AccountDevices.
            {
                let mut state = self
                    .privacy
                    .state
                    .write()
                    .expect("privacy lock not poisoned");
                if let Some(state) = state.as_mut()
                    && state
                        .proofs
                        .authorize_linked_device(
                            super::account_devices::AccountDevice {
                                public: req.joiner.clone(),
                                certificate: cert.clone(),
                            },
                            &self.account_id(),
                        )
                        .is_err()
                {
                    return;
                }
            }
            // Old public presence belongs to the retired account. A fresh signed
            // own-account announce must establish the adopted device's new endpoint.
            self.roster
                .lock()
                .expect("roster lock not poisoned")
                .authorize_linked_account(&req.joiner, &cert);
            // Retire every pre-enrollment I/O guard, including inbound connections
            // retained by the other device. Only this one-shot pairing exchange may
            // finish under the new generation; ordinary retries require new auth.
            let generation = self
                .privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                .wrapping_add(1);
            self.sessions.clear();
            self.guard_channel(channel, generation);
            *pending = None; // atomically consume this exact authorized code
            cert
        };
        drop(transition);
        let (offered, policy_revision) = self.device_sync_policy_snapshot();
        let resp = PairingResponse {
            account_secret: self.account.secret_bytes(),
            account_ed25519_pub: self.account.public().ed25519_pub,
            cert,
        };
        let sent = if let Some(access) = access {
            super::pairing::PairingAccessResponse {
                account: resp,
                access,
                created_at_ms: self.account.created_at_ms(),
                profile: self.account_profile(&self.account_id()),
            }
            .send(channel)
            .await
        } else {
            let response = zeroize::Zeroizing::new(if metadata {
                resp.encode_metadata(self.account.created_at_ms())
            } else {
                resp.encode()
            });
            channel.send(&response).await
        };
        if sent.is_err() {
            return;
        }
        // Backfill: stream our account history so the new device starts populated,
        // then an empty frame as terminator. Best-effort — failures just mean the
        // joiner backfills nothing.
        if selected.unwrap_or(true) && offered.history {
            let mut total_bytes = 0usize;
            let Ok(records) = self
                .account_blocking(|node| Ok(node.export_account_backfill()))
                .await
            else {
                return;
            };
            for rec in records {
                if self.device_sync_policy_snapshot().1 != policy_revision {
                    return;
                }
                let encoded = rec.encode();
                if total_bytes.saturating_add(encoded.len()) > MAX_PAIRING_BACKFILL_BYTES {
                    break;
                }
                total_bytes += encoded.len();
                if channel.send(&encoded).await.is_err() {
                    return;
                }
            }
        }
        if self.device_sync_policy_snapshot().1 == policy_revision {
            let _ = channel.send(&[]).await;
        }
    }

    /// Export every account-history message we hold (sent + received) as transferable
    /// [`BackfillRecord`]s — for handing to a freshly-linked device. Only entries that
    /// are account `DmEnvelope`s are included (device DMs / channels are skipped). Sent
    /// entries are attributed to our own account; received to the recorded sender.
    pub(in crate::node) fn export_account_backfill(&self) -> Vec<BackfillRecord> {
        let my_account = self.account.account_id();
        let mut out = Vec::new();
        let mut total_bytes = 0usize;
        // Fixed-width bincode fields plus MTBF1; compute before cloning plaintext.
        let mut selected = 0usize;
        let mut remaining = |from_len: usize, plaintext_len: usize| {
            let encoded_len = 5usize + 32 + 8 + from_len + 8 + 8 + plaintext_len + 32;
            if selected >= MAX_PAIRING_BACKFILL_RECORDS
                || total_bytes.saturating_add(encoded_len) > MAX_PAIRING_BACKFILL_BYTES
            {
                return false;
            }
            total_bytes += encoded_len;
            selected += 1;
            true
        };
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for conv in sentlog.conversations() {
                for e in sentlog.entries_ref(&conv) {
                    if let Some(env) = DmEnvelope::decode(&e.plaintext) {
                        if self.recipient_expired(&env.body, EventId::new(env.msg_id), e.wall_clock)
                        {
                            continue;
                        }
                        if env.route.sender_account != my_account
                            || *super::conversation::account_conversation_id(
                                &my_account,
                                &env.route.recipient_account,
                            )
                            .as_bytes()
                                != *conv.as_bytes()
                        {
                            continue;
                        }
                        if !remaining(my_account.len(), e.plaintext.len()) {
                            return out;
                        }
                        out.push(BackfillRecord {
                            conv: *conv.as_bytes(),
                            from: my_account.clone(),
                            wall_clock: e.wall_clock,
                            plaintext: e.plaintext.clone(),
                            event_id: env.msg_id,
                        });
                    }
                }
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for conv in received.conversations() {
                for e in received.entries_ref(&conv) {
                    if let Some(envelope) = DmEnvelope::decode(&e.plaintext) {
                        if self.recipient_expired(&envelope.body, e.event_id, e.wall_clock) {
                            continue;
                        }
                        let other = if envelope.route.sender_account == my_account {
                            &envelope.route.recipient_account
                        } else if envelope.route.recipient_account == my_account {
                            &envelope.route.sender_account
                        } else {
                            continue;
                        };
                        if envelope.route.sender_account != e.from
                            || *super::conversation::account_conversation_id(&my_account, other)
                                .as_bytes()
                                != *conv.as_bytes()
                        {
                            continue;
                        }
                        if !remaining(e.from.len(), e.plaintext.len()) {
                            return out;
                        }
                        out.push(BackfillRecord {
                            conv: *conv.as_bytes(),
                            from: e.from.clone(),
                            wall_clock: e.wall_clock,
                            plaintext: e.plaintext.clone(),
                            event_id: envelope.msg_id,
                        });
                    }
                }
            }
        }
        out
    }

    /// Import backfilled account history into our received store (used by a freshly
    /// linked device). Each record is keyed by its account conversation id, which is
    /// identical on both devices once they share the account.
    pub(in crate::node) fn import_account_backfill(
        &self,
        records: &[BackfillRecord],
    ) -> Result<(), NodeError> {
        self.import_account_backfill_for_account(&self.account_id(), records)
    }

    pub(super) fn import_account_backfill_for_account(
        &self,
        account_id: &str,
        records: &[BackfillRecord],
    ) -> Result<(), NodeError> {
        for r in records {
            let Some(envelope) = DmEnvelope::decode(&r.plaintext) else {
                continue;
            };
            let own = account_id;
            let other = if envelope.route.sender_account == own {
                &envelope.route.recipient_account
            } else if envelope.route.recipient_account == own {
                &envelope.route.sender_account
            } else {
                continue;
            };
            if envelope.route.sender_account != r.from
                || *super::conversation::account_conversation_id(own, other).as_bytes() != r.conv
                || envelope.msg_id != r.event_id
            {
                continue;
            }
            if self.recipient_expired(&envelope.body, EventId::new(r.event_id), r.wall_clock) {
                continue;
            }
            if self.private_dm_message_recalled(other, EventId::new(r.event_id), &r.from) {
                continue;
            }
            let conversation = ConversationId::new(r.conv);
            if self
                .account_suppression
                .lock()
                .expect("account suppression mutex not poisoned")
                .contains(conversation, EventId::new(envelope.msg_id))
            {
                continue;
            }
            let already_sent = self
                .sentlog
                .lock()
                .expect("sentlog mutex not poisoned")
                .entries_ref(&conversation)
                .iter()
                .any(|entry| {
                    DmEnvelope::decode(&entry.plaintext)
                        .is_some_and(|existing| existing.msg_id == envelope.msg_id)
                });
            if already_sent {
                continue;
            }
            let mut received = self.received.lock().expect("received mutex not poisoned");
            if received.entries_ref(&conversation).iter().any(|entry| {
                DmEnvelope::decode(&entry.plaintext)
                    .is_some_and(|existing| existing.msg_id == envelope.msg_id)
            }) {
                continue;
            }
            received
                .record_durable(&super::received_log::ReceivedEntry {
                    conversation,
                    from: r.from.clone(),
                    wall_clock: r.wall_clock,
                    plaintext: r.plaintext.clone(),
                    event_id: EventId::new(r.event_id),
                })
                .map_err(NodeError::Log)?;
            self.note_account_history_change();
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "pairing_selection_tests.rs"]
mod pairing_selection_tests;

/// Shared wire exchange for authenticated relinking and accountless enrollment.
pub(super) async fn pairing_exchange(
    channel: &mut SecureChannel<TcpStream>,
    joiner: &PublicIdentity,
    peer_public: &PublicIdentity,
    code: &PairingCode,
    history: bool,
    access_required: bool,
    budget: std::time::Duration,
) -> Result<
    (
        PairingResponse,
        Option<u64>,
        Option<super::AccountAccessSnapshot>,
        Option<super::AccountProfile>,
    ),
    LinkingError,
> {
    let linker_ed = peer_public.ed25519_pub;
    let joiner = joiner.clone();
    let tag = if access_required {
        code.authenticator_access(&linker_ed, &joiner.ed25519_pub, history)
    } else {
        code.authenticator_metadata(&linker_ed, &joiner.ed25519_pub, history)
    };
    let request = PairingRequest { joiner, tag };
    let bytes = if access_required {
        request.encode_access(history)
    } else {
        request.encode_metadata(history)
    };
    let response = tokio::time::timeout(budget, async {
        channel.send(&bytes).await.map_err(LinkingError::Protocol)?;
        let bytes = zeroize::Zeroizing::new(channel.recv().await.map_err(|error| match error {
            TransportError::Io(ref io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
                LinkingError::Closed
            }
            error => LinkingError::Protocol(error),
        })?);
        if access_required {
            super::pairing::PairingAccessResponse::receive(channel, &bytes)
                .await
                .map(|r| (r.account, r.created_at_ms, Some(r.access), r.profile))
                .ok_or(LinkingError::InvalidResponse)
        } else {
            PairingResponse::decode_metadata(&bytes)
                .map(|(r, created_at)| (r, created_at, None, None))
                .ok_or(LinkingError::InvalidResponse)
        }
    })
    .await
    .map_err(|_| LinkingError::ResponseTimeout)??;
    Ok(response)
}

pub(super) async fn receive_pairing_backfill(
    channel: &mut SecureChannel<TcpStream>,
    history: bool,
    mut current: impl FnMut() -> bool,
) -> Result<Vec<BackfillRecord>, NodeError> {
    let mut records = Vec::new();
    let mut bytes = 0usize;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    while current() {
        let remaining = deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .min(std::time::Duration::from_secs(15));
        let frame = match tokio::time::timeout(remaining, channel.recv()).await {
            Ok(Ok(frame)) => frame,
            _ => break,
        };
        if frame.is_empty() {
            break;
        }
        if !history {
            return Err(NodeError::Channel(
                "unexpected pairing history while history sync is disabled".into(),
            ));
        }
        bytes = bytes.saturating_add(frame.len());
        if records.len() >= MAX_PAIRING_BACKFILL_RECORDS || bytes > MAX_PAIRING_BACKFILL_BYTES {
            return Err(NodeError::Channel("pairing history exceeds limit".into()));
        }
        match BackfillRecord::decode(&frame) {
            Some(record) => records.push(record),
            None => break,
        }
    }
    Ok(records)
}

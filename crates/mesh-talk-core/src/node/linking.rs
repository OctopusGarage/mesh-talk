//! Multi-device pairing: linking codes, the pairing exchange, and account backfill. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::account::Account;
use crate::identity::device::PublicIdentity;
use crate::node::pairing::PendingLink;
use crate::node::session::SessionError;
use crate::transport::SecureChannel;
use tokio::net::TcpStream;

/// A pairing code is valid for this long after it's shown (defense-in-depth TTL).
const PAIRING_TTL_MS: u64 = 120_000;
/// Wrong proofs before a pending code is invalidated (anti-brute-force, belt-and-braces).
const MAX_PAIRING_ATTEMPTS: u32 = 5;
const MAX_PAIRING_BACKFILL_RECORDS: usize = 4_096;
const MAX_PAIRING_BACKFILL_BYTES: usize = 64 * 1024 * 1024;

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
        let code = PairingCode::from_hex(code_hex)
            .ok_or_else(|| NodeError::Channel("invalid pairing code".into()))?;
        self.initiate_device(peer_public)
            .await
            .map_err(|e| NodeError::Log(crate::eventlog::LogError::Io(e)))?;
        let mut channel = self
            .privacy_dial(addr, peer_public)
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let tag = code.authenticator(
            &peer_public.ed25519_pub,
            &self.identity.public().ed25519_pub,
        );
        let req = PairingRequest {
            joiner: self.identity.public(),
            tag,
        };
        channel
            .send(&req.encode())
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp_bytes = channel
            .recv()
            .await
            .map_err(|e| NodeError::Session(SessionError::Transport(e)))?;
        let resp = PairingResponse::decode(&resp_bytes)
            .ok_or_else(|| NodeError::Channel("pairing rejected".into()))?;
        // Verify the returned cert really binds THIS device to THAT account.
        if !resp.cert.verify()
            || resp.cert.device_ed25519_pub != self.identity.public().ed25519_pub
            || resp.cert.account_ed25519_pub != resp.account_ed25519_pub
        {
            return Err(NodeError::Channel("pairing cert invalid".into()));
        }
        // Receive the linker's account-history backfill (records until an empty frame),
        // so this device starts populated. Best-effort: stop on any recv error.
        let mut backfill = Vec::new();
        let mut backfill_bytes = 0usize;
        loop {
            let frame = match channel.recv().await {
                Ok(f) => f,
                Err(_) => break,
            };
            if frame.is_empty() {
                break; // terminator
            }
            backfill_bytes = backfill_bytes.saturating_add(frame.len());
            if backfill.len() >= MAX_PAIRING_BACKFILL_RECORDS
                || backfill_bytes > MAX_PAIRING_BACKFILL_BYTES
            {
                return Err(NodeError::Channel("pairing history exceeds limit".into()));
            }
            match BackfillRecord::decode(&frame) {
                Some(rec) => backfill.push(rec),
                None => break,
            }
        }
        let account = Account::from_secret_bytes(resp.account_secret);
        // Defense-in-depth: the cert binds `account_ed25519_pub`; make sure the secret we were
        // handed actually derives that same key, so a buggy/malicious linker can't pair us onto
        // a cert for one account while seeding a secret for another.
        if account.public().ed25519_pub != resp.account_ed25519_pub {
            return Err(NodeError::Channel(
                "pairing account secret does not match the certified account key".into(),
            ));
        }
        if self.device_sync_policy().history {
            self.import_account_backfill_for_account(&account.account_id(), &backfill)?;
        }
        Ok(LinkedAccount {
            secret: resp.account_secret,
            account_id: account.account_id(),
        })
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
    pub(in crate::node) async fn serve_pairing(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        req: PairingRequest,
    ) {
        // Bind the proof to the Noise-authenticated peer.
        if req.joiner != *channel.peer_identity() {
            return;
        }
        let my_ed = self.identity.public().ed25519_pub;
        // Decide under the lock: reject if there's no pending code, it has expired, or the
        // proof is wrong (counting the attempt and burning the code after too many).
        {
            let _operation = self.privacy.gate.read().await;
            if !channel.io_admitted()
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
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
            if !pl.code.verify(&my_ed, &req.joiner.ed25519_pub, &req.tag) {
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
                    || !pl.code.verify(&my_ed, &req.joiner.ed25519_pub, &req.tag)
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
        let resp = PairingResponse {
            account_secret: self.account.secret_bytes(),
            account_ed25519_pub: self.account.public().ed25519_pub,
            cert,
        };
        if channel.send(&resp.encode()).await.is_err() {
            return;
        }
        // Backfill: stream our account history so the new device starts populated,
        // then an empty frame as terminator. Best-effort — failures just mean the
        // joiner backfills nothing.
        if self.device_sync_policy().history {
            let mut total_bytes = 0usize;
            for rec in self
                .export_account_backfill()
                .into_iter()
                .take(MAX_PAIRING_BACKFILL_RECORDS)
            {
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
        let _ = channel.send(&[]).await;
    }

    /// Export every account-history message we hold (sent + received) as transferable
    /// [`BackfillRecord`]s — for handing to a freshly-linked device. Only entries that
    /// are account `DmEnvelope`s are included (device DMs / channels are skipped). Sent
    /// entries are attributed to our own account; received to the recorded sender.
    pub(in crate::node) fn export_account_backfill(&self) -> Vec<BackfillRecord> {
        let my_account = self.account.account_id();
        let mut out = Vec::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for conv in sentlog.conversations() {
                for e in sentlog.entries(&conv) {
                    if let Some(env) = DmEnvelope::decode(&e.plaintext) {
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
                for e in received.entries(&conv) {
                    if let Some(envelope) = DmEnvelope::decode(&e.plaintext) {
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

    fn import_account_backfill_for_account(
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
                .record(
                    conversation,
                    r.from.clone(),
                    r.wall_clock,
                    &r.plaintext,
                    EventId::new(r.event_id),
                )
                .map_err(NodeError::Log)?;
        }
        Ok(())
    }
}

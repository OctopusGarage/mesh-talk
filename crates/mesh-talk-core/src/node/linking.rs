//! Multi-device pairing: linking codes, the pairing exchange, and account backfill. Split out of node.rs (one `impl Node` block per domain).

use super::node::now_millis;
use super::*;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::account::Account;
use crate::identity::device::PublicIdentity;
use crate::node::pairing::PendingLink;
use crate::node::session::SessionError;
use crate::node::transport::dial;
use crate::transport::SecureChannel;
use tokio::net::TcpStream;

/// A pairing code is valid for this long after it's shown (defense-in-depth TTL).
const PAIRING_TTL_MS: u64 = 120_000;
/// Wrong proofs before a pending code is invalidated (anti-brute-force, belt-and-braces).
const MAX_PAIRING_ATTEMPTS: u32 = 5;

impl Node {
    /// Enter "link a device" mode: generate a one-time code to display. The next valid
    /// pairing request from a device proving this code is served the account secret.
    pub fn start_linking(&self) -> String {
        let mut guard = self.pending_link.lock().expect("pending_link mutex");
        // Idempotent: if an unexpired code is already pending (e.g. the UI double-fired
        // the button), return it rather than invalidating the code the user may already
        // be reading aloud. A fresh code requires stop_linking (or letting it expire).
        if let Some(pl) = guard.as_ref() {
            if now_millis().saturating_sub(pl.created_at_ms) <= PAIRING_TTL_MS {
                return pl.code.as_hex();
            }
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
        let mut channel = dial(addr, &self.identity, Some(peer_public))
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
        loop {
            let frame = match channel.recv().await {
                Ok(f) => f,
                Err(_) => break,
            };
            if frame.is_empty() {
                break; // terminator
            }
            match BackfillRecord::decode(&frame) {
                Some(rec) => backfill.push(rec),
                None => break,
            }
        }
        self.import_account_backfill(&backfill);

        let account = Account::from_secret_bytes(resp.account_secret);
        // Defense-in-depth: the cert binds `account_ed25519_pub`; make sure the secret we were
        // handed actually derives that same key, so a buggy/malicious linker can't pair us onto
        // a cert for one account while seeding a secret for another.
        if account.public().ed25519_pub != resp.account_ed25519_pub {
            return Err(NodeError::Channel(
                "pairing account secret does not match the certified account key".into(),
            ));
        }
        Ok(LinkedAccount {
            secret: resp.account_secret,
            account_id: account.account_id(),
        })
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
        if req.joiner.ed25519_pub != channel.peer_identity().ed25519_pub {
            return;
        }
        let my_ed = self.identity.public().ed25519_pub;
        // Decide under the lock: reject if there's no pending code, it has expired, or the
        // proof is wrong (counting the attempt and burning the code after too many).
        {
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
        let cert = self.account.certify(&req.joiner.ed25519_pub);
        let resp = PairingResponse {
            account_secret: self.account.secret_bytes(),
            account_ed25519_pub: self.account.public().ed25519_pub,
            cert,
        };
        if channel.send(&resp.encode()).await.is_err() {
            return;
        }
        self.stop_linking(); // single-use
                             // Backfill: stream our account history so the new device starts populated,
                             // then an empty frame as terminator. Best-effort — failures just mean the
                             // joiner backfills nothing.
        for rec in self.export_account_backfill() {
            if channel.send(&rec.encode()).await.is_err() {
                return;
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
                    if DmEnvelope::decode(&e.plaintext).is_some() {
                        out.push(BackfillRecord {
                            conv: *conv.as_bytes(),
                            from: e.from.clone(),
                            wall_clock: e.wall_clock,
                            plaintext: e.plaintext.clone(),
                            event_id: *e.event_id.as_bytes(),
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
    pub(in crate::node) fn import_account_backfill(&self, records: &[BackfillRecord]) {
        let mut received = self.received.lock().expect("received mutex not poisoned");
        for r in records {
            let _ = received.record(
                ConversationId::new(r.conv),
                r.from.clone(),
                r.wall_clock,
                &r.plaintext,
                EventId::new(r.event_id),
            );
        }
    }
}

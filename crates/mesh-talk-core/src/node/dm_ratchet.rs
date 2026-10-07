//! The DM ratchet service: per-peer session establishment + encrypt/decrypt with
//! durable session persistence. Bootstrap is deterministic — the shared root secret
//! is the identity-key DH; the first sender is the initiator (`init_alice`), the
//! receiver bootstraps (`init_bob`); simultaneous init is resolved by a user-id
//! tie-break. The wire payload frames the ratchet header + ciphertext.

use crate::eventlog::LogError;
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use crate::node::ratchet_sessions::RatchetSessions;
use crate::ratchet::{init_alice, init_bob, Header};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

/// Derive the shared root secret from the two identities' X25519 keys (symmetric).
fn shared_root(me: &DeviceIdentity, peer: &PublicIdentity) -> [u8; 32] {
    let my_secret = StaticSecret::from(me.secret_bytes().1); // x25519 secret (mirror dm.rs)
    let dh = my_secret
        .diffie_hellman(&PublicKey::from(peer.x25519_pub))
        .to_bytes();
    let hk = Hkdf::<Sha256>::new(None, &dh);
    let mut out = [0u8; 32];
    hk.expand(b"MeshTalk-DM-Root", &mut out).expect("32 valid");
    out
}

/// Frame a header + ciphertext into one wire blob: `u16-BE header_len ‖ header ‖ ct`.
fn frame(header: &Header, ct: &[u8]) -> Vec<u8> {
    let h = header.encode();
    let mut out = Vec::with_capacity(2 + h.len() + ct.len());
    out.extend_from_slice(&(h.len() as u16).to_be_bytes());
    out.extend_from_slice(&h);
    out.extend_from_slice(ct);
    out
}

fn unframe(wire: &[u8]) -> Option<(Header, &[u8])> {
    if wire.len() < 2 {
        return None;
    }
    let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
    if wire.len() < 2 + hlen {
        return None;
    }
    let header = Header::decode(&wire[2..2 + hlen])?;
    Some((header, &wire[2 + hlen..]))
}

/// Owns the durable session store + this node's identity; manages per-peer ratchets.
pub struct DmRatchet {
    sessions: RatchetSessions,
}

/// Secret transition intended only for encrypted local transaction journals.
#[derive(Serialize, Deserialize)]
pub(crate) struct PreparedRatchet {
    pub(crate) peer: String,
    pub(crate) state: Vec<u8>,
    previous: Option<Vec<u8>>,
}

impl DmRatchet {
    pub fn new(sessions: RatchetSessions) -> Self {
        DmRatchet { sessions }
    }

    /// Encrypt `plaintext` to `peer`. Establishes the session (as initiator) on first
    /// use. Returns the wire blob (header + ciphertext). Persists the advanced session.
    pub fn encrypt(
        &mut self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        plaintext: &[u8],
    ) -> Result<Vec<u8>, LogError> {
        let (wire, prepared) = self.prepare_encrypt(me, peer, plaintext)?;
        self.commit_prepared(&prepared)?;
        Ok(wire)
    }

    pub(crate) fn prepare_encrypt(
        &self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        plaintext: &[u8],
    ) -> Result<(Vec<u8>, PreparedRatchet), LogError> {
        let peer_id = peer.user_id();
        let mut state = match self.sessions.get(&peer_id) {
            Some(s) => s,
            None => init_alice(&shared_root(me, peer), &peer.x25519_pub),
        };
        let (header, ct) = state
            .ratchet_encrypt(plaintext)
            .map_err(|_| LogError::Serialization("ratchet encrypt".into()))?;
        Ok((frame(&header, &ct), self.prepared(peer_id, &state)))
    }

    /// Decrypt a wire blob from `peer`. Bootstraps as responder on first contact;
    /// resolves a simultaneous-init race by a user-id tie-break. Persists the session.
    pub fn decrypt(
        &mut self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        wire: &[u8],
    ) -> Result<Vec<u8>, LogError> {
        let (plain, prepared) = self.prepare_decrypt(me, peer, wire)?;
        self.commit_prepared(&prepared)?;
        Ok(plain)
    }

    pub(crate) fn prepare_decrypt(
        &self,
        me: &DeviceIdentity,
        peer: &PublicIdentity,
        wire: &[u8],
    ) -> Result<(Vec<u8>, PreparedRatchet), LogError> {
        let (header, ct) =
            unframe(wire).ok_or_else(|| LogError::Serialization("bad wire".into()))?;
        let peer_id = peer.user_id();
        let my_secret = me.secret_bytes().1; // x25519 secret bytes

        let mut state = match self.sessions.get(&peer_id) {
            Some(s) => s,
            None => init_bob(&shared_root(me, peer), my_secret),
        };
        match state.ratchet_decrypt(&header, ct) {
            Ok(pt) => Ok((pt, self.prepared(peer_id, &state))),
            Err(_) => {
                // Possible simultaneous init: we set up as initiator, but the peer's
                // message expects us to be the responder. Tie-break: the LOWER user-id
                // is the canonical initiator. If the PEER is canonical-initiator, drop
                // our session, re-bootstrap as responder, and retry once.
                if peer_id < me.public().user_id() {
                    let mut fresh = init_bob(&shared_root(me, peer), my_secret);
                    let pt = fresh
                        .ratchet_decrypt(&header, ct)
                        .map_err(|_| LogError::Serialization("ratchet decrypt".into()))?;
                    Ok((pt, self.prepared(peer_id, &fresh)))
                } else {
                    Err(LogError::Serialization("ratchet decrypt".into()))
                }
            }
        }
    }

    fn prepared(&self, peer: String, state: &crate::ratchet::RatchetState) -> PreparedRatchet {
        let previous = self.sessions.get(&peer).map(|s| s.serialize());
        PreparedRatchet {
            peer,
            state: state.serialize(),
            previous,
        }
    }

    pub(crate) fn commit_prepared(&mut self, prepared: &PreparedRatchet) -> Result<(), LogError> {
        let state = crate::ratchet::RatchetState::deserialize(&prepared.state)
            .ok_or_else(|| LogError::Serialization("invalid prepared ratchet".into()))?;
        let current = self.sessions.get(&prepared.peer).map(|s| s.serialize());
        if current.as_deref() == Some(prepared.state.as_slice()) {
            return self.sessions.sync();
        }
        if current != prepared.previous {
            return Err(LogError::Serialization("stale prepared ratchet".into()));
        }
        self.sessions.put_durable(&prepared.peer, &state)
    }

    pub fn has_session(&self, peer: &PublicIdentity) -> bool {
        self.sessions.has(&peer.user_id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn manager(dir: &std::path::Path, name: &str) -> DmRatchet {
        DmRatchet::new(RatchetSessions::open(&dir.join(name), "pw").unwrap())
    }

    #[test]
    fn two_parties_exchange_and_persist() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");

        let w1 = am.encrypt(&alice, &bob.public(), b"hi bob").unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w1).unwrap(), b"hi bob");
        let w2 = bm.encrypt(&bob, &alice.public(), b"hi alice").unwrap();
        assert_eq!(am.decrypt(&alice, &bob.public(), &w2).unwrap(), b"hi alice");

        // Reload both managers from disk; the session continues.
        let mut am2 = manager(dir.path(), "a.sess");
        let w3 = am2.encrypt(&alice, &bob.public(), b"still works").unwrap();
        assert_eq!(
            bm.decrypt(&bob, &alice.public(), &w3).unwrap(),
            b"still works"
        );
    }

    #[test]
    fn prepared_transitions_leave_sessions_unchanged_until_commit() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let before = std::fs::read(dir.path().join("a.sess")).unwrap();
        let (wire, prepared) = am
            .prepare_encrypt(&alice, &bob.public(), b"prepared")
            .unwrap();
        assert!(!am.has_session(&bob.public()));
        assert_eq!(std::fs::read(dir.path().join("a.sess")).unwrap(), before);
        let (plain, received) = bm.prepare_decrypt(&bob, &alice.public(), &wire).unwrap();
        assert_eq!(plain, b"prepared");
        assert!(!bm.has_session(&alice.public()));
        assert!(!manager(dir.path(), "a.sess").has_session(&bob.public()));
        assert!(!manager(dir.path(), "b.sess").has_session(&alice.public()));
        am.commit_prepared(&prepared).unwrap();
        bm.commit_prepared(&received).unwrap();
        am.commit_prepared(&prepared).unwrap();
        let mut am = manager(dir.path(), "a.sess");
        am.commit_prepared(&prepared).unwrap();
        let reply = bm.encrypt(&bob, &alice.public(), b"reply").unwrap();
        assert_eq!(am.decrypt(&alice, &bob.public(), &reply).unwrap(), b"reply");
        assert!(am.commit_prepared(&prepared).is_err());
        let before_disk = std::fs::read(dir.path().join("a.sess")).unwrap();
        let before_live = am
            .sessions
            .get(&bob.public().user_id())
            .unwrap()
            .serialize();
        am.prepare_encrypt(&alice, &bob.public(), b"uncommitted")
            .unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("a.sess")).unwrap(),
            before_disk
        );
        assert_eq!(
            am.sessions
                .get(&bob.public().user_id())
                .unwrap()
                .serialize(),
            before_live
        );
        let (_, mut bad) = am.prepare_encrypt(&alice, &bob.public(), b"bad").unwrap();
        let before = am
            .sessions
            .get(&bob.public().user_id())
            .unwrap()
            .serialize();
        bad.state = vec![255; 3];
        assert!(am.commit_prepared(&bad).is_err());
        assert_eq!(
            am.sessions
                .get(&bob.public().user_id())
                .unwrap()
                .serialize(),
            before
        );
    }

    #[test]
    fn out_of_order_delivery_opens() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let w0 = am.encrypt(&alice, &bob.public(), b"m0").unwrap();
        let w1 = am.encrypt(&alice, &bob.public(), b"m1").unwrap();
        let w2 = am.encrypt(&alice, &bob.public(), b"m2").unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w2).unwrap(), b"m2");
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w0).unwrap(), b"m0");
        assert_eq!(bm.decrypt(&bob, &alice.public(), &w1).unwrap(), b"m1");
    }

    #[test]
    fn prepared_receive_replay_preserves_buffered_skipped_keys() {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let first = am.encrypt(&alice, &bob.public(), b"first").unwrap();
        let later = am.encrypt(&alice, &bob.public(), b"later").unwrap();
        let (plain, prepared) = bm.prepare_decrypt(&bob, &alice.public(), &later).unwrap();
        assert_eq!(plain, b"later");
        bm.commit_prepared(&prepared).unwrap();
        drop(bm);
        let mut bm = manager(dir.path(), "b.sess");
        bm.commit_prepared(&prepared).unwrap();
        assert_eq!(bm.decrypt(&bob, &alice.public(), &first).unwrap(), b"first");
    }

    #[test]
    fn simultaneous_init_is_resolved_by_tie_break() {
        // Both sides send first (each becomes initiator), then each receives the
        // other's first message. The tie-break re-bootstraps the higher-id side as
        // responder so at least the canonical-initiator's messages open.
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let mut am = manager(dir.path(), "a.sess");
        let mut bm = manager(dir.path(), "b.sess");
        let wa = am.encrypt(&alice, &bob.public(), b"from-a").unwrap();
        let wb = bm.encrypt(&bob, &alice.public(), b"from-b").unwrap();
        // Identify the canonical initiator (lower user-id) and assert its message opens
        // on the other side after the tie-break.
        if alice.public().user_id() < bob.public().user_id() {
            assert_eq!(bm.decrypt(&bob, &alice.public(), &wa).unwrap(), b"from-a");
        } else {
            assert_eq!(am.decrypt(&alice, &bob.public(), &wb).unwrap(), b"from-b");
        }
    }
}

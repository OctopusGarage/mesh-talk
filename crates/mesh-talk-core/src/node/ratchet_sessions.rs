//! A local-only, password-encrypted store mapping peer `user_id` to serialized
//! `RatchetState`. The on-disk framing is the shared [`EncryptedRecordLog`]
//! (magic + salt header, then length-prefixed AES-256-GCM records). Replay is
//! last-write-wins per peer, so the in-memory index always holds the latest
//! session per peer.

use crate::eventlog::LogError;
use crate::ratchet::RatchetState;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTRTCH";

/// One peer's session record (the `state` bytes are `RatchetState::serialize()` output).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionRecord {
    peer: String,
    state: Vec<u8>,
}

/// An append-only, password-encrypted store of ratchet sessions, one per peer.
/// Last write wins: replaying the file rebuilds the latest session for every peer.
pub struct RatchetSessions {
    file: EncryptedRecordLog<SessionRecord>,
    latest: HashMap<String, Vec<u8>>,
    /// Records physically on disk (appended, pre-compaction). Used to decide when
    /// to compact: the file is append-only (one record per encrypt/decrypt), so
    /// without compaction it grows per-message forever and `open` decrypts EVERY
    /// record at login (O(total messages) startup).
    on_disk: usize,
}

impl RatchetSessions {
    /// Open (or create) the session store at `path`.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<SessionRecord>::open(path, password, MAGIC)?;
        let on_disk = records.len();
        let mut latest: HashMap<String, Vec<u8>> = HashMap::new();
        for record in records {
            latest.insert(record.peer, record.state);
        }
        Ok(Self {
            file,
            latest,
            on_disk,
        })
    }

    /// The latest stored session for `peer`, deserialized, if any.
    pub fn get(&self, peer: &str) -> Option<RatchetState> {
        RatchetState::deserialize(self.latest.get(peer)?)
    }

    /// Persist `state` for `peer` (append a record; update the index). Periodically
    /// compacts the file down to one record per peer so it does not grow per-message
    /// without bound (which would make `open` O(total messages)).
    pub fn put(&mut self, peer: &str, state: &RatchetState) -> Result<(), LogError> {
        let state_bytes = state.serialize();
        let record = SessionRecord {
            peer: peer.to_string(),
            state: state_bytes.clone(),
        };
        self.file.append(&record)?;
        self.on_disk += 1;
        self.latest.insert(peer.to_string(), state_bytes);
        self.maybe_compact()?;
        Ok(())
    }

    /// Compact to exactly one record per peer when the on-disk append count has
    /// drifted well past the live peer count (slack factor 4× + 16). The slack
    /// keeps compaction infrequent (amortized O(1) per put) while bounding the file
    /// to O(peers) rather than O(messages).
    fn maybe_compact(&mut self) -> Result<(), LogError> {
        if self.on_disk <= self.latest.len() * 4 + 16 {
            return Ok(());
        }
        let records: Vec<SessionRecord> = self
            .latest
            .iter()
            .map(|(peer, state)| SessionRecord {
                peer: peer.clone(),
                state: state.clone(),
            })
            .collect();
        self.file.rewrite(&records)?;
        self.on_disk = records.len();
        Ok(())
    }

    /// Returns `true` if there is a stored session for `peer` that actually decodes.
    /// Goes through `get` (not just key-presence) so `has`/`get` can't disagree: a blob
    /// that authenticates but fails to deserialize must not report `has == true` while
    /// `get == None` (which would silently re-bootstrap the session and break the peer's
    /// decryption).
    pub fn has(&self, peer: &str) -> bool {
        self.get(peer).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ratchet::{init_alice, init_bob};
    use x25519_dalek::{PublicKey, StaticSecret};

    fn make_pair() -> (RatchetState, RatchetState) {
        let shared = [7u8; 32];
        let bob_secret = [5u8; 32];
        let bob_pub = PublicKey::from(&StaticSecret::from(bob_secret)).to_bytes();
        let alice = init_alice(&shared, &bob_pub);
        let bob = init_bob(&shared, bob_secret);
        (alice, bob)
    }

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let mut store = RatchetSessions::open(&path, "pw").unwrap();
        let (alice, _bob) = make_pair();
        assert!(!store.has("peer1"));
        store.put("peer1", &alice).unwrap();
        assert!(store.has("peer1"));
        // get returns a deserialized copy; verify it can encrypt (has a sending chain)
        let mut restored = store.get("peer1").unwrap();
        assert!(restored.ratchet_encrypt(b"test").is_ok());
    }

    #[test]
    fn reopen_restores_latest_put_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let (mut alice, mut bob) = make_pair();
        {
            let mut store = RatchetSessions::open(&path, "pw").unwrap();
            store.put("peer1", &alice).unwrap();
            // advance the session and put again — the second write should win
            let (h, ct) = alice.ratchet_encrypt(b"msg1").unwrap();
            bob.ratchet_decrypt(&h, &ct).unwrap();
            store.put("peer1", &alice).unwrap();
        }
        // Reopen: should get the latest (advanced) session, not the first
        let store2 = RatchetSessions::open(&path, "pw").unwrap();
        assert!(store2.has("peer1"));
        let mut restored = store2.get("peer1").unwrap();
        // The restored alice should be able to send msg2 (ns == 1, not 0)
        let (h2, ct2) = restored.ratchet_encrypt(b"msg2").unwrap();
        // Bob (not reopened) should decrypt it — this validates the session is the
        // advanced one (n=1, not n=0 which bob already consumed)
        assert_eq!(bob.ratchet_decrypt(&h2, &ct2).unwrap(), b"msg2");
    }

    #[test]
    fn repeated_puts_compact_toward_distinct_peer_count() {
        // Regression (disk leak): without compaction the file grows one record per
        // put forever and `open` decrypts EVERY record at login (O(total messages)).
        // Many puts for the same peer must compact the on-disk record count back
        // toward the number of distinct peers, while reopen still yields the latest
        // per peer.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        let (mut alice, mut bob) = make_pair();
        {
            let mut store = RatchetSessions::open(&path, "pw").unwrap();
            // Bootstrap a second peer so "distinct peers" == 2.
            let (alice2, _b2) = make_pair();
            store.put("peer2", &alice2).unwrap();
            // Hammer peer1 with many advancing puts.
            for _ in 0..200 {
                let (h, ct) = alice.ratchet_encrypt(b"m").unwrap();
                bob.ratchet_decrypt(&h, &ct).unwrap();
                store.put("peer1", &alice).unwrap();
            }
            // on_disk must have compacted, not grown to ~201.
            assert!(
                store.on_disk <= store.latest.len() * 4 + 16,
                "on_disk {} should be compacted within slack of {} peers",
                store.on_disk,
                store.latest.len()
            );
        }
        // Reopen: the physical file holds far fewer records than the 201 appends.
        let store2 = RatchetSessions::open(&path, "pw").unwrap();
        assert!(
            store2.on_disk <= 2 * 4 + 16,
            "reopened file should be compact, got {} records",
            store2.on_disk
        );
        // And the latest session per peer survives compaction.
        assert!(store2.has("peer1"));
        assert!(store2.has("peer2"));
        let mut restored = store2.get("peer1").unwrap();
        let (h, ct) = restored.ratchet_encrypt(b"after").unwrap();
        assert_eq!(bob.ratchet_decrypt(&h, &ct).unwrap(), b"after");
    }

    #[test]
    fn has_and_get_agree_when_a_stored_blob_fails_to_deserialize() {
        // Documented invariant: `has` routes through `get`, so a record that
        // AEAD-authenticates but whose `state` bytes don't decode as a RatchetState
        // must report `has == false` (matching `get == None`) — never `has == true`
        // while `get == None`, which would let the node believe it has a session and
        // silently re-bootstrap, breaking the peer's decryption.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.db");
        {
            let mut store = RatchetSessions::open(&path, "pw").unwrap();
            // Append a record with authentic framing but undecodable state bytes,
            // straight through the underlying log (bypassing `put`'s serialize).
            store
                .file
                .append(&SessionRecord {
                    peer: "peer1".into(),
                    state: vec![0xFF; 3], // not a valid RatchetState wire form
                })
                .unwrap();
        }
        // Reopen so the bogus blob is the indexed `latest` for the peer.
        let store = RatchetSessions::open(&path, "pw").unwrap();
        assert!(
            store.get("peer1").is_none(),
            "garbage state does not decode"
        );
        assert!(
            !store.has("peer1"),
            "has must agree with get — never claim a session that won't decode"
        );
    }
}

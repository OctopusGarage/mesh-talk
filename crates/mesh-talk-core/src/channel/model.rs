//! Channel membership + per-sender ratchet state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the sender-key ratchet
//! state it holds (its own per-epoch sending key + per-`(author,epoch)` receiving
//! chains), used to seal/open channel messages with single-use forward-secret keys.

use crate::channel::crypto::ChannelError;
use crate::channel::sender_key::{
    open_message as sk_open, seal_message as sk_seal, SenderChain, SenderKey, SenderKeyDistribution,
};
use crate::eventlog::event::ConversationId;
use crate::identity::device::PublicIdentity;
use bincode::Options;
use rand_core::OsRng;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// The plaintext membership snapshot carried by a `MembershipChange` event: the full
/// member set + channel name + epoch. A snapshot (not a delta) so the latest one
/// fully defines current membership on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub members: Vec<PublicIdentity>,
    pub epoch: u64,
    /// The channel owner's device `user_id` (the creator). The owner is the ONLY
    /// principal allowed to change membership: every node accepts a `MembershipChange`
    /// only when its event author equals this `owner` (see `ChannelBook::process`), so a
    /// non-owner cannot add/remove members on any node. Fixed at genesis — a later
    /// snapshot that names a different owner is rejected (no ownership hijack).
    pub owner: String,
}

impl ChannelMeta {
    /// Serialize for an event's payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("channel meta serializes")
    }

    /// Parse a membership snapshot, fail-closed (reject trailing bytes).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }

    /// Whether `user_id` (an Ed25519 fingerprint) is a current member.
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }
}

/// A fresh random channel id — a `ConversationId` minted at creation, distinct from
/// the derived DM conversation ids.
pub fn new_channel_id() -> ConversationId {
    let mut id = [0u8; 32];
    OsRng.fill_bytes(&mut id);
    ConversationId::new(id)
}

/// The per-message header for the sender-key path: epoch, the author's user id, and
/// the message's position `n` in that author's chain. Encoded as the AAD bound into
/// each sealed channel message (epoch ‖ sender ‖ n).
#[derive(Serialize, Deserialize)]
struct MsgHeader {
    epoch: u64,
    sender: String,
    n: u32,
}

impl MsgHeader {
    fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("hdr")
    }
    fn decode(b: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(b)
            .ok()
    }
}

/// A member's view of a channel: its id, current membership/name/epoch, and the
/// sender-key ratchet state this node holds (its own per-epoch sending key plus the
/// receiving chains it follows for each `(author, epoch)`).
pub struct ChannelState {
    id: ConversationId,
    name: String,
    members: Vec<PublicIdentity>,
    epoch: u64,
    owner: String,
    my_user_id: Option<String>,
    my_sender: HashMap<u64, SenderKey>,
    sender_chains: HashMap<(String, u64), SenderChain>,
}

impl ChannelState {
    /// Build state from a channel id and its current membership snapshot (no keys yet).
    pub fn from_meta(id: ConversationId, meta: ChannelMeta) -> Self {
        ChannelState {
            id,
            name: meta.name,
            members: meta.members,
            epoch: meta.epoch,
            owner: meta.owner,
            my_user_id: None,
            my_sender: HashMap::new(),
            sender_chains: HashMap::new(),
        }
    }

    /// Adopt a (possibly newer) membership snapshot: if it advances the epoch, take
    /// its name/members/epoch. Older or equal snapshots are ignored — idempotent
    /// replay regardless of event arrival order.
    ///
    /// Owner authorization is enforced at the call site in `ChannelBook::process`
    /// (the event author must equal the owner). This snapshot's `owner` is additionally
    /// pinned to the genesis owner: a snapshot naming a DIFFERENT owner is rejected here
    /// outright, so even an owner-authored event cannot reassign ownership and a higher
    /// epoch can never hijack the channel.
    pub fn apply_meta(&mut self, meta: ChannelMeta) {
        use std::cmp::Ordering;
        // Owner is immutable: never adopt a snapshot that renames the owner.
        if meta.owner != self.owner {
            return;
        }
        let take = match meta.epoch.cmp(&self.epoch) {
            Ordering::Greater => true,
            // Two concurrent membership changes can mint the SAME epoch with different
            // member sets. Without a tie-break the result would depend on arrival order
            // (nodes diverge). Converge deterministically: keep the canonically-greater
            // snapshot (by encoded bytes), so every node lands on the same membership.
            Ordering::Equal => {
                let current = ChannelMeta {
                    name: self.name.clone(),
                    members: self.members.clone(),
                    epoch: self.epoch,
                    owner: self.owner.clone(),
                };
                meta.encode() > current.encode()
            }
            Ordering::Less => false,
        };
        if take {
            self.name = meta.name;
            self.members = meta.members;
            self.epoch = meta.epoch;
        }
    }

    pub fn id(&self) -> ConversationId {
        self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// The channel owner's device `user_id` — the only principal allowed to change membership.
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn members(&self) -> &[PublicIdentity] {
        &self.members
    }
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }

    /// Set this node's identity (who "I" am) for sender-key send/AAD. Idempotent.
    pub fn set_identity(&mut self, my_user_id: String) {
        self.my_user_id = Some(my_user_id);
    }

    /// My current-epoch sender-key distribution (generating my sender key if needed).
    /// Distribute this (sealed per-member) so members can follow my chain from n=0.
    pub fn my_sender_distribution(&mut self) -> SenderKeyDistribution {
        let epoch = self.epoch;
        self.my_sender
            .entry(epoch)
            .or_insert_with(SenderKey::generate)
            .distribution()
    }

    /// Whether this node has already generated (and thus should have distributed) its
    /// sender key for `epoch`.
    pub fn has_my_sender(&self, epoch: u64) -> bool {
        self.my_sender.contains_key(&epoch)
    }

    /// Snapshot this node's per-epoch sending keys for persistence: each entry is
    /// `(epoch, serialized SenderKey)`. The bytes are SECRET — store encrypted at rest.
    pub fn export_my_senders(&self) -> Vec<(u64, Vec<u8>)> {
        self.my_sender
            .iter()
            .map(|(epoch, sk)| (*epoch, sk.serialize()))
            .collect()
    }

    /// Restore a persisted sending key for `epoch` (overwrites any in-memory key for
    /// that epoch). Used at open to resume our sending chains after a restart.
    pub fn import_my_sender(&mut self, epoch: u64, sk: SenderKey) {
        self.my_sender.insert(epoch, sk);
    }

    /// Record a peer's sender chain for `(author, epoch)` from their distribution.
    pub fn record_sender_chain(&mut self, author: String, epoch: u64, skd: &SenderKeyDistribution) {
        self.sender_chains
            .entry((author, epoch))
            .or_insert_with(|| SenderChain::from_distribution(skd));
    }

    /// Seal `plaintext` with my current-epoch sender key. Wire = `u16 hdr_len ‖ hdr ‖ ct`,
    /// AAD = hdr (epoch‖sender‖n). Errors if my identity isn't set.
    pub fn seal_sender_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let me = self
            .my_user_id
            .clone()
            .ok_or_else(|| ChannelError::Malformed("no identity".into()))?;
        let epoch = self.epoch;
        let sk = self
            .my_sender
            .entry(epoch)
            .or_insert_with(SenderKey::generate);
        let (n, mk) = sk.ratchet();
        let hdr = MsgHeader {
            epoch,
            sender: me,
            n,
        }
        .encode();
        let ct = sk_seal(&mk, plaintext, &hdr).map_err(|_| ChannelError::Encrypt)?;
        let mut out = Vec::with_capacity(2 + hdr.len() + ct.len());
        out.extend_from_slice(&(hdr.len() as u16).to_be_bytes());
        out.extend_from_slice(&hdr);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Open a sender-keyed message (`&mut` — the receiving chain ratchets + deletes the
    /// key). `expected_sender` is the **authenticated** event author's user-id: the
    /// header's claimed `sender` must equal it, so a member holding another member's
    /// sender chain cannot seal under it (impersonation) nor burn its single-use
    /// positions (censorship/DoS). The bind is checked BEFORE the chain is consumed.
    /// `None` if the sender is forged, we lack that sender's chain for the epoch, or the
    /// key is consumed.
    pub fn open_sender_message(&mut self, expected_sender: &str, wire: &[u8]) -> Option<Vec<u8>> {
        if wire.len() < 2 {
            return None;
        }
        let hlen = u16::from_be_bytes([wire[0], wire[1]]) as usize;
        if wire.len() < 2 + hlen {
            return None;
        }
        let hdr_bytes = &wire[2..2 + hlen];
        let ct = &wire[2 + hlen..];
        let hdr = MsgHeader::decode(hdr_bytes)?;
        // Bind the decryption chain to the authenticated author — the wire-claimed
        // `sender` is not trusted on its own.
        if hdr.sender != expected_sender {
            return None;
        }
        let chain = self
            .sender_chains
            .get_mut(&(hdr.sender.clone(), hdr.epoch))?;
        let mk = chain.message_key(hdr.n).ok()?;
        sk_open(&mk, ct, hdr_bytes).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    fn meta(name: &str, members: &[&DeviceIdentity], epoch: u64) -> ChannelMeta {
        // The first member is treated as the owner for these state-level tests.
        let owner = members
            .first()
            .map(|d| d.public().user_id())
            .unwrap_or_default();
        ChannelMeta {
            name: name.to_string(),
            members: members.iter().map(|d| d.public()).collect(),
            epoch,
            owner,
        }
    }

    #[test]
    fn channel_meta_round_trips_and_rejects_trailing() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let m = meta("general", &[&a, &b], 0);
        let bytes = m.encode();
        assert_eq!(ChannelMeta::decode(&bytes), Some(m.clone()));
        let mut junk = bytes.clone();
        junk.push(0xAB);
        assert_eq!(ChannelMeta::decode(&junk), None);
        assert!(m.is_member(&a.public().user_id()));
        assert!(!m.is_member("nobody"));
    }

    #[test]
    fn channel_ids_are_random() {
        assert_ne!(new_channel_id(), new_channel_id());
    }

    #[test]
    fn apply_meta_adopts_newer_epoch_only() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut state = ChannelState::from_meta(id, meta("general", &[&a], 0));
        assert_eq!(state.members().len(), 1);

        state.apply_meta(meta("general", &[&a, &b], 1));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
        assert!(state.is_member(&b.public().user_id()));

        state.apply_meta(meta("general", &[&a], 0));
        assert_eq!(state.epoch(), 1);
        assert_eq!(state.members().len(), 2);
    }

    #[test]
    fn concurrent_same_epoch_membership_converges() {
        // Two membership changes minted at the SAME epoch (concurrent edits) must
        // converge to identical membership on every node regardless of arrival order —
        // otherwise nodes disagree on who is in the channel (and who gets re-keyed).
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let c = DeviceIdentity::generate();
        let id = new_channel_id();
        let add_c = meta("general", &[&a, &b, &c], 1);
        let remove_b = meta("general", &[&a], 1);

        let mut s1 = ChannelState::from_meta(id, meta("general", &[&a, &b], 0));
        s1.apply_meta(add_c.clone());
        s1.apply_meta(remove_b.clone());

        let mut s2 = ChannelState::from_meta(id, meta("general", &[&a, &b], 0));
        s2.apply_meta(remove_b.clone());
        s2.apply_meta(add_c.clone());

        assert_eq!(s1.epoch(), s2.epoch());
        let m1: Vec<String> = s1.members().iter().map(|p| p.user_id()).collect();
        let m2: Vec<String> = s2.members().iter().map(|p| p.user_id()).collect();
        assert_eq!(m1, m2, "concurrent same-epoch membership must converge");
    }

    #[test]
    fn apply_meta_rejects_owner_change_even_at_higher_epoch() {
        // The owner is pinned at genesis. A snapshot naming a DIFFERENT owner — even at a
        // higher epoch — must be ignored, so ownership cannot be hijacked.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut state = ChannelState::from_meta(id, meta("general", &[&a, &b], 0));
        assert_eq!(state.owner(), a.public().user_id());

        // b tries to seize ownership at a higher epoch.
        let hijack = ChannelMeta {
            name: "general".into(),
            members: vec![b.public()],
            epoch: 99,
            owner: b.public().user_id(),
        };
        state.apply_meta(hijack);
        assert_eq!(state.epoch(), 0, "owner-changing snapshot must be ignored");
        assert_eq!(state.owner(), a.public().user_id());
        assert!(state.is_member(&a.public().user_id()));
    }

    // --- sender-key path ---

    fn sender_pair(epoch: u64) -> (ChannelState, ChannelState) {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        alice.set_identity("alice".into());
        let mut bob = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        bob.set_identity("bob".into());
        // Bob follows Alice's chain from her distribution.
        let skd = alice.my_sender_distribution();
        bob.record_sender_chain("alice".into(), epoch, &skd);
        (alice, bob)
    }

    #[test]
    fn sender_key_seal_then_open() {
        let (mut alice, mut bob) = sender_pair(0);
        let wire = alice.seal_sender_message(b"hi").unwrap();
        assert_eq!(
            bob.open_sender_message("alice", &wire).as_deref(),
            Some(&b"hi"[..])
        );
    }

    #[test]
    fn sender_key_out_of_order() {
        let (mut alice, mut bob) = sender_pair(0);
        let w0 = alice.seal_sender_message(b"m0").unwrap();
        let w1 = alice.seal_sender_message(b"m1").unwrap();
        let w2 = alice.seal_sender_message(b"m2").unwrap();
        // Bob opens 2, 0, 1 — the chain ratchets forward and buffers skipped keys.
        assert_eq!(
            bob.open_sender_message("alice", &w2).as_deref(),
            Some(&b"m2"[..])
        );
        assert_eq!(
            bob.open_sender_message("alice", &w0).as_deref(),
            Some(&b"m0"[..])
        );
        assert_eq!(
            bob.open_sender_message("alice", &w1).as_deref(),
            Some(&b"m1"[..])
        );
    }

    #[test]
    fn sender_key_is_single_use() {
        let (mut alice, mut bob) = sender_pair(0);
        let wire = alice.seal_sender_message(b"once").unwrap();
        assert_eq!(
            bob.open_sender_message("alice", &wire).as_deref(),
            Some(&b"once"[..])
        );
        // The key was consumed (forward secrecy) — re-opening the same wire fails.
        assert!(bob.open_sender_message("alice", &wire).is_none());
    }

    #[test]
    fn open_rejects_sender_mismatch_without_consuming_chain() {
        // A member who holds Alice's chain could otherwise seal a message "as Alice"
        // (or replay one) under an event they author, burning Alice's single-use
        // position so her genuine message is dropped (censorship/DoS). The author bind
        // must reject it BEFORE the chain is consumed.
        let (mut alice, mut bob) = sender_pair(0);
        let wire = alice.seal_sender_message(b"hi").unwrap();
        // Same wire, but the authenticated author is someone else: rejected.
        assert!(bob.open_sender_message("mallory", &wire).is_none());
        // Crucially, Alice's position was NOT consumed — her genuine message still opens.
        assert_eq!(
            bob.open_sender_message("alice", &wire).as_deref(),
            Some(&b"hi"[..]),
        );
    }

    #[test]
    fn sender_key_open_without_chain_is_none() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.set_identity("alice".into());
        let wire = alice.seal_sender_message(b"secret").unwrap();
        // A member who never recorded Alice's chain cannot open her message.
        let mut outsider = ChannelState::from_meta(id, meta("general", &[&a], 0));
        outsider.set_identity("carol".into());
        assert!(outsider.open_sender_message("alice", &wire).is_none());
    }

    #[test]
    fn seal_sender_message_fails_without_identity() {
        let a = DeviceIdentity::generate();
        let mut state = ChannelState::from_meta(new_channel_id(), meta("general", &[&a], 0));
        assert!(state.seal_sender_message(b"x").is_err());
    }
}

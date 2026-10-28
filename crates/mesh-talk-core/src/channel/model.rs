//! Channel membership + per-sender ratchet state. `ChannelMeta` is the plaintext
//! membership snapshot carried by a `MembershipChange` event (the post office may
//! read it — membership is accepted metadata; the event is still author-signed).
//! `ChannelState` is a member's view: current membership + the sender-key ratchet
//! state it holds (its own per-epoch sending key + per-`(author,epoch)` receiving
//! chains), used to seal/open channel messages with single-use forward-secret keys.

use crate::channel::crypto::ChannelError;
use crate::channel::sender_key::{
    SenderChain, SenderKey, SenderKeyDistribution, seal_message as sk_seal,
};
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::device::PublicIdentity;
use bincode::Options;
use rand_core::OsRng;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use zeroize::{Zeroize, Zeroizing};

/// The plaintext membership snapshot carried by a `MembershipChange` event: the full
/// member set + channel name + epoch. A snapshot (not a delta) so the latest one
/// fully defines current membership on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMeta {
    pub name: String,
    pub members: Vec<PublicIdentity>,
    pub epoch: u64,
    /// Current owner's device user id. A signed transfer changes authority; a
    /// membership snapshot alone cannot assign a new owner.
    pub owner: String,
}

/// New channel genesis marker. Legacy snapshots have no prefix and remain
/// readable; an invited channel requires an explicit local acceptance.
const INVITATION_GENESIS: &[u8] = b"MTCH2";

impl ChannelMeta {
    /// Serialize for an event's payload (fixint, matching the codebase wire style).
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("channel meta serializes")
    }

    pub fn encode_invitation(&self) -> Vec<u8> {
        [INVITATION_GENESIS, self.encode().as_slice()].concat()
    }

    pub fn is_invitation(bytes: &[u8]) -> bool {
        bytes.starts_with(INVITATION_GENESIS)
    }

    /// Parse a membership snapshot, fail-closed (reject trailing bytes).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.strip_prefix(INVITATION_GENESIS).unwrap_or(bytes);
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

/// Encode just the member set for the equal-epoch membership tie-break. Deterministic
/// across nodes because members are canonicalized (sorted + de-duped) at every
/// construction site, so the same SET always yields the same bytes.
fn encode_members(members: &[PublicIdentity]) -> Vec<u8> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(members)
        .expect("members serialize")
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
pub(crate) type PreparedSenderMessage =
    (Vec<u8>, Option<SenderKeyDistribution>, Vec<(u64, Vec<u8>)>);

pub struct ChannelState {
    id: ConversationId,
    name: String,
    /// The `(lamport, id)` of the rename event that set the current `name`, for
    /// last-writer-wins (mirrors the `Profile` LWW convention). The genesis name from the
    /// membership snapshot starts at the baseline `(0, [0; 32])`, so any real rename event
    /// (lamport ≥ 1) supersedes it.
    name_version: (u64, EventId),
    members: Vec<PublicIdentity>,
    epoch: u64,
    owner: String,
    account_owner: Option<String>,
    account_members: HashMap<String, String>,
    dissolved: bool,
    genesis_owner: String,
    pending_invite: bool,
    accepted_event_id: Option<EventId>,
    accepted_members: HashSet<String>,
    my_user_id: Option<String>,
    my_sender: HashMap<u64, SenderKey>,
    sender_chains: HashMap<(String, u64), SenderChain>,
}

impl ChannelState {
    /// Build state from a channel id and its current membership snapshot (no keys yet).
    pub fn from_meta(id: ConversationId, meta: ChannelMeta) -> Self {
        let accepted_members = meta.members.iter().map(PublicIdentity::user_id).collect();
        ChannelState {
            id,
            name: meta.name,
            name_version: (0, EventId::new([0u8; 32])),
            members: meta.members,
            epoch: meta.epoch,
            dissolved: false,
            owner: meta.owner.clone(),
            account_owner: None,
            account_members: HashMap::new(),
            genesis_owner: meta.owner,
            pending_invite: false,
            accepted_event_id: None,
            accepted_members,
            my_user_id: None,
            my_sender: HashMap::new(),
            sender_chains: HashMap::new(),
        }
    }

    /// Adopt a (possibly newer) membership snapshot: if it advances the epoch, take its
    /// members/epoch. Older or equal snapshots are ignored — idempotent replay regardless
    /// of event arrival order. The NAME is NOT taken from here: it is owned by independent
    /// `ChannelRename` events (see `apply_rename`), so a rename can never clobber — nor be
    /// clobbered by — a concurrent membership change at the same epoch.
    ///
    /// Owner authorization is enforced at the call site in `ChannelBook::process`
    /// (the event author must equal the owner). This snapshot's `owner` is additionally
    /// pinned to the current owner: a snapshot naming another owner is rejected.
    pub fn apply_meta(&mut self, meta: ChannelMeta) {
        self.apply_meta_with_departure(meta, false);
    }

    /// Merge an owner snapshot with a concurrent, independently signed leave.
    /// Its full member set still defines the owner's other additions/removals,
    /// while the departed member has already been filtered from `meta`.
    pub fn apply_meta_after_concurrent_leave(&mut self, meta: ChannelMeta) {
        self.apply_meta_with_departure(meta, true);
    }

    fn apply_meta_with_departure(&mut self, meta: ChannelMeta, concurrent_leave: bool) {
        use std::cmp::Ordering;
        if self.dissolved {
            return;
        }
        // A membership snapshot cannot perform an ownership transfer.
        if meta.owner != self.owner {
            return;
        }
        let take = match meta.epoch.cmp(&self.epoch) {
            Ordering::Greater => true,
            // Two concurrent membership changes can mint the SAME epoch with different
            // member sets. Without a tie-break the result would depend on arrival order
            // (nodes diverge). Converge deterministically on the canonically-greater MEMBER
            // SET (by encoded bytes) — name is excluded so it can't sway the membership
            // winner — so every node lands on the same membership.
            Ordering::Equal => {
                concurrent_leave || encode_members(&meta.members) > encode_members(&self.members)
            }
            Ordering::Less => false,
        };
        if take {
            self.members = meta.members;
            self.epoch = meta.epoch;
        }
    }

    /// Adopt a channel rename last-writer-wins by `(lamport, id)` — the same ordering the
    /// log uses for `Profile` updates. Independent of the membership epoch, so renaming
    /// neither rotates keys nor races a membership change. Owner authorization is enforced
    /// at the call site in `ChannelBook::process`.
    pub fn apply_rename(&mut self, name: &str, lamport: u64, id: EventId) {
        if self.dissolved {
            return;
        }
        let version = (lamport, id);
        if version > self.name_version {
            self.name = name.to_string();
            self.name_version = version;
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
    /// Legacy owner device identifier. Use `can_admin` for projected owner roles.
    pub fn owner(&self) -> &str {
        &self.owner
    }
    /// Account roles are installed only by the validated channel projection.
    pub fn owner_account_id(&self) -> Option<&str> {
        self.account_owner.as_deref()
    }
    pub(crate) fn member_account_id(&self, user_id: &str) -> Option<&str> {
        self.account_members.get(user_id).map(String::as_str)
    }
    pub fn can_admin(&self, user_id: &str) -> bool {
        if self.dissolved {
            return false;
        }
        if let Some(owner) = &self.account_owner {
            self.is_accepted_member(user_id) && self.account_members.get(user_id) == Some(owner)
        } else {
            self.owner == user_id
        }
    }
    pub(crate) fn install_account_projection(
        &mut self,
        owner: String,
        participants: Vec<(PublicIdentity, String)>,
        epoch: u64,
    ) {
        self.account_owner = Some(owner);
        self.account_members = participants
            .iter()
            .map(|(member, account)| (member.user_id(), account.clone()))
            .collect();
        self.accepted_members = self.account_members.keys().cloned().collect();
        self.members = participants.into_iter().map(|(member, _)| member).collect();
        self.members.sort_unstable_by_key(PublicIdentity::user_id);
        self.members.dedup();
        self.epoch = epoch;
        if let Some(me) = &self.my_user_id {
            self.pending_invite = self.is_member(me) && !self.is_accepted_member(me);
        }
    }
    pub(crate) fn install_account_invites(&mut self, participants: Vec<(PublicIdentity, String)>) {
        for (member, account) in participants {
            let uid = member.user_id();
            if !self.is_member(&uid) {
                self.members.push(member);
            }
            self.account_members.entry(uid).or_insert(account);
        }
        self.members.sort_unstable_by_key(PublicIdentity::user_id);
        if let Some(me) = &self.my_user_id {
            self.pending_invite = self.is_member(me) && !self.is_accepted_member(me);
        }
    }
    /// Copy projected authority without replacing local sending/receiving ratchets.
    pub(crate) fn install_membership_projection(&mut self, projected: Self) {
        self.name = projected.name;
        self.name_version = projected.name_version;
        self.members = projected.members;
        self.epoch = projected.epoch;
        self.owner = projected.owner;
        self.account_owner = projected.account_owner;
        self.account_members = projected.account_members;
        self.dissolved = projected.dissolved;
        self.accepted_members = projected.accepted_members;
        if let Some(me) = &self.my_user_id {
            self.pending_invite = self.is_member(me) && !self.is_accepted_member(me);
        }
    }
    pub fn is_dissolved(&self) -> bool {
        self.dissolved
    }
    pub fn dissolve(&mut self) {
        self.dissolved = true;
    }
    pub fn genesis_owner(&self) -> &str {
        &self.genesis_owner
    }
    pub fn set_genesis_owner(&mut self, owner: String) {
        self.genesis_owner = owner;
    }
    pub fn pending_invite(&self) -> bool {
        self.pending_invite
    }
    pub fn accepted_event_id(&self) -> Option<EventId> {
        self.accepted_event_id
    }
    pub fn mark_invited(&mut self, user_id: &str) {
        self.accepted_members.remove(user_id);
        if self.my_user_id.as_deref() == Some(user_id) {
            self.pending_invite = true;
            self.accepted_event_id = None;
        }
    }
    pub fn accept_invite(&mut self, user_id: &str, event_id: EventId) {
        if !self.is_member(user_id) {
            return;
        }
        self.accepted_members.insert(user_id.to_string());
        if self.my_user_id.as_deref() == Some(user_id) && self.pending_invite {
            self.pending_invite = false;
            self.accepted_event_id = Some(event_id);
        }
    }
    /// Historical, unmarked membership snapshots auto-joined newly added
    /// members. Preserve that behavior without introducing an acceptance cutoff.
    pub fn auto_accept_legacy_member(&mut self, user_id: &str) {
        if !self.is_member(user_id) {
            return;
        }
        self.accepted_members.insert(user_id.to_string());
        if self.my_user_id.as_deref() == Some(user_id) {
            self.pending_invite = false;
            self.accepted_event_id = None;
        }
    }
    /// Transfer administration only to a current member. The signed-event author is
    /// checked by the caller before this method is invoked.
    pub fn transfer_owner(&mut self, next: &str) -> bool {
        if !self.is_member(next) || !self.accepted_members.contains(next) {
            return false;
        }
        self.owner = next.to_string();
        true
    }
    /// Apply a self-signed departure. Epoch advances so later sends use fresh
    /// sender keys and concurrent stale owner snapshots cannot restore the member.
    pub fn leave_member(&mut self, user_id: &str) -> bool {
        if user_id == self.owner || !self.is_member(user_id) {
            return false;
        }
        self.members.retain(|member| member.user_id() != user_id);
        self.accepted_members.remove(user_id);
        self.epoch = self.epoch.saturating_add(1);
        true
    }
    pub fn members(&self) -> &[PublicIdentity] {
        &self.members
    }
    /// Content recipients, distinct from the roster's visible pending invitations.
    /// Legacy groups retain their original invitation/key-distribution contract.
    pub(crate) fn recipient_members(&self) -> Vec<PublicIdentity> {
        self.members
            .iter()
            .filter(|member| {
                self.account_owner.is_none() || self.is_accepted_member(&member.user_id())
            })
            .cloned()
            .collect()
    }
    pub fn is_member(&self, user_id: &str) -> bool {
        self.members.iter().any(|m| m.user_id() == user_id)
    }
    pub fn is_accepted_member(&self, user_id: &str) -> bool {
        self.is_member(user_id) && self.accepted_members.contains(user_id)
    }

    /// Set this node's identity (who "I" am) for sender-key send/AAD. Idempotent.
    pub fn set_identity(&mut self, my_user_id: String) {
        self.pending_invite = self.is_member(&my_user_id) && !self.is_accepted_member(&my_user_id);
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

    /// Current-epoch receive material for an authorized sibling. The caller owns
    /// account membership admission and encrypted transport. These distributions
    /// seed receive chains only; a new device must generate its own sending key.
    pub(crate) fn current_receive_distributions(
        &self,
    ) -> Vec<(String, u64, SenderKeyDistribution)> {
        let mut distributions: Vec<_> = self
            .sender_chains
            .iter()
            .filter(|((_, epoch), _)| *epoch == self.epoch)
            .map(|((author, epoch), chain)| (author.clone(), *epoch, chain.current_distribution()))
            .collect();
        if let Some(author) = &self.my_user_id
            && let Some(sender) = self.my_sender.get(&self.epoch)
        {
            distributions.retain(|(existing, _, _)| existing != author);
            distributions.push((author.clone(), self.epoch, sender.distribution()));
        }
        distributions.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        distributions
    }

    /// Record a peer's sender chain for `(author, epoch)` from their distribution.
    pub fn record_sender_chain(&mut self, author: String, epoch: u64, skd: &SenderKeyDistribution) {
        self.sender_chains
            .entry((author, epoch))
            .or_insert_with(|| SenderChain::from_distribution(skd));
    }

    /// Install receive-only material from a centrally admitted account sibling.
    /// Ordinary sender-key distributions remain first-wins; only this explicit
    /// recovery path may advance an existing receiver past an unavailable gap
    /// beyond the normal skip bound. Routine snapshots preserve pending delivery.
    pub(crate) fn refresh_sender_chain(
        &mut self,
        author: String,
        epoch: u64,
        skd: &SenderKeyDistribution,
    ) {
        if epoch != self.epoch {
            return;
        }
        self.sender_chains
            .entry((author, epoch))
            .and_modify(|chain| chain.refresh_distribution(skd))
            .or_insert_with(|| SenderChain::from_distribution(skd));
    }

    /// Seal `plaintext` with my current-epoch sender key. Wire = `u16 hdr_len ‖ hdr ‖ ct`,
    /// AAD = hdr (epoch‖sender‖n). Errors if my identity isn't set.
    pub fn seal_sender_message(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, ChannelError> {
        let me = self
            .my_user_id
            .clone()
            .ok_or_else(|| ChannelError::Malformed("no identity".into()))?;
        if !self.is_member(&me) || self.pending_invite {
            return Err(ChannelError::Malformed("not a channel member".into()));
        }
        let epoch = self.epoch;
        let sk = self
            .my_sender
            .entry(epoch)
            .or_insert_with(SenderKey::generate);
        let (n, mk) = sk
            .try_ratchet()
            .map_err(|_| ChannelError::Malformed("sender chain exhausted".into()))?;
        let mk = Zeroizing::new(mk);
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

    /// Prepare a send without changing the live sender chain. The host journals
    /// the returned snapshot atomically with the signed event before installing it.
    pub(crate) fn prepare_sender_message(
        &self,
        plaintext: &[u8],
    ) -> Result<PreparedSenderMessage, ChannelError> {
        let me = self
            .my_user_id
            .clone()
            .ok_or_else(|| ChannelError::Malformed("no identity".into()))?;
        let mut key = match self.my_sender.get(&self.epoch) {
            Some(key) => {
                let encoded = Zeroizing::new(key.serialize());
                SenderKey::deserialize(&encoded)
                    .ok_or_else(|| ChannelError::Malformed("invalid sender snapshot".into()))?
            }
            None => SenderKey::generate(),
        };
        let distribution =
            (!self.has_my_sender(self.epoch)).then(|| Zeroizing::new(key.distribution()));
        let (n, mk) = key
            .try_ratchet()
            .map_err(|_| ChannelError::Malformed("sender chain exhausted".into()))?;
        let mk = Zeroizing::new(mk);
        let header = MsgHeader {
            epoch: self.epoch,
            sender: me,
            n,
        }
        .encode();
        let ciphertext = sk_seal(&mk, plaintext, &header).map_err(|_| ChannelError::Encrypt)?;
        let mut payload = Vec::with_capacity(2 + header.len() + ciphertext.len());
        payload.extend_from_slice(&(header.len() as u16).to_be_bytes());
        payload.extend(header);
        payload.extend(ciphertext);
        let mut senders = self.export_my_senders();
        senders.retain_mut(|(epoch, encoded)| {
            if *epoch == self.epoch {
                encoded.zeroize();
                false
            } else {
                true
            }
        });
        senders.push((self.epoch, key.serialize()));
        Ok((payload, distribution.map(|value| (*value).clone()), senders))
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
        let prepared = chain.prepare_open(hdr.n, ct, hdr_bytes).ok()?;
        Some(prepared.install(chain))
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
    fn rename_and_membership_change_do_not_clobber_each_other() {
        // A rename and a membership change are orthogonal — name vs members — and must BOTH
        // survive when applied together, in either order. Before names were split onto their
        // own `(lamport, id)`-ordered events, both rode the membership epoch, so a same-epoch
        // tie-break silently dropped one (a rename could revert a member removal, or vice
        // versa). Now membership comes from `apply_meta`, the name from `apply_rename`.
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let c = DeviceIdentity::generate();
        let id = new_channel_id();
        let genesis = meta("general", &[&a, &b, &c], 0);
        // Remove c at epoch 1. Its `name` field is now inert (apply_meta ignores it).
        let remove_c = meta("name-is-ignored-here", &[&a, &b], 1);
        let rename_id = EventId::new([7u8; 32]);

        let mut s1 = ChannelState::from_meta(id, genesis.clone());
        s1.apply_meta(remove_c.clone());
        s1.apply_rename("renamed", 5, rename_id);

        let mut s2 = ChannelState::from_meta(id, genesis.clone());
        s2.apply_rename("renamed", 5, rename_id);
        s2.apply_meta(remove_c.clone());

        for s in [&s1, &s2] {
            assert_eq!(
                s.name(),
                "renamed",
                "the rename survives a concurrent removal"
            );
            assert_eq!(
                s.members().len(),
                2,
                "the removal survives a concurrent rename"
            );
            assert!(!s.is_member(&c.public().user_id()));
        }
    }

    #[test]
    fn apply_rename_is_last_writer_wins_by_lamport_then_id() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut s = ChannelState::from_meta(id, meta("general", &[&a], 0));
        // First rename adopts the name (any real lamport beats the genesis baseline).
        s.apply_rename("first", 3, EventId::new([1u8; 32]));
        assert_eq!(s.name(), "first");
        // Higher lamport wins.
        s.apply_rename("second", 5, EventId::new([0u8; 32]));
        assert_eq!(s.name(), "second");
        // Lower lamport is ignored (stale replay, idempotent).
        s.apply_rename("stale", 4, EventId::new([9u8; 32]));
        assert_eq!(s.name(), "second");
        // Equal lamport → higher id wins (deterministic cross-node tie-break).
        s.apply_rename("tiebreak", 5, EventId::new([9u8; 32]));
        assert_eq!(s.name(), "tiebreak");
        // A membership change never touches the name.
        s.apply_meta(meta("ignored", &[&a], 1));
        assert_eq!(s.name(), "tiebreak");
    }

    #[test]
    fn dissolved_channel_rejects_later_metadata_and_renames() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut state = ChannelState::from_meta(id, meta("general", &[&a], 0));
        state.dissolve();

        state.apply_meta(meta("general", &[&a, &b], 1));
        state.apply_rename("renamed", 2, EventId::new([1u8; 32]));

        assert!(state.is_dissolved());
        assert_eq!(state.epoch(), 0);
        assert_eq!(state.name(), "general");
        assert!(!state.is_member(&b.public().user_id()));
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

    fn sender_pair(epoch: u64) -> (ChannelState, ChannelState, String) {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        let alice_uid = a.public().user_id();
        alice.set_identity(alice_uid.clone());
        let mut bob = ChannelState::from_meta(id, meta("general", &[&a, &b], epoch));
        bob.set_identity(b.public().user_id());
        // Bob follows Alice's chain from her distribution.
        let skd = alice.my_sender_distribution();
        bob.record_sender_chain(alice_uid.clone(), epoch, &skd);
        (alice, bob, alice_uid)
    }

    #[test]
    fn receiver_candidates_bind_exact_epoch_before_installation() {
        let (mut alice, mut bob, alice_uid) = sender_pair(7);
        let mut next_epoch = ChannelState::from_meta(
            alice.id(),
            ChannelMeta {
                name: alice.name().into(),
                members: alice.members().to_vec(),
                epoch: 8,
                owner: alice.owner().into(),
            },
        );
        next_epoch.set_identity(alice_uid.clone());
        bob.record_sender_chain(alice_uid.clone(), 8, &next_epoch.my_sender_distribution());
        let old = alice.seal_sender_message(b"epoch seven").unwrap();
        let new = next_epoch.seal_sender_message(b"epoch eight").unwrap();
        let old_header_len = u16::from_be_bytes([old[0], old[1]]) as usize;
        let new_header_len = u16::from_be_bytes([new[0], new[1]]) as usize;
        let mut substituted = new[..2 + new_header_len].to_vec();
        substituted.extend_from_slice(&old[2 + old_header_len..]);
        assert!(bob.open_sender_message(&alice_uid, &substituted).is_none());
        assert_eq!(
            bob.open_sender_message(&alice_uid, &new).unwrap(),
            b"epoch eight"
        );
        assert_eq!(
            bob.open_sender_message(&alice_uid, &old).unwrap(),
            b"epoch seven"
        );
        assert!(bob.open_sender_message(&alice_uid, &new).is_none());
        assert!(bob.open_sender_message(&alice_uid, &old).is_none());
    }

    #[test]
    fn model_send_paths_return_exhaustion_without_mutation() {
        let (mut alice, _, _) = sender_pair(0);
        let mut encoded = vec![13; 32];
        encoded.extend_from_slice(&u32::MAX.to_le_bytes());
        alice.import_my_sender(0, SenderKey::deserialize(&encoded).unwrap());
        assert!(alice.seal_sender_message(b"cannot send").is_err());
        assert_eq!(alice.export_my_senders(), vec![(0, encoded.clone())]);
        assert!(alice.prepare_sender_message(b"cannot prepare").is_err());
        assert_eq!(alice.export_my_senders(), vec![(0, encoded)]);
    }

    #[test]
    fn failed_aead_does_not_consume_live_or_skipped_receiver_key() {
        let (mut alice, mut bob, alice_uid) = sender_pair(0);
        let first = alice.seal_sender_message(b"first").unwrap();
        let second = alice.seal_sender_message(b"second").unwrap();
        let mut forged = second.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert!(bob.open_sender_message(&alice_uid, &forged).is_none());
        assert_eq!(
            bob.open_sender_message(&alice_uid, &second).unwrap(),
            b"second"
        );
        let mut forged = first.clone();
        *forged.last_mut().unwrap() ^= 1;
        assert!(bob.open_sender_message(&alice_uid, &forged).is_none());
        assert_eq!(
            bob.open_sender_message(&alice_uid, &first).unwrap(),
            b"first"
        );
    }

    #[test]
    fn sender_key_seal_then_open() {
        let (mut alice, mut bob, alice_uid) = sender_pair(0);
        let wire = alice.seal_sender_message(b"hi").unwrap();
        assert_eq!(
            bob.open_sender_message(&alice_uid, &wire).as_deref(),
            Some(&b"hi"[..])
        );
    }

    #[test]
    fn sender_key_out_of_order() {
        let (mut alice, mut bob, alice_uid) = sender_pair(0);
        let w0 = alice.seal_sender_message(b"m0").unwrap();
        let w1 = alice.seal_sender_message(b"m1").unwrap();
        let w2 = alice.seal_sender_message(b"m2").unwrap();
        // Bob opens 2, 0, 1 — the chain ratchets forward and buffers skipped keys.
        assert_eq!(
            bob.open_sender_message(&alice_uid, &w2).as_deref(),
            Some(&b"m2"[..])
        );
        assert_eq!(
            bob.open_sender_message(&alice_uid, &w0).as_deref(),
            Some(&b"m0"[..])
        );
        assert_eq!(
            bob.open_sender_message(&alice_uid, &w1).as_deref(),
            Some(&b"m1"[..])
        );
    }

    #[test]
    fn sender_key_is_single_use() {
        let (mut alice, mut bob, alice_uid) = sender_pair(0);
        let wire = alice.seal_sender_message(b"once").unwrap();
        assert_eq!(
            bob.open_sender_message(&alice_uid, &wire).as_deref(),
            Some(&b"once"[..])
        );
        // The key was consumed (forward secrecy) — re-opening the same wire fails.
        assert!(bob.open_sender_message(&alice_uid, &wire).is_none());
    }

    #[test]
    fn open_rejects_sender_mismatch_without_consuming_chain() {
        // A member who holds Alice's chain could otherwise seal a message "as Alice"
        // (or replay one) under an event they author, burning Alice's single-use
        // position so her genuine message is dropped (censorship/DoS). The author bind
        // must reject it BEFORE the chain is consumed.
        let (mut alice, mut bob, alice_uid) = sender_pair(0);
        let wire = alice.seal_sender_message(b"hi").unwrap();
        // Same wire, but the authenticated author is someone else: rejected.
        assert!(bob.open_sender_message("mallory", &wire).is_none());
        // Crucially, Alice's position was NOT consumed — her genuine message still opens.
        assert_eq!(
            bob.open_sender_message(&alice_uid, &wire).as_deref(),
            Some(&b"hi"[..]),
        );
    }

    #[test]
    fn sender_key_open_without_chain_is_none() {
        let a = DeviceIdentity::generate();
        let id = new_channel_id();
        let mut alice = ChannelState::from_meta(id, meta("general", &[&a], 0));
        alice.set_identity(a.public().user_id());
        let wire = alice.seal_sender_message(b"secret").unwrap();
        // A member who never recorded Alice's chain cannot open her message.
        let mut outsider = ChannelState::from_meta(id, meta("general", &[&a], 0));
        outsider.set_identity("carol".into());
        assert!(
            outsider
                .open_sender_message(&a.public().user_id(), &wire)
                .is_none()
        );
    }

    #[test]
    fn account_projection_gives_owner_siblings_the_same_rights_and_removes_revoked_devices() {
        let owner = DeviceIdentity::generate();
        let sibling = DeviceIdentity::generate();
        let removed = DeviceIdentity::generate();
        let mut state =
            ChannelState::from_meta(new_channel_id(), meta("group", &[&owner, &removed], 4));
        state.install_account_projection(
            "owner-account".into(),
            vec![
                (owner.public(), "owner-account".into()),
                (sibling.public(), "owner-account".into()),
            ],
            5,
        );
        assert!(state.can_admin(&owner.public().user_id()));
        assert!(state.can_admin(&sibling.public().user_id()));
        assert!(!state.is_member(&removed.public().user_id()));
        assert!(!state.can_admin(&removed.public().user_id()));
        assert_eq!(state.epoch(), 5);
        assert_eq!(state.owner_account_id(), Some("owner-account"));
        assert_eq!(state.owner(), owner.public().user_id());
    }

    #[test]
    fn removed_pending_account_does_not_survive_the_next_authority_projection() {
        let owner = DeviceIdentity::generate();
        let invited = DeviceIdentity::generate();
        let mut state = ChannelState::from_meta(new_channel_id(), meta("group", &[&owner], 0));
        state.install_account_projection("owner".into(), vec![(owner.public(), "owner".into())], 0);
        state.install_account_invites(vec![(invited.public(), "invited".into())]);
        state.install_account_projection("owner".into(), vec![(owner.public(), "owner".into())], 1);
        assert!(
            !state.is_member(&invited.public().user_id()),
            "a removed pending account must disappear from every device roster"
        );
    }

    #[test]
    fn account_invites_receive_no_content_keys_until_the_account_accepts() {
        let owner = DeviceIdentity::generate();
        let invitee = DeviceIdentity::generate();
        let sibling = DeviceIdentity::generate();
        let mut state = ChannelState::from_meta(new_channel_id(), meta("group", &[&owner], 0));
        state.install_account_projection(
            "owner-account".into(),
            vec![(owner.public(), "owner-account".into())],
            0,
        );
        state.install_account_invites(vec![
            (invitee.public(), "invited-account".into()),
            (sibling.public(), "invited-account".into()),
        ]);
        assert_eq!(
            state.members().len(),
            3,
            "invites remain visible in the roster"
        );
        let recipients = state.recipient_members();
        let keys = crate::node::channel::seal_keys_for(
            &owner,
            &recipients,
            &state.my_sender_distribution(),
            0,
        )
        .unwrap();
        assert_eq!(keys.entries.len(), 1);
        assert_eq!(keys.entries[0].user_id, owner.public().user_id());
        state.install_account_projection(
            "owner-account".into(),
            vec![
                (owner.public(), "owner-account".into()),
                (invitee.public(), "invited-account".into()),
                (sibling.public(), "invited-account".into()),
            ],
            1,
        );
        let recipients = state.recipient_members();
        let keys = crate::node::channel::seal_keys_for(
            &owner,
            &recipients,
            &state.my_sender_distribution(),
            1,
        )
        .unwrap();
        assert_eq!(
            keys.entries.len(),
            3,
            "account consent includes both certified devices"
        );
    }

    #[test]
    fn current_receive_snapshot_recovers_a_gap_without_restoring_consumed_keys() {
        let author = DeviceIdentity::generate();
        let receiver = DeviceIdentity::generate();
        let channel = new_channel_id();
        let members = meta("gap", &[&author, &receiver], 0);
        let mut source = ChannelState::from_meta(channel, members.clone());
        source.set_identity(author.user_id());
        let initial = source.my_sender_distribution();
        let mut target = ChannelState::from_meta(channel, members);
        target.set_identity(receiver.user_id());
        target.record_sender_chain(author.user_id(), 0, &initial);
        let consumed = source.seal_sender_message(b"consumed").unwrap();
        assert!(
            target
                .open_sender_message(&author.user_id(), &consumed)
                .is_some()
        );
        let skipped = source
            .seal_sender_message(b"skipped before recovery")
            .unwrap();
        let next = source.seal_sender_message(b"consume after skip").unwrap();
        assert!(
            target
                .open_sender_message(&author.user_id(), &next)
                .is_some()
        );
        // These events are intentionally unavailable to this device. The
        // bounded normal ratchet must reject this gap rather than allocate it.
        for _ in 0..1002 {
            source.seal_sender_message(b"unavailable").unwrap();
        }
        let too_far = source.seal_sender_message(b"beyond skip bound").unwrap();
        assert!(
            target
                .open_sender_message(&author.user_id(), &too_far)
                .is_none()
        );
        let snapshot = source.current_receive_distributions().pop().unwrap().2;
        target.refresh_sender_chain(author.user_id(), 0, &snapshot);
        let future = source.seal_sender_message(b"future").unwrap();
        assert_eq!(
            target
                .open_sender_message(&author.user_id(), &future)
                .as_deref(),
            Some(&b"future"[..])
        );
        assert!(
            target
                .open_sender_message(&author.user_id(), &consumed)
                .is_none()
        );
        assert!(
            target
                .open_sender_message(&author.user_id(), &too_far)
                .is_none()
        );
        assert!(!target.has_my_sender(0));
        target.refresh_sender_chain(author.user_id(), 0, &initial);
        target.refresh_sender_chain(author.user_id(), 0, &snapshot);
        assert_eq!(
            target
                .open_sender_message(&author.user_id(), &skipped)
                .as_deref(),
            Some(&b"skipped before recovery"[..])
        );
        assert!(
            target
                .open_sender_message(&author.user_id(), &skipped)
                .is_none()
        );
        assert!(
            target
                .open_sender_message(&author.user_id(), &consumed)
                .is_none()
        );
        let following = source.seal_sender_message(b"still forward").unwrap();
        assert!(
            target
                .open_sender_message(&author.user_id(), &following)
                .is_some()
        );
    }

    #[test]
    fn routine_receive_snapshot_preserves_small_out_of_order_messages() {
        let author = DeviceIdentity::generate();
        let receiver = DeviceIdentity::generate();
        let channel = new_channel_id();
        let members = meta("out of order", &[&author, &receiver], 0);
        let mut source = ChannelState::from_meta(channel, members.clone());
        source.set_identity(author.user_id());
        let initial = source.my_sender_distribution();
        let mut target = ChannelState::from_meta(channel, members);
        target.set_identity(receiver.user_id());
        target.record_sender_chain(author.user_id(), 0, &initial);
        let pending = source
            .seal_sender_message(b"pending ordinary message")
            .unwrap();
        let later = source
            .seal_sender_message(b"later ordinary message")
            .unwrap();
        let snapshot = source.current_receive_distributions().pop().unwrap().2;
        target.refresh_sender_chain(author.user_id(), 0, &snapshot);
        assert_eq!(
            target
                .open_sender_message(&author.user_id(), &later)
                .as_deref(),
            Some(&b"later ordinary message"[..])
        );
        assert_eq!(
            target
                .open_sender_message(&author.user_id(), &pending)
                .as_deref(),
            Some(&b"pending ordinary message"[..])
        );
        assert!(
            target
                .open_sender_message(&author.user_id(), &pending)
                .is_none()
        );
        assert!(!target.has_my_sender(0));
    }

    #[test]
    fn receive_bootstrap_never_copies_another_devices_sending_chain() {
        let alice_identity = DeviceIdentity::generate();
        let target_identity = DeviceIdentity::generate();
        let members = meta("general", &[&alice_identity, &target_identity], 0);
        let id = new_channel_id();
        let mut source = ChannelState::from_meta(id, members.clone());
        source.set_identity(alice_identity.public().user_id());
        let old = source.seal_sender_message(b"before linking").unwrap();
        let snapshots = source.current_receive_distributions();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].0, alice_identity.public().user_id());
        assert_eq!(snapshots[0].1, 0);
        assert_eq!(snapshots[0].2.n, 1);
        let mut target = ChannelState::from_meta(id, members);
        target.set_identity(target_identity.public().user_id());
        for (author, epoch, distribution) in snapshots {
            target.record_sender_chain(author, epoch, &distribution);
        }
        assert!(
            !target.has_my_sender(0),
            "receive bootstrap never installs a sending key"
        );
        assert!(
            target
                .open_sender_message(&alice_identity.public().user_id(), &old)
                .is_none()
        );
        let future = source.seal_sender_message(b"after linking").unwrap();
        assert_eq!(
            target
                .open_sender_message(&alice_identity.public().user_id(), &future)
                .as_deref(),
            Some(&b"after linking"[..])
        );
        let target_distribution = target.my_sender_distribution();
        let source_distribution = source.my_sender_distribution();
        assert_ne!(target_distribution.chain_key, source_distribution.chain_key);
        assert_eq!(target_distribution.n, 0);
    }

    #[test]
    fn seal_sender_message_fails_without_identity() {
        let a = DeviceIdentity::generate();
        let mut state = ChannelState::from_meta(new_channel_id(), meta("general", &[&a], 0));
        assert!(state.seal_sender_message(b"x").is_err());
    }
}

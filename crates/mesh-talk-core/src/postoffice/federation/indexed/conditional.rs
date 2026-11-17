//! Constant-size, authenticated no-change proofs. Cache eviction only costs a full round.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub(super) const REQUEST_V5: &[u8] = b"MTFI5Q";
pub(super) const DENIED: &[u8] = b"MTFI5N";
pub(super) const SUPPORTED: &[u8] = b"MTFI5R";
const OFFER: &[u8] = b"MTFI5S";
const DECISION: &[u8] = b"MTFI5D";
pub(super) const END: &[u8] = b"MTFI5E";
const ACK: &[u8] = b"MTFI5A";
const FINISH: &[u8] = b"MTFI5F";
const MAX_PEERS: usize = 256;
const CAPABILITY_RETRY: Duration = Duration::from_secs(60);
type Stamp = [u8; 32];
type PeerKey = ([u8; 32], [u8; 32]);
type Pair = (Stamp, Stamp);
struct Entry {
    proof: Option<Pair>,
    unsupported_until: Option<Instant>,
    used: Instant,
}
pub(super) struct ConditionalState {
    nonce: [u8; 16],
    peers: Mutex<BTreeMap<PeerKey, Entry>>,
}
impl Default for ConditionalState {
    fn default() -> Self {
        Self {
            nonce: *uuid::Uuid::new_v4().as_bytes(),
            peers: Mutex::new(BTreeMap::new()),
        }
    }
}
fn key(peer: &PublicIdentity) -> PeerKey {
    (peer.ed25519_pub, peer.x25519_pub)
}
impl ConditionalState {
    pub(super) fn probe_version(&self, peer: &PublicIdentity) -> bool {
        let mut peers = self.peers.lock().expect("conditional peer cache");
        peers.get_mut(&key(peer)).is_none_or(|entry| {
            entry.used = Instant::now();
            entry
                .unsupported_until
                .is_none_or(|until| until <= Instant::now())
        })
    }
    fn put(&self, peer: &PublicIdentity, proof: Option<Pair>, unsupported_until: Option<Instant>) {
        let mut peers = self.peers.lock().expect("conditional peer cache");
        if peers.len() >= MAX_PEERS
            && !peers.contains_key(&key(peer))
            && let Some(oldest) = peers
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .map(|(key, _)| *key)
        {
            peers.remove(&oldest);
        }
        peers.insert(
            key(peer),
            Entry {
                proof,
                unsupported_until,
                used: Instant::now(),
            },
        );
    }
    pub(super) fn unsupported(&self, peer: &PublicIdentity) {
        self.put(peer, None, Some(Instant::now() + CAPABILITY_RETRY));
    }
    fn may_skip(&self, peer: &PublicIdentity, local: Option<Stamp>) -> bool {
        let Some(local) = local else {
            return false;
        };
        self.peers
            .lock()
            .expect("conditional peer cache")
            .get(&key(peer))
            .is_some_and(|entry| entry.proof.is_some_and(|pair| pair.0 == local))
    }
    fn matches(&self, proof: &ProofContext) -> bool {
        let (Some(local), Some(remote)) = (proof.local, proof.remote) else {
            return false;
        };
        let mut peers = self.peers.lock().expect("conditional peer cache");
        peers.get_mut(&key(&proof.peer)).is_some_and(|entry| {
            entry.used = Instant::now();
            entry.proof == Some((local, remote))
        })
    }
    fn remember(&self, proof: &ProofContext) {
        if let (Some(local), Some(remote)) = (proof.local, proof.remote) {
            self.put(&proof.peer, Some((local, remote)), None);
        }
    }
}
pub(super) struct ProofContext {
    peer: PublicIdentity,
    local: Option<Stamp>,
    remote: Option<Stamp>,
}
fn encode_offer(stamp: Option<Stamp>) -> Vec<u8> {
    let mut bytes = OFFER.to_vec();
    bytes.push(u8::from(stamp.is_some()));
    if let Some(stamp) = stamp {
        bytes.extend_from_slice(&stamp);
    }
    bytes
}
fn decode_offer(bytes: &[u8]) -> Result<Option<Stamp>, IndexedRelayError> {
    if bytes == DENIED {
        return Err("federation peer is not authorized".into());
    }
    let body = bytes
        .strip_prefix(OFFER)
        .ok_or("invalid conditional inventory offer")?;
    match body {
        [0] => Ok(None),
        [1, stamp @ ..] if stamp.len() == 32 => {
            Ok(Some(stamp.try_into().expect("checked stamp size")))
        }
        _ => Err("invalid conditional inventory stamp".into()),
    }
}
fn encode_flag(prefix: &[u8], value: bool) -> Vec<u8> {
    let mut bytes = prefix.to_vec();
    bytes.push(u8::from(value));
    bytes
}
pub(super) fn decode_flag(bytes: &[u8], prefix: &[u8]) -> Result<bool, IndexedRelayError> {
    match bytes.strip_prefix(prefix) {
        Some([0]) => Ok(false),
        Some([1]) => Ok(true),
        _ => Err("invalid conditional completion frame".into()),
    }
}
async fn receive_supported<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
) -> Result<(), IndexedRelayError> {
    let bytes = channel.recv().await.map_err(|error| {
        if matches!(&error, crate::transport::TransportError::Io(io)
            if matches!(io.kind(), std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset)) {
            IndexedRelayError::UnsupportedVersion
        } else { error.into() }
    })?;
    if bytes == DENIED {
        return Err("federation peer is not authorized".into());
    }
    if bytes != SUPPORTED {
        return Err("invalid conditional version acceptance".into());
    }
    Ok(())
}
async fn receive_offer<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
) -> Result<Option<Stamp>, IndexedRelayError> {
    decode_offer(&channel.recv().await?)
}
fn decision(mode: u8) -> Vec<u8> {
    let mut bytes = DECISION.to_vec();
    bytes.push(mode);
    bytes
}
fn decode_decision(bytes: &[u8]) -> Result<u8, IndexedRelayError> {
    match bytes.strip_prefix(DECISION) {
        Some([mode @ 0..=2]) => Ok(*mode),
        _ => Err("invalid conditional inventory decision".into()),
    }
}
impl IndexedRelayEndpoint {
    fn conditional_stamp(&self) -> Result<Option<Stamp>, IndexedRelayError> {
        Ok(self
            .office
            .state
            .conditional_inventory_stamp_at(unix_now())?
            .map(|stamp| {
                let mut hash = Sha256::new();
                hash.update(b"mesh-talk-federation-runtime-stamp-v1");
                hash.update(self.conditional.nonce);
                hash.update(stamp);
                hash.finalize().into()
            }))
    }
    fn stamp_matches(&self, stamp: Option<Stamp>) -> Result<bool, IndexedRelayError> {
        Ok(stamp.is_some() && self.conditional_stamp()? == stamp)
    }
    pub(super) async fn sync_conditional(
        &self,
        addr: SocketAddr,
        identity: &DeviceIdentity,
        expected: &PublicIdentity,
        network: Option<&super::super::super::network::RelayNetwork>,
        started: Instant,
    ) -> Result<usize, IndexedRelayError> {
        // A known full exchange keeps the established fail-before-connect storage
        // boundary. Only an already proven local token may defer this allocation.
        let owner = if self
            .conditional
            .may_skip(expected, self.conditional_stamp()?)
        {
            None
        } else {
            Some(SessionOwner::begin(
                self.office.state.profile().clone(),
                Direction::Outbound,
                unix_now(),
                started,
            )?)
        };
        let stream = TcpStream::connect(addr).await?;
        stream.set_nodelay(true)?;
        let mut channel = SecureChannel::connect(stream, identity, Some(expected)).await?;
        if let Some(network) = network {
            network.connect(&mut channel).await?;
        }
        send_frame(&mut channel, REQUEST_V5).await?;
        receive_supported(&mut channel).await?;
        let local = self.conditional_stamp()?;
        send_frame(&mut channel, &encode_offer(local)).await?;
        let remote = receive_offer(&mut channel).await?;
        let proof = ProofContext {
            peer: expected.clone(),
            local,
            remote,
        };
        let candidate =
            owner.is_none() && self.conditional.matches(&proof) && self.stamp_matches(local)?;
        send_frame(&mut channel, &decision(u8::from(candidate))).await?;
        match decode_decision(&channel.recv().await?)? {
            1 if candidate => {
                self.fast_presence(&mut channel, &proof, true).await?;
                Ok(0)
            }
            1 => Err("unagreed conditional fast path".into()),
            2 => Err(IndexedRelayError::PreparationPending),
            _ => {
                let owner = match owner {
                    Some(owner) => owner,
                    None => SessionOwner::begin(
                        self.office.state.profile().clone(),
                        Direction::Outbound,
                        unix_now(),
                        started,
                    )?,
                };
                self.sync_inventory_on(&mut channel, expected, REQUEST_V4, owner, Some(&proof))
                    .await
            }
        }
    }
    pub(super) async fn serve_conditional<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        started: Instant,
    ) -> Result<(), IndexedRelayError> {
        // Positive capability is independent of storage readiness and is sent
        // only after the caller has passed existing federation authorization.
        send_frame(channel, SUPPORTED).await?;
        let remote = receive_offer(channel).await?;
        let local = self.conditional_stamp()?;
        send_frame(channel, &encode_offer(local)).await?;
        let proof = ProofContext {
            peer: channel.peer_identity().clone(),
            local,
            remote,
        };
        let candidate = decode_decision(&channel.recv().await?)?;
        if candidate == 2 {
            return Err("invalid requester retry decision".into());
        }
        let fast =
            candidate == 1 && self.conditional.matches(&proof) && self.stamp_matches(local)?;
        if !fast && let Err(error) = self.prepared() {
            if error.is_local_retry() {
                send_frame(channel, &decision(2)).await?;
            }
            return Err(error);
        }
        send_frame(channel, &decision(u8::from(fast))).await?;
        if fast {
            self.fast_presence(channel, &proof, false).await
        } else {
            self.serve_inventory_on(channel, REQUEST_V4, started, Some(&proof))
                .await
        }
    }
    async fn fast_presence<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        proof: &ProofContext,
        requester: bool,
    ) -> Result<(), IndexedRelayError> {
        let limit = presence_limit(REQUEST_V4);
        if requester {
            send_presence_pages(channel, self.directory(), limit, true, true).await?;
            receive_presence_pages(channel, self.directory(), limit, true, true).await?;
            let unchanged = self.stamp_matches(proof.local)?;
            send_frame(channel, &encode_flag(FINISH, unchanged)).await?;
            let agreed = decode_flag(&channel.recv().await?, ACK)?;
            if !agreed || !unchanged || !self.stamp_matches(proof.local)? {
                return Err(LogError::StaleCursor.into());
            }
        } else {
            receive_presence_pages(channel, self.directory(), limit, true, true).await?;
            send_presence_pages(channel, self.directory(), limit, true, true).await?;
            let unchanged =
                decode_flag(&channel.recv().await?, FINISH)? && self.stamp_matches(proof.local)?;
            send_frame(channel, &encode_flag(ACK, unchanged)).await?;
            if !unchanged {
                return Err(LogError::StaleCursor.into());
            }
        }
        Ok(())
    }
    pub(super) async fn finish_conditional_client<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        proof: &ProofContext,
        equal: bool,
    ) -> Result<(), IndexedRelayError> {
        let eligible = equal && self.stamp_matches(proof.local)?;
        send_frame(channel, &encode_flag(END, eligible)).await?;
        let confirmed = decode_flag(&channel.recv().await?, ACK)?;
        if eligible && confirmed && self.stamp_matches(proof.local)? {
            self.conditional.remember(proof);
        }
        Ok(())
    }
    pub(super) async fn finish_conditional_server<IO: AsyncRead + AsyncWrite + Unpin>(
        &self,
        channel: &mut SecureChannel<IO>,
        proof: &ProofContext,
        view: &InventoryView,
        lease: &SessionLease,
        claimed_equal: bool,
    ) -> Result<(), IndexedRelayError> {
        let mut eligible = claimed_equal && self.stamp_matches(proof.local)?;
        if eligible {
            let scan = async {
                self.office
                    .state
                    .begin_inventory_mismatch_at(view, lease, None, unix_now())?;
                loop {
                    let step = self.office.state.inventory_mismatch_step_at(
                        view,
                        lease,
                        512,
                        1,
                        unix_now(),
                    )?;
                    if !step.ids.is_empty() {
                        return Ok::<_, LogError>(false);
                    }
                    if step.done {
                        return Ok(true);
                    }
                    tokio::task::yield_now().await;
                }
            }
            .await;
            eligible = match scan {
                Ok(equal) => equal,
                Err(LogError::StaleCursor) => false,
                Err(error) => return Err(error.into()),
            };
        }
        eligible = eligible && self.stamp_matches(proof.local)?;
        send_frame(channel, &encode_flag(ACK, eligible)).await?;
        if eligible {
            self.conditional.remember(proof);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn proof(n: u16) -> ProofContext {
        let mut ed = [1; 32];
        ed[..2].copy_from_slice(&n.to_be_bytes());
        ProofContext {
            peer: PublicIdentity {
                ed25519_pub: ed,
                x25519_pub: [2; 32],
            },
            local: Some([3; 32]),
            remote: Some([4; 32]),
        }
    }
    #[test]
    fn conditional_cache_is_bounded_and_bound_to_both_identity_keys_and_stamps() {
        let state = ConditionalState::default();
        for n in 0..=MAX_PEERS as u16 {
            state.remember(&proof(n));
        }
        assert_eq!(state.peers.lock().unwrap().len(), MAX_PEERS);
        assert!(!state.matches(&proof(0)));
        assert!(state.matches(&proof(MAX_PEERS as u16)));
        let mut changed = proof(MAX_PEERS as u16);
        changed.peer.x25519_pub = [5; 32];
        assert!(!state.matches(&changed));
        changed = proof(MAX_PEERS as u16);
        changed.local = Some([6; 32]);
        assert!(!state.matches(&changed));
        changed = proof(MAX_PEERS as u16);
        changed.remote = None;
        assert!(!state.matches(&changed));
        state.unsupported(&proof(1).peer);
        assert!(!state.matches(&proof(1)));
        assert!(!state.probe_version(&proof(1).peer));
    }
    #[test]
    fn conditional_frames_reject_unbounded_trailing_or_invalid_values() {
        for stamp in [None, Some([7; 32])] {
            assert_eq!(decode_offer(&encode_offer(stamp)).unwrap(), stamp);
        }
        for bytes in [
            b"MTFI5S\x02".as_slice(),
            b"MTFI5S\x00extra",
            b"MTFI5S\x01short",
            DENIED,
        ] {
            assert!(decode_offer(bytes).is_err());
        }
        assert!(decode_flag(b"MTFI5A\x02", ACK).is_err());
        assert!(decode_flag(b"MTFI5A\x01extra", ACK).is_err());
        assert!(decode_decision(b"MTFI5D\x03").is_err());
    }
    async fn append_during_fast_refresh(changed_requester: bool) {
        use super::super::conditional_tests::{pair_fixture, pair_round};
        use super::super::tests::opaque_event;
        let pair = pair_fixture();
        pair_round(&pair).await.unwrap();
        let local = pair.local.clone();
        let remote = pair.remote.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (ed, x) = pair.server.secret_bytes();
        let server = tokio::spawn(async move {
            let identity = DeviceIdentity::from_secret_bytes(ed, x);
            let mut channel = SecureChannel::accept(listener.accept().await.unwrap().0, &identity)
                .await
                .unwrap();
            assert_eq!(channel.recv().await.unwrap(), REQUEST_V5);
            send_frame(&mut channel, SUPPORTED).await.unwrap();
            let local_stamp = remote.conditional_stamp().unwrap();
            let remote_stamp = receive_offer(&mut channel).await.unwrap();
            let proof = ProofContext {
                peer: channel.peer_identity().clone(),
                local: local_stamp,
                remote: remote_stamp,
            };
            assert!(remote.conditional.matches(&proof));
            send_frame(&mut channel, &encode_offer(local_stamp))
                .await
                .unwrap();
            assert_eq!(decode_decision(&channel.recv().await.unwrap()).unwrap(), 1);
            send_frame(&mut channel, &decision(1)).await.unwrap();
            let event = opaque_event(1, &DeviceIdentity::generate(), &DeviceIdentity::generate());
            let changed = if changed_requester { &local } else { &remote };
            changed
                .office
                .state
                .record(&event, unix_now(), None)
                .unwrap();
            remote.fast_presence(&mut channel, &proof, false).await
        });
        let result = pair
            .local
            .sync_with_relay_checked(addr, &pair.client, &pair.server.public())
            .await;
        let remote_result = server.await.unwrap();
        assert!(matches!(
            result,
            Err(IndexedRelayError::Store(LogError::StaleCursor))
        ));
        assert!(matches!(
            remote_result,
            Err(IndexedRelayError::Store(LogError::StaleCursor))
        ));
    }
    #[tokio::test]
    async fn requester_append_after_fast_decision_cannot_report_unchanged_success() {
        append_during_fast_refresh(true).await;
    }
    #[tokio::test]
    async fn responder_append_after_fast_decision_cannot_report_unchanged_success() {
        append_during_fast_refresh(false).await;
    }
}

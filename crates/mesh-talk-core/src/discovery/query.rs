//! First-contact discovery over a routed IP path.

use crate::discovery::announce::{Announce, decode as decode_announce, encode as encode_announce};
use crate::identity::device::DeviceIdentity;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub type Nonce = [u8; 16];

const QUERY_MAGIC: &[u8; 4] = b"MTDQ";
const REPLY_MAGIC: &[u8; 4] = b"MTDR";
const VERSION: u8 = 1;
const QUERY_SIZE: usize = 64;
const REPLY_HEADER: usize = 4 + 1 + 16 + 2;
const SIGNATURE_SIZE: usize = 64;
const MAX_ANNOUNCE: usize = 1600;
const SIGNING_DOMAIN: &[u8] = b"mesh-talk-discovery-response-v1";
const QUERY_LIFETIME: Duration = Duration::from_secs(5);
const MAX_PENDING: usize = 256;

/// Short-lived request state. An unsolicited or replayed signed announce can
/// never refresh online presence through the routed query path.
#[derive(Default)]
pub struct QueryTracker {
    pending: HashMap<(IpAddr, Nonce), Instant>,
}

impl QueryTracker {
    pub fn record(&mut self, ip: IpAddr, nonce: Nonce, now: Instant) -> bool {
        self.pending
            .retain(|_, sent| now.saturating_duration_since(*sent) <= QUERY_LIFETIME);
        if !self.pending.contains_key(&(ip, nonce)) && self.pending.len() >= MAX_PENDING {
            return false;
        }
        self.pending.insert((ip, nonce), now);
        true
    }

    pub fn accept(&mut self, ip: IpAddr, bytes: &[u8], now: Instant) -> Option<Announce> {
        // Read only the bounded nonce field before the full authenticated decode.
        let nonce: Nonce = bytes.get(5..21)?.try_into().ok()?;
        let key = (ip, nonce);
        let sent = *self.pending.get(&key)?;
        if now.saturating_duration_since(sent) > QUERY_LIFETIME {
            self.pending.remove(&key);
            return None;
        }
        let announce = decode_response(bytes, nonce)?;
        self.pending.remove(&key);
        Some(announce)
    }
}

/// A fixed-size query carries no application identity. Padding limits UDP
/// amplification by making the request larger than the protocol header.
pub fn encode_query(nonce: Nonce) -> [u8; QUERY_SIZE] {
    let mut out = [0u8; QUERY_SIZE];
    out[..4].copy_from_slice(QUERY_MAGIC);
    out[4] = VERSION;
    out[5..21].copy_from_slice(&nonce);
    out
}

pub fn decode_query(bytes: &[u8]) -> Option<Nonce> {
    if bytes.len() != QUERY_SIZE
        || &bytes[..4] != QUERY_MAGIC
        || bytes[4] != VERSION
        || bytes[21..].iter().any(|byte| *byte != 0)
    {
        return None;
    }
    bytes[5..21].try_into().ok()
}

fn signing_input(nonce: Nonce, announce: &[u8]) -> Vec<u8> {
    let mut input = Vec::with_capacity(SIGNING_DOMAIN.len() + nonce.len() + announce.len());
    input.extend_from_slice(SIGNING_DOMAIN);
    input.extend_from_slice(&nonce);
    input.extend_from_slice(announce);
    input
}

/// Sign one query-specific response using the announced device key.
pub fn encode_response(
    nonce: Nonce,
    announce: &Announce,
    identity: &DeviceIdentity,
) -> Option<Vec<u8>> {
    encode_response_signed(nonce, announce, &identity.public(), |input| {
        identity.sign(input)
    })
}

pub(crate) fn encode_response_signed(
    nonce: Nonce,
    announce: &Announce,
    public: &crate::identity::device::PublicIdentity,
    sign: impl FnOnce(&[u8]) -> [u8; 64],
) -> Option<Vec<u8>> {
    if !announce.valid_presence() || announce.public() != *public {
        return None;
    }
    let body = encode_announce(announce);
    if body.len() > MAX_ANNOUNCE {
        return None;
    }
    let mut out = Vec::with_capacity(REPLY_HEADER + body.len() + SIGNATURE_SIZE);
    out.extend_from_slice(REPLY_MAGIC);
    out.push(VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&(body.len() as u16).to_be_bytes());
    out.extend_from_slice(&body);
    out.extend_from_slice(&sign(&signing_input(nonce, &body)));
    Some(out)
}

/// Verify both the existing announce signature and the fresh nonce binding.
pub fn decode_response(bytes: &[u8], expected_nonce: Nonce) -> Option<Announce> {
    if bytes.len() < REPLY_HEADER + SIGNATURE_SIZE
        || bytes.len() > REPLY_HEADER + MAX_ANNOUNCE + SIGNATURE_SIZE
        || &bytes[..4] != REPLY_MAGIC
        || bytes[4] != VERSION
        || bytes[5..21] != expected_nonce
    {
        return None;
    }
    let len = u16::from_be_bytes(bytes[21..23].try_into().ok()?) as usize;
    if len > MAX_ANNOUNCE || bytes.len() != REPLY_HEADER + len + SIGNATURE_SIZE {
        return None;
    }
    let body = &bytes[REPLY_HEADER..REPLY_HEADER + len];
    let announce = decode_announce(body)?;
    if !announce.valid_presence() {
        return None;
    }
    let signature: [u8; 64] = bytes[REPLY_HEADER + len..].try_into().ok()?;
    DeviceIdentity::verify(
        &announce.ed25519_pub,
        &signing_input(expected_nonce, body),
        &signature,
    )
    .then_some(announce)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn query_is_identity_free_and_only_a_nonce_bound_reply_is_accepted() {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_announce = Announce::new(&bob, "Bob", 47475);
        let nonce = [7; 16];
        let request = encode_query(nonce);
        assert_eq!(decode_query(&request), Some(nonce));
        assert!(
            !request
                .windows(alice.public().ed25519_pub.len())
                .any(|w| { w == alice.public().ed25519_pub })
        );

        let reply = encode_response(nonce, &bob_announce, &bob).unwrap();
        assert_eq!(decode_response(&reply, nonce), Some(bob_announce));
        assert!(decode_response(&reply, [8; 16]).is_none());
        let mut tampered = reply;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decode_response(&tampered, nonce).is_none());
    }

    #[test]
    fn independent_queries_to_colocated_services_accept_both_replies() {
        let desktop = DeviceIdentity::generate();
        let relay = DeviceIdentity::generate();
        let desktop_announce = Announce::new(&desktop, "Desktop", 50001);
        let relay_announce = Announce::new_post_office(&relay, "Post office", 47475);
        let now = Instant::now();
        let ip = "100.64.0.10".parse().unwrap();
        for relay_first in [false, true] {
            let mut tracker = QueryTracker::default();
            assert!(tracker.record(ip, [1; 16], now));
            assert!(tracker.record(ip, [2; 16], now));
            let desktop_reply = encode_response([1; 16], &desktop_announce, &desktop).unwrap();
            let relay_reply = encode_response([2; 16], &relay_announce, &relay).unwrap();
            let replies = if relay_first {
                [&relay_reply, &desktop_reply]
            } else {
                [&desktop_reply, &relay_reply]
            };
            for reply in replies {
                assert!(
                    tracker.accept(ip, reply, now).is_some(),
                    "one pending query must not replace another service on the same host"
                );
                assert!(
                    tracker.accept(ip, reply, now).is_none(),
                    "a consumed nonce cannot refresh presence again"
                );
            }
        }
    }

    #[test]
    fn colocated_queries_remain_bounded_and_invalid_replies_do_not_consume_them() {
        let identity = DeviceIdentity::generate();
        let announce = Announce::new(&identity, "Desktop", 50001);
        let ip = "::1".parse().unwrap();
        let now = Instant::now();
        let mut tracker = QueryTracker::default();
        for index in 0..MAX_PENDING {
            assert!(tracker.record(ip, (index as u128).to_be_bytes(), now));
        }
        assert!(!tracker.record(ip, [255; 16], now));
        let reply = encode_response([0; 16], &announce, &identity).unwrap();
        let mut invalid = reply.clone();
        *invalid.last_mut().unwrap() ^= 1;
        assert!(tracker.accept(ip, &invalid, now).is_none());
        assert!(tracker.accept(ip, &reply[..20], now).is_none());
        assert_eq!(tracker.accept(ip, &reply, now), Some(announce));
        assert!(tracker.record(ip, [255; 16], now));
        let later = now + QUERY_LIFETIME + Duration::from_millis(1);
        assert!(tracker.accept(ip, &reply, later).is_none());
        assert!(tracker.record(ip, [254; 16], later));
        assert_eq!(tracker.pending.len(), 1);
    }

    #[test]
    fn pending_response_is_bound_to_source_ip_and_consumed_once() {
        let bob = DeviceIdentity::generate();
        let announce = Announce::new(&bob, "Bob", 47475);
        let nonce = [3; 16];
        let reply = encode_response(nonce, &announce, &bob).unwrap();
        let now = std::time::Instant::now();
        let expected = "100.101.102.103".parse().unwrap();
        let attacker = "100.101.102.104".parse().unwrap();
        let mut tracker = QueryTracker::default();
        assert!(tracker.record(expected, nonce, now));
        assert!(tracker.accept(attacker, &reply, now).is_none());
        assert!(tracker.accept(expected, &reply, now).is_some());
        assert!(tracker.accept(expected, &reply, now).is_none());
        assert!(tracker.record(expected, nonce, now));
        assert!(
            tracker
                .accept(expected, &reply, now + std::time::Duration::from_secs(6))
                .is_none()
        );
    }
}

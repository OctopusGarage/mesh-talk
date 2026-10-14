//! Opt-in introductions through an existing post office. The post office keeps
//! only short-lived, self-authored identity proofs; it never speaks for a peer or
//! learns the peer's LAN address from the directory protocol.

use crate::discovery::Announce;
use crate::identity::device::PublicIdentity;
use crate::transport::MAX_PLAINTEXT;
use bincode::Options;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const REQUEST: &[u8] = b"MTBD1Q";
const RESPONSE: &[u8] = b"MTBD1R";
const TTL: Duration = Duration::from_secs(45);
const MAX_MEMBERS: usize = 128;

#[derive(Default)]
pub struct BridgeDirectory {
    members: HashMap<String, (Announce, Instant)>,
    gateway: Option<Arc<Mutex<Option<Announce>>>>,
}

impl BridgeDirectory {
    pub fn with_gateway(gateway: Arc<Mutex<Option<Announce>>>) -> Self {
        Self {
            members: HashMap::new(),
            gateway: Some(gateway),
        }
    }

    /// The caller's Noise-authenticated presence is the only admissible proof.
    /// A request body cannot nominate an identity or an address on its behalf.
    pub fn register(
        &mut self,
        public: &PublicIdentity,
        presence: Option<&Announce>,
        publicly_visible: bool,
        now: Instant,
    ) -> Option<Vec<Announce>> {
        self.members
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < TTL);
        let proof = presence.filter(|proof| {
            publicly_visible
                && proof.verify()
                && proof.public() == *public
                && proof.account_id().is_some()
                && proof.tcp_port != 0
                && proof.name.len() <= 1024
        })?;
        if self.members.len() >= MAX_MEMBERS && !self.members.contains_key(&proof.user_id) {
            return None;
        }
        self.members
            .insert(proof.user_id.clone(), (proof.clone(), now));
        let mut proofs: Vec<_> = self
            .members
            .iter()
            .filter(|(id, _)| *id != &proof.user_id)
            .map(|(_, (proof, _))| proof.clone())
            .collect();
        if let Some(gateway) = &self.gateway
            && let Some(remote) = gateway
                .lock()
                .expect("gateway proof lock not poisoned")
                .clone()
            && remote.post_office
            && remote.verify()
            && remote.account_id().is_some()
            && remote.tcp_port != 0
            && remote.name.len() <= 1024
        {
            proofs.push(remote);
        }
        Some(proofs)
    }
}

pub fn is_request(bytes: &[u8]) -> bool {
    bytes == REQUEST
}

pub fn request() -> &'static [u8] {
    REQUEST
}

pub fn encode_response(proofs: &[Announce]) -> Option<Vec<Vec<u8>>> {
    let mut ordered: Vec<_> = proofs
        .iter()
        .filter(|proof| proof.verify())
        .cloned()
        .collect();
    ordered.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    let mut pages = Vec::new();
    let mut selected = Vec::new();
    for proof in ordered {
        selected.push(proof);
        let encoded = bincode::serialize(&selected).ok()?;
        if RESPONSE.len() + 1 + encoded.len() > MAX_PLAINTEXT {
            let overflow = selected.pop()?;
            if selected.is_empty() {
                return None;
            }
            pages.push(frame(&selected, false)?);
            selected = vec![overflow];
        }
    }
    pages.push(frame(&selected, true)?);
    Some(pages)
}

fn frame(proofs: &[Announce], last: bool) -> Option<Vec<u8>> {
    let mut bytes = Vec::from(RESPONSE);
    bytes.push(u8::from(last));
    bytes.extend_from_slice(&bincode::serialize(proofs).ok()?);
    (bytes.len() <= MAX_PLAINTEXT).then_some(bytes)
}

pub fn decode_response(bytes: &[u8]) -> Option<(Vec<Announce>, bool)> {
    let body = bytes.strip_prefix(RESPONSE)?;
    if bytes.len() > MAX_PLAINTEXT || body.is_empty() || body[0] > 1 {
        return None;
    }
    let last = body[0] == 1;
    let proofs: Vec<Announce> = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_PLAINTEXT as u64)
        .reject_trailing_bytes()
        .deserialize(&body[1..])
        .ok()?;
    (proofs.len() <= MAX_MEMBERS
        && proofs.iter().all(|proof| {
            proof.verify()
                && proof.account_id().is_some()
                && proof.tcp_port != 0
                && proof.name.len() <= 1024
        }))
    .then_some((proofs, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn only_authenticated_visible_self_presence_can_join() {
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let account = Account::generate();
        let proof = Announce::new_with_account(&a, &account, "A", 47475);
        let now = Instant::now();
        let mut directory = BridgeDirectory::default();
        assert!(
            directory
                .register(&b.public(), Some(&proof), true, now)
                .is_none()
        );
        assert!(
            directory
                .register(&a.public(), Some(&proof), false, now)
                .is_none()
        );
        assert_eq!(
            directory.register(&a.public(), Some(&proof), true, now),
            Some(Vec::new())
        );
        assert_eq!(
            directory
                .register(
                    &b.public(),
                    Some(&Announce::new_with_account(&b, &account, "B", 47476)),
                    true,
                    now
                )
                .unwrap(),
            vec![proof.clone()]
        );
        assert!(
            directory
                .register(&b.public(), None, true, now + TTL)
                .is_none()
        );
        assert!(directory.members.is_empty());
    }

    #[test]
    fn response_rejects_tampered_or_trailing_proofs() {
        let a = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&a, &Account::generate(), "A", 47475);
        let encoded = encode_response(std::slice::from_ref(&proof)).unwrap();
        assert_eq!(encoded.len(), 1);
        assert_eq!(decode_response(&encoded[0]), Some((vec![proof], true)));
        let mut trailing = encoded[0].clone();
        trailing.push(0);
        assert!(decode_response(&trailing).is_none());
    }

    #[test]
    fn gateway_proof_is_shared_only_after_visible_self_registration() {
        let a = DeviceIdentity::generate();
        let e = DeviceIdentity::generate();
        let account = Account::generate();
        let a_proof = Announce::new_with_account(&a, &account, "A", 47475);
        let remote = Announce::new_post_office_with_account(&e, &account, "E", 47476);
        let gateway = Arc::new(Mutex::new(Some(remote.clone())));
        let mut directory = BridgeDirectory::with_gateway(Arc::clone(&gateway));
        let now = Instant::now();

        assert!(directory.register(&a.public(), None, true, now).is_none());
        assert!(
            directory
                .register(&a.public(), Some(&a_proof), false, now)
                .is_none()
        );
        assert_eq!(
            directory.register(&a.public(), Some(&a_proof), true, now),
            Some(vec![remote])
        );
        *gateway.lock().unwrap() = None;
        assert_eq!(
            directory.register(&a.public(), Some(&a_proof), true, now),
            Some(Vec::new())
        );
    }

    #[test]
    fn a_full_directory_is_paged_without_losing_identities() {
        let account = Account::generate();
        let proofs: Vec<_> = (0..MAX_MEMBERS)
            .map(|_| {
                Announce::new_with_account(
                    &DeviceIdentity::generate(),
                    &account,
                    "long name".repeat(100),
                    47475,
                )
            })
            .collect();
        let pages = encode_response(&proofs).unwrap();
        assert!(pages.len() > 1);
        let mut decoded = Vec::new();
        for (i, page) in pages.iter().enumerate() {
            let (members, last) = decode_response(page).unwrap();
            assert_eq!(last, i == pages.len() - 1);
            decoded.extend(members);
        }
        decoded.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        let mut expected = proofs;
        expected.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        assert_eq!(decoded, expected);
    }
}

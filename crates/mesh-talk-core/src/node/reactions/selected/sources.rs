//! Selected proof resolution preserves the existing causal/bound/directory order.
use super::*;
use crate::discovery::Announce;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};

type DevicePublicKeys = ([u8; 32], [u8; 32]);
type AccountIdsByDevice = BTreeMap<DevicePublicKeys, BTreeSet<[u8; 32]>>;

pub(super) struct Evidence {
    pub proofs: Vec<Announce>,
    accounts: AccountIdsByDevice,
    ambiguous: HashSet<([u8; 32], [u8; 32])>,
    pub stamp: [u8; 32],
}
impl Evidence {
    pub fn capture(node: &Node) -> Result<Self, NodeError> {
        let mut proofs = node.historical_peer_proofs();
        proofs.sort_by_key(|p| (p.ed25519_pub, p.x25519_pub));
        let mut accounts: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for proof in &proofs {
            if proof.valid_presence()
                && !proof.post_office
                && let Some(cert) = &proof.account_cert
            {
                accounts
                    .entry((proof.ed25519_pub, proof.x25519_pub))
                    .or_default()
                    .insert(cert.account_ed25519_pub);
            }
        }
        for patch in node.account_binding_patches() {
            for transition in patch.transitions {
                if transition.verify() {
                    let roots = accounts
                        .entry((transition.public.ed25519_pub, transition.public.x25519_pub))
                        .or_default();
                    roots.insert(transition.previous_certificate.account_ed25519_pub);
                    roots.insert(transition.current_certificate.account_ed25519_pub);
                }
            }
        }
        let ambiguous: HashSet<_> = node
            .privacy
            .state
            .read()
            .expect("privacy state mutex")
            .as_ref()
            .map(|s| s.proofs.ambiguous_historical_accounts())
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.ed25519_pub, p.x25519_pub))
            .collect();
        let mut hash = Sha256::new();
        for proof in &proofs {
            hash.update(proof.ed25519_pub);
            hash.update(proof.x25519_pub);
            hash.update([
                u8::from(proof.post_office),
                u8::from(proof.valid_presence()),
                u8::from(node.known_account_allowed(&proof.public())),
            ]);
            hash.update(
                proof
                    .account_cert
                    .as_ref()
                    .map_or([0; 32], |c| c.account_ed25519_pub),
            );
        }
        for (public, roots) in &accounts {
            hash.update(public.0);
            hash.update(public.1);
            hash.update([u8::from(ambiguous.contains(public))]);
            for root in roots {
                hash.update(root);
            }
        }
        Ok(Self {
            proofs,
            accounts,
            ambiguous,
            stamp: hash.finalize().into(),
        })
    }
    pub fn principal(&self, source: &PublicIdentity, bound: Option<String>) -> String {
        bound
            .or_else(|| {
                self.accounts
                    .get(&(source.ed25519_pub, source.x25519_pub))
                    .filter(|roots| {
                        roots.len() == 1
                            && !self
                                .ambiguous
                                .contains(&(source.ed25519_pub, source.x25519_pub))
                    })
                    .and_then(|roots| roots.first().copied())
                    .map(|ed25519_pub| {
                        crate::identity::account::AccountPublic { ed25519_pub }.account_id()
                    })
            })
            .unwrap_or_else(|| source.user_id())
    }
}
impl Node {
    pub(super) fn decode_selected_reaction(
        &self,
        candidate: &store::Candidate,
        channel: bool,
        evidence: &Evidence,
        causal: Option<&crate::node::channel::ChannelMembershipProjection>,
    ) -> Result<store::Decoded, NodeError> {
        let mut decoded = store::Decoded {
            id: candidate.id,
            ordinal: candidate.ordinal,
            conversation: candidate.conversation,
            payload: None,
            source: None,
            principal: None,
            wall: 0,
            route: None,
        };
        if !channel
            && self
                .channels
                .lock()
                .expect("channels mutex")
                .state(&candidate.conversation)
                .is_some()
        {
            return Ok(decoded);
        }
        if channel && !self.channel_content_visible(candidate.conversation, candidate.id)? {
            return Ok(decoded);
        }
        let event = self
            .log
            .lock()
            .expect("log mutex")
            .get(&candidate.id)?
            .ok_or(LogError::StaleCursor)?;
        decoded.wall = event.wall_clock;
        if channel {
            let causal_source = causal.and_then(|p| p.state.as_ref()).and_then(|state| {
                state
                    .members()
                    .iter()
                    .find(|member| member.ed25519_pub == *event.author.ed25519_pub())
                    .cloned()
            });
            let causal_account = causal
                .filter(|p| p.authority.current_event().is_some())
                .and_then(|p| p.state.as_ref())
                .and_then(|state| {
                    causal_source
                        .as_ref()
                        .and_then(|member| state.member_account_id(&member.user_id()))
                })
                .map(str::to_owned);
            let source = causal_source
                .or_else(|| {
                    self.channels
                        .lock()
                        .expect("channels mutex")
                        .state(&candidate.conversation)
                        .and_then(|s| {
                            s.members()
                                .iter()
                                .find(|m| m.ed25519_pub == *event.author.ed25519_pub())
                                .cloned()
                        })
                })
                .or_else(|| {
                    evidence
                        .proofs
                        .iter()
                        .find(|p| p.ed25519_pub == *event.author.ed25519_pub())
                        .map(Announce::public)
                });
            if let Some(source) = source {
                decoded.payload = crate::node::channel::SealedPayload::decode(&event.ciphertext)
                    .and_then(|p| p.open(&self.identity, &source.x25519_pub))
                    .and_then(|p| ReactionPayload::decode(&p));
                decoded.source = Some(source);
                decoded.principal = causal_account;
            }
        } else {
            let proof = evidence.proofs.iter().find(|p| {
                !p.post_office
                    && p.public() != self.identity.public()
                    && p.ed25519_pub == *event.author.ed25519_pub()
                    && dm_conversation_id(&self.identity.public(), &p.public())
                        == candidate.conversation
                    && self.known_account_allowed(&p.public())
            });
            if let Some(proof) = proof
                && let Ok(bytes) =
                    crate::dm::open(&self.identity, &proof.x25519_pub, &event.ciphertext)
            {
                decoded.source = Some(proof.public());
                if let Some(envelope) = ReactionEnvelope::decode(&bytes) {
                    if proof.account_id().as_deref() == Some(&envelope.route.sender_account)
                        && (envelope.route.sender_account == self.account_id()
                            || envelope.route.recipient_account == self.account_id())
                    {
                        decoded.route = Some((
                            envelope.route.sender_account,
                            envelope.route.recipient_account,
                        ));
                        decoded.payload = Some(ReactionPayload {
                            target: EventId::new(envelope.target),
                            emoji: envelope.emoji,
                            remove: envelope.remove,
                        });
                    }
                } else {
                    decoded.payload = ReactionPayload::decode(&bytes);
                }
            }
        }
        Ok(decoded)
    }
}

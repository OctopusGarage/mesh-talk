//! Account-private witnesses of durable DM controls. Witness order is the original
//! action clock, never the time a sibling happens to receive an old event.
use super::*;
use crate::{
    discovery::Announce,
    eventlog::{ConversationId, Event, EventId, EventKind},
    node::{
        conversation::{account_conversation_id, dm_conversation_id},
        reaction::ReactionPayload,
    },
};

#[derive(Clone, Serialize, Deserialize)]
struct Control {
    wall: u64,
    remove: bool,
    witness: Witness,
}
#[derive(Clone, Serialize, Deserialize)]
enum Witness {
    Authored,
    Observed {
        author: Box<Announce>,
        observer: Box<Announce>,
        event: Box<Event>,
    },
    // Only the explicit retained-source publisher creates this witness, after
    // checking the retained message route and the durable verified recall cache.
    RetainedRecall {
        sender: String,
        recipient: String,
    },
}
fn parts(key: &AccountDocumentKey) -> Option<(&str, &str, &str)> {
    match key {
        AccountDocumentKey::DmReaction {
            peer_account,
            target,
            actor_account,
            ..
        }
        | AccountDocumentKey::DmRecall {
            peer_account,
            target,
            actor_account,
        } => Some((peer_account, target, actor_account)),
        _ => None,
    }
}
fn revision(key: &AccountDocumentKey, value: &Control) -> Option<u64> {
    if matches!(key, AccountDocumentKey::DmRecall { .. }) {
        Some(1)
    } else {
        value
            .wall
            .checked_mul(2)?
            .checked_add(u64::from(value.remove) + 1)
    }
}
pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Control>(payload) else {
        return false;
    };
    let Some((peer, _, actor)) = parts(&meta.key) else {
        return false;
    };
    let own = crate::identity::account::AccountPublic {
        ed25519_pub: meta.account_public,
    }
    .account_id();
    if meta.tombstone
        || !(actor == own || actor == peer)
        || revision(&meta.key, &value) != Some(meta.revision)
        || (matches!(meta.key, AccountDocumentKey::DmRecall { .. }) && !value.remove)
    {
        return false;
    }
    match &value.witness {
        Witness::Authored => actor == own,
        Witness::RetainedRecall { sender, recipient } => {
            matches!(meta.key, AccountDocumentKey::DmRecall { .. })
                && actor == sender
                && ((sender == &own && recipient == peer) || (sender == peer && recipient == &own))
        }
        Witness::Observed {
            author,
            observer,
            event,
        } => {
            author.verify()
                && observer.verify()
                && author.account_id().as_deref() == Some(actor)
                && observer.account_id().as_deref() == Some(own.as_str())
                && observer.user_id == meta.writer_device
                && event.author.ed25519_pub() == &author.ed25519_pub
                && event.verify_integrity()
                && event.verify_signature()
                && event.conversation_id == dm_conversation_id(&author.public(), &observer.public())
                && event.wall_clock == value.wall
                && event.kind
                    == if matches!(meta.key, AccountDocumentKey::DmRecall { .. }) {
                        EventKind::Delete
                    } else {
                        EventKind::React
                    }
        }
    }
}
impl Node {
    fn install_private_control(
        &self,
        key: AccountDocumentKey,
        value: Control,
    ) -> Result<(), NodeError> {
        if !key.valid() {
            return Err(invalid());
        }
        let wall = revision(&key, &value).ok_or_else(invalid)?;
        let payload = serde_json::to_vec(&value).map_err(|_| invalid())?;
        if payload.len() > key.limit() {
            return Err(invalid());
        }
        let meta =
            AccountDocumentMeta::new(&self.account, self.user_id(), key, wall, &payload, false);
        let changed = self
            .account_documents
            .lock()
            .expect("documents mutex")
            .merge(meta, &payload)
            .map_err(NodeError::Log)?;
        if changed {
            self.documents_changed();
            self.note_account_history_change();
        }
        Ok(())
    }
    pub(in crate::node) fn publish_own_account_reaction(
        &self,
        peer: &str,
        wall: u64,
        payload: &ReactionPayload,
    ) -> Result<(), NodeError> {
        self.install_private_control(
            AccountDocumentKey::DmReaction {
                peer_account: peer.into(),
                target: payload.target.to_hex(),
                actor_account: self.account_id(),
                emoji: payload.emoji.clone(),
            },
            Control {
                wall,
                remove: payload.remove,
                witness: Witness::Authored,
            },
        )
    }
    pub(in crate::node) fn publish_own_account_recall(
        &self,
        peer: &str,
        target: EventId,
        wall: u64,
    ) -> Result<(), NodeError> {
        self.install_private_control(
            AccountDocumentKey::DmRecall {
                peer_account: peer.into(),
                target: target.to_hex(),
                actor_account: self.account_id(),
            },
            Control {
                wall,
                remove: true,
                witness: Witness::Authored,
            },
        )
    }
    fn private_control_entries(
        &self,
        domain: AccountDocumentDomain,
        peer: &str,
    ) -> Vec<(AccountDocumentKey, Control)> {
        let store = self.account_documents.lock().expect("documents mutex");
        let mut after = None;
        let mut out = Vec::new();
        loop {
            let rows = store.list(domain, after.as_ref(), 64);
            if rows.is_empty() {
                break;
            }
            for meta in &rows {
                if parts(&meta.key).is_some_and(|(p, _, _)| p == peer)
                    && let Ok(Some(doc)) = store.get(&meta.key)
                    && let Ok(value) = serde_json::from_slice::<Control>(doc.payload())
                {
                    out.push((meta.key.clone(), value));
                }
            }
            after = rows.last().map(|m| m.key.clone());
        }
        out
    }
    pub(in crate::node) fn private_account_reactions(
        &self,
        peer: &str,
    ) -> Vec<(String, u64, ReactionPayload)> {
        let entries = self.private_control_entries(AccountDocumentDomain::DmReaction, peer);
        let conversation = account_conversation_id(&self.account_id(), peer);
        let suppression = self
            .account_suppression
            .lock()
            .expect("account suppression mutex");
        entries
            .into_iter()
            .filter_map(|(key, value)| {
                let AccountDocumentKey::DmReaction {
                    target,
                    actor_account,
                    emoji,
                    ..
                } = key
                else {
                    return None;
                };
                let target = EventId::new(hex::decode(target).ok()?.try_into().ok()?);
                if suppression.contains(conversation, target) {
                    return None;
                }
                Some((
                    actor_account,
                    value.wall,
                    ReactionPayload {
                        target,
                        emoji,
                        remove: value.remove,
                    },
                ))
            })
            .collect()
    }
    pub(in crate::node) fn private_dm_recall_claims(&self, peer: &str) -> Vec<(EventId, String)> {
        self.private_control_entries(AccountDocumentDomain::DmRecall, peer)
            .into_iter()
            .filter_map(|(key, _)| {
                let AccountDocumentKey::DmRecall {
                    target,
                    actor_account,
                    ..
                } = key
                else {
                    return None;
                };
                Some((
                    EventId::new(hex::decode(target).ok()?.try_into().ok()?),
                    actor_account,
                ))
            })
            .collect()
    }
    pub(in crate::node) fn private_dm_control_revision(&self) -> (u64, [u8; 32]) {
        let local = { self.recalls.lock().expect("recalls mutex").revision() };
        let canonical = {
            self.account_documents
                .lock()
                .expect("documents mutex")
                .digest(AccountDocumentDomain::DmRecall)
        };
        (local, canonical)
    }
    /// A pure test against certified control claims; target authorship is supplied
    /// by the validated account envelope, so a recall can safely precede its body.
    pub(in crate::node) fn private_dm_message_recalled(
        &self,
        peer: &str,
        target: EventId,
        author: &str,
    ) -> bool {
        let host = super::super::conversation::account_conversation_id(&self.account_id(), peer);
        let target = self.file_message_logical_id(host, target);
        self.private_dm_recall_claims(peer)
            .iter()
            .any(|(id, actor)| self.file_message_logical_id(host, *id) == target && actor == author)
    }
    pub(in crate::node) fn project_private_dm_controls(
        &self,
        conv: ConversationId,
    ) -> Result<(), NodeError> {
        let events: Vec<_> = self
            .log
            .lock()
            .expect("log mutex")
            .events(&conv)
            .into_iter()
            .filter(|e| matches!(e.kind, EventKind::React | EventKind::Delete))
            .cloned()
            .collect();
        for event in events {
            let Some(author) = self.historical_author(event.author.ed25519_pub()) else {
                continue;
            };
            if conv != dm_conversation_id(&self.identity.public(), &author.public())
                || !event.verify_integrity()
                || !event.verify_signature()
            {
                continue;
            }
            let Some(actor) = author.account_id() else {
                continue;
            };
            let Ok(plain) = crate::dm::open(&self.identity, &author.x25519_pub, &event.ciphertext)
            else {
                continue;
            };
            let (sender, recipient, target, emoji, remove) = if event.kind == EventKind::React {
                let Some(env) = crate::node::ReactionEnvelope::decode(&plain) else {
                    continue;
                };
                (
                    env.route.sender_account,
                    env.route.recipient_account,
                    EventId::new(env.target),
                    Some(env.emoji),
                    env.remove,
                )
            } else {
                let Some(env) = crate::node::RecallEnvelope::decode(&plain) else {
                    continue;
                };
                (
                    env.route.sender_account,
                    env.route.recipient_account,
                    EventId::new(env.target),
                    None,
                    true,
                )
            };
            let own = self.account_id();
            if sender != actor || (sender != own && recipient != own) {
                continue;
            }
            let peer = if sender == own { recipient } else { sender };
            let key = if let Some(emoji) = emoji {
                AccountDocumentKey::DmReaction {
                    peer_account: peer,
                    target: target.to_hex(),
                    actor_account: actor,
                    emoji,
                }
            } else {
                AccountDocumentKey::DmRecall {
                    peer_account: peer,
                    target: target.to_hex(),
                    actor_account: actor,
                }
            };
            let wall = event.wall_clock;
            self.install_private_control(
                key,
                Control {
                    wall,
                    remove,
                    witness: Witness::Observed {
                        author: Box::new(author),
                        observer: Box::new(Announce::new_with_account(
                            &self.identity,
                            &self.account,
                            "",
                            0,
                        )),
                        event: Box::new(event),
                    },
                },
            )?;
        }
        Ok(())
    }
    /// Called only by explicit KEEP SOURCE preparation, before a code is issued.
    /// Ordinary startup/joining never attributes an old local cache to a new root.
    pub fn publish_legacy_account_controls(&self) -> Result<(), NodeError> {
        let own = self.account_id();
        let mut peers = self.known_account_ids();
        peers.push(own.clone());
        peers.sort();
        peers.dedup();
        for peer in peers {
            let conv = account_conversation_id(&own, &peer);
            let rows: Vec<_> = self
                .my_dm_reactions
                .lock()
                .expect("reactions mutex")
                .iter()
                .filter(|((c, _, _), _)| *c == conv)
                .map(|(_, (wall, p))| (*wall, p.clone()))
                .collect();
            for (wall, payload) in rows {
                self.publish_own_account_reaction(&peer, wall, &payload)?;
            }
            // Validate old unscoped recall marks against a retained account route;
            // a local delete/clear never enters RecallLog and cannot be promoted.
            let marks = self.recalls.lock().expect("recalls mutex").targets(&conv);
            for target in marks {
                if let Some((sender, recipient, wall)) = self.private_dm_target_route(&peer, target)
                {
                    self.install_private_control(
                        AccountDocumentKey::DmRecall {
                            peer_account: peer.clone(),
                            target: target.to_hex(),
                            actor_account: sender.clone(),
                        },
                        Control {
                            wall,
                            remove: true,
                            witness: Witness::RetainedRecall { sender, recipient },
                        },
                    )?;
                }
            }
        }
        let convs: Vec<_> = self
            .historical_dm_peers()
            .into_iter()
            .map(|p| dm_conversation_id(&self.identity.public(), &p.public()))
            .collect();
        for conv in convs {
            self.project_private_dm_controls(conv)?;
        }
        Ok(())
    }
    pub(in crate::node) fn private_dm_target_route(
        &self,
        peer: &str,
        target: EventId,
    ) -> Option<(String, String, u64)> {
        let own = self.account_id();
        let conv = account_conversation_id(&own, peer);
        let sent = self.sentlog.lock().expect("sentlog mutex");
        for row in sent.entries_ref(&conv) {
            if let Some(env) = crate::node::DmEnvelope::decode(&row.plaintext)
                && env.msg_id == *target.as_bytes()
                && env.route.sender_account == own
                && env.route.recipient_account == peer
            {
                return Some((
                    env.route.sender_account,
                    env.route.recipient_account,
                    row.wall_clock,
                ));
            }
        }
        drop(sent);
        let received = self.received.lock().expect("received mutex");
        for row in received.entries_ref(&conv) {
            if let Some(env) = crate::node::DmEnvelope::decode(&row.plaintext)
                && env.msg_id == *target.as_bytes()
                && env.route.sender_account == row.from
                && ((env.route.sender_account == own && env.route.recipient_account == peer)
                    || (env.route.sender_account == peer && env.route.recipient_account == own))
            {
                return Some((
                    env.route.sender_account,
                    env.route.recipient_account,
                    row.wall_clock,
                ));
            }
        }
        drop(received);
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|d| d.public.user_id())
            .chain(std::iter::once(self.user_id()))
            .collect();
        let authors: std::collections::HashMap<_, _> = self
            .historical_dm_peers()
            .into_iter()
            .filter_map(|p| p.account_id().map(|a| (p.user_id, a)))
            .collect();
        let files = self.received_files.lock().expect("received files mutex");
        for row in files.entries_ref(&conv) {
            if row.event_id != target
                || !super::super::files::validated_manifest(&row.plaintext)
                    .is_some_and(|m| m.protection().is_some())
            {
                continue;
            }
            let actor = if own_devices.contains(&row.from) {
                Some(own.clone())
            } else {
                authors.get(&row.from).cloned()
            };
            if let Some(actor) = actor
                && (actor == own || actor == peer)
            {
                let recipient = if actor == own {
                    peer.to_owned()
                } else {
                    own.clone()
                };
                return Some((actor, recipient, row.wall_clock));
            }
        }
        None
    }
}

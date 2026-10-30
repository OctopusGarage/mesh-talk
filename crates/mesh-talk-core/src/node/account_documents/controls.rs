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
    ) -> Result<Vec<(String, u64, ReactionPayload)>, crate::eventlog::LogError> {
        let entries = self.private_control_entries(AccountDocumentDomain::DmReaction, peer);
        let conversation = account_conversation_id(&self.account_id(), peer);
        let suppression = self
            .account_suppression
            .lock()
            .expect("account suppression mutex");
        let mut out = Vec::new();
        for (key, value) in entries {
            let AccountDocumentKey::DmReaction {
                target,
                actor_account,
                emoji,
                ..
            } = key
            else {
                continue;
            };
            let target = EventId::new(
                hex::decode(target)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| {
                        crate::eventlog::LogError::CorruptFile("invalid reaction identity".into())
                    })?,
            );
            if !suppression.contains(conversation, target)? {
                out.push((
                    actor_account,
                    value.wall,
                    ReactionPayload {
                        target,
                        emoji,
                        remove: value.remove,
                    },
                ));
            }
        }
        Ok(out)
    }
    #[cfg(test)]
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
    pub(in crate::node) fn private_dm_recall_selected(
        &self,
        peer: &str,
        target: EventId,
        author_account: &str,
    ) -> Result<bool, crate::eventlog::LogError> {
        if author_account != self.account_id() && author_account != peer {
            return Ok(false);
        }
        let key = AccountDocumentKey::DmRecall {
            peer_account: peer.into(),
            target: target.to_hex(),
            actor_account: author_account.into(),
        };
        let store = self.account_documents.lock().expect("documents mutex");
        let Some(document) = store.get(&key)? else {
            return Ok(false);
        };
        let value: Control = serde_json::from_slice(document.payload()).map_err(|_| {
            crate::eventlog::LogError::CorruptFile("invalid selected recall control".into())
        })?;
        Ok(value.remove)
    }

    pub(in crate::node) fn private_dm_control_revision(&self) -> (u64, [u8; 32]) {
        let local = { self.recalls.lock().expect("recalls mutex").revision() };
        let canonical = {
            self.account_documents
                .lock()
                .expect("documents mutex")
                .digest(AccountDocumentDomain::DmRecall)
        };
        let mut hash = Sha256::new();
        hash.update(canonical);
        hash.update(self.personal_floor_digest());
        (local, hash.finalize().into())
    }
    /// A pure test against certified control claims; target authorship is supplied
    /// by the validated account envelope, so a recall can safely precede its body.
    pub(in crate::node) fn private_dm_message_recalled(
        &self,
        peer: &str,
        target: EventId,
        author: &str,
    ) -> Result<bool, crate::eventlog::LogError> {
        let host = account_conversation_id(&self.account_id(), peer);
        let logical = self.file_message_logical_id(host, target)?;
        self.private_dm_recall_selected(peer, logical, author)
    }
    pub(in crate::node) fn project_private_dm_controls(
        &self,
        conv: ConversationId,
    ) -> Result<(), NodeError> {
        let ids = self
            .log
            .lock()
            .expect("log mutex")
            .repository()?
            .pending_control_ids(conv, 128, 8 * 1024 * 1024)?;
        for id in ids {
            let Some(event) = self.log.lock().expect("log mutex").get(&id)? else {
                continue;
            };
            self.project_private_dm_control(event)?;
        }
        Ok(())
    }
    fn project_private_dm_control(&self, event: Event) -> Result<bool, NodeError> {
        let conv = event.conversation_id;
        let id = event.id;
        self.log
            .lock()
            .expect("log mutex")
            .repository()?
            .check_receive_event(id)?;
        let Some(author) = self.historical_author(event.author.ed25519_pub()) else {
            return Ok(false);
        };
        if conv != dm_conversation_id(&self.identity.public(), &author.public())
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return Ok(false);
        }
        let Some(actor) = author.account_id() else {
            return Ok(false);
        };
        let Ok(plain) = crate::dm::open(&self.identity, &author.x25519_pub, &event.ciphertext)
        else {
            return Ok(false);
        };
        let (sender, recipient, target, emoji, remove) = if event.kind == EventKind::React {
            let Some(env) = crate::node::ReactionEnvelope::decode(&plain) else {
                return Ok(false);
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
                return Ok(false);
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
            return Ok(false);
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
        let value = Control {
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
        };
        // Invalid authenticated peer controls must not prevent profile opening.
        // Only persistence errors abort projection; unknown keys remain retryable.
        if !key.valid()
            || revision(&key, &value).is_none()
            || serde_json::to_vec(&value).map_err(|_| invalid())?.len() > key.limit()
        {
            self.log
                .lock()
                .expect("log mutex")
                .repository()?
                .complete_control(id)?;
            return Ok(false);
        }
        self.install_private_control(key, value)?;
        self.log
            .lock()
            .expect("log mutex")
            .repository()?
            .complete_control(id)?;
        Ok(true)
    }
    pub(in crate::node) fn bootstrap_private_dm_controls(&self) -> Result<(), NodeError> {
        loop {
            let (after, through) = self
                .log
                .lock()
                .expect("log mutex")
                .repository()?
                .bootstrap_control_position()?;
            if after >= through {
                return Ok(());
            }
            let page = self
                .log
                .lock()
                .expect("log mutex")
                .repository()?
                .bootstrap_control_headers(after, through)?;
            let mut progressed = after;
            let mut bytes = 0usize;
            for row in page {
                if row.ordinal > through {
                    break;
                }
                let header = row.header;
                if row.active && matches!(header.kind, EventKind::React | EventKind::Delete) {
                    let cost = header.stored_bytes.saturating_mul(3);
                    if cost > 8 * 1024 * 1024 {
                        // Preserve the pending identity; normal retry reports
                        // its required byte budget without blocking migration.
                        progressed = row.ordinal;
                        continue;
                    }
                    if bytes.saturating_add(cost) > 8 * 1024 * 1024 {
                        break;
                    }
                    bytes += cost;
                    let event = self.log.lock().expect("log mutex").get(&header.id)?;
                    if let Some(event) = event {
                        self.project_private_dm_control(event)?;
                    }
                }
                progressed = row.ordinal;
            }
            if progressed == after {
                progressed = through;
            }
            self.log
                .lock()
                .expect("log mutex")
                .repository()?
                .advance_control_bootstrap(progressed)?;
        }
    }
    /// Called only by explicit KEEP SOURCE preparation, before a code is issued.
    /// Ordinary startup/joining never attributes an old local cache to a new root.
    pub fn publish_legacy_account_controls(&self) -> Result<(), NodeError> {
        let own = self.account_id();
        let mut peers = self.known_account_ids()?;
        peers.push(own.clone());
        peers.sort();
        peers.dedup();
        for peer in peers {
            let conv = account_conversation_id(&own, &peer);
            let mut after = None;
            loop {
                let page = self.my_dm_reactions.lock().expect("reactions mutex").page(
                    Some(conv),
                    None,
                    after,
                    128,
                    8 * 1024 * 1024,
                )?;
                if page.rows.is_empty() {
                    break;
                }
                after = page.next;
                for row in page.rows {
                    self.publish_own_account_reaction(&peer, row.wall, &row.payload)?;
                }
            }
            // Validate old unscoped recall marks against a retained account route;
            // a local delete/clear never enters RecallLog and cannot be promoted.
            let mut cursor = None;
            loop {
                let marks =
                    self.recalls
                        .lock()
                        .expect("recalls mutex")
                        .page(Some(conv), cursor, 128)?;
                if marks.is_empty() {
                    break;
                }
                cursor = marks.last().map(|m| (m.conversation, m.target));
                for mark in marks {
                    let target = mark.target;
                    if let Some((sender, recipient, wall)) =
                        self.private_dm_target_route(&peer, target)?
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
    ) -> Result<Option<(String, String, u64)>, NodeError> {
        let own = self.account_id();
        let conv = account_conversation_id(&own, peer);
        if let Some(row) = self
            .sentlog
            .lock()
            .expect("sent mutex")
            .account_entry(&conv, *target.as_bytes())?
            && let Some(env) = crate::node::DmEnvelope::decode(&row.plaintext)
            && env.route.sender_account == own
            && env.route.recipient_account == peer
        {
            return Ok(Some((
                env.route.sender_account,
                env.route.recipient_account,
                row.wall_clock,
            )));
        }
        if let Some(row) = self
            .received
            .lock()
            .expect("received mutex")
            .account_entry(&conv, target)?
            && let Some(env) = crate::node::DmEnvelope::decode(&row.plaintext)
            && env.route.sender_account == row.from
            && ((env.route.sender_account == own && env.route.recipient_account == peer)
                || (env.route.sender_account == peer && env.route.recipient_account == own))
        {
            return Ok(Some((
                env.route.sender_account,
                env.route.recipient_account,
                row.wall_clock,
            )));
        }
        let aliases = self
            .file_message_identities
            .lock()
            .expect("identity mutex")
            .aliases(conv, target)?;
        for id in aliases.into_iter().chain(std::iter::once(target)) {
            let Some(row) = self
                .received_files
                .lock()
                .expect("files mutex")
                .entry_in(&conv, id)?
            else {
                continue;
            };
            if super::super::files::validated_manifest(&row.plaintext)
                .is_none_or(|m| m.protection().is_none())
            {
                continue;
            }
            let actor = self
                .file_message_identities
                .lock()
                .expect("identity mutex")
                .row_author_account(&row)?;
            let actor = actor.or_else(|| {
                if row.from == self.user_id() {
                    Some(own.clone())
                } else {
                    self.historical_dm_peers()
                        .into_iter()
                        .find(|p| p.user_id == row.from)
                        .and_then(|p| p.account_id())
                }
            });
            if let Some(actor) = actor
                && (actor == own || actor == peer)
            {
                let recipient = if actor == own {
                    peer.to_owned()
                } else {
                    own.clone()
                };
                return Ok(Some((actor, recipient, row.wall_clock)));
            }
        }
        Ok(None)
    }
}

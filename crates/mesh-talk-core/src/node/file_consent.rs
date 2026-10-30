//! Exact file-offer decisions transported only inside encrypted pairwise mailboxes.
//! These signed controls never enter the public event projection or chat history.

use super::{Node, ReceiveDecision};
use crate::eventlog::{ConversationId, Event, EventId, EventKind, LogError};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FRAME: &[u8] = b"mesh-talk-file-consent-control-v1\0\0\0\0\0\0\0\0";
const MAX_BYTES: usize = 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Decision {
    pub file: ConversationId,
    pub manifest: EventId,
    pub sender: PublicIdentity,
    pub recipient: PublicIdentity,
    pub sender_account: Option<String>,
    pub recipient_account: String,
    pub decision: ReceiveDecision,
    /// Last original-author chunk, set only after verified durable local save.
    pub completion: Option<EventId>,
}

impl Decision {
    fn scope(&self) -> ConversationId {
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-file-consent-v1");
        hash.update(self.file.as_bytes());
        hash.update(self.manifest.as_bytes());
        hash.update(self.sender.ed25519_pub);
        hash.update(self.recipient.ed25519_pub);
        ConversationId::new(hash.finalize().into())
    }

    pub(super) fn event(
        &self,
        identity: &DeviceIdentity,
        revision: u64,
    ) -> Result<Event, LogError> {
        let mut wire = FRAME.to_vec();
        wire.extend(bincode::serialize(self).map_err(|e| LogError::Serialization(e.to_string()))?);
        Ok(Event::new(
            identity,
            self.scope(),
            revision,
            vec![],
            revision,
            0,
            EventKind::Message,
            wire,
        ))
    }

    pub(super) fn open(event: &Event) -> Option<Self> {
        let body = event.ciphertext.strip_prefix(FRAME)?;
        if event.ciphertext.len() > MAX_BYTES
            || event.seq == 0
            || event.lamport != event.seq
            || event.wall_clock != 0
            || !event.parents.is_empty()
            || event.kind != EventKind::Message
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return None;
        }
        let decision: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_BYTES as u64)
            .reject_trailing_bytes()
            .deserialize(body)
            .ok()?;
        let account_valid = |id: &str| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit());
        (decision.scope() == event.conversation_id
            && event.author.ed25519_pub() == &decision.recipient.ed25519_pub
            && decision.sender != decision.recipient
            && account_valid(&decision.recipient_account)
            && decision.sender_account.as_deref().is_none_or(account_valid))
        .then_some(decision)
    }
}

impl Node {
    pub(in crate::node) fn file_consent_events(
        &self,
        sender: &PublicIdentity,
    ) -> Result<Vec<Event>, LogError> {
        self.file_consent_events_for(sender, None)
    }

    pub(in crate::node) fn direct_file_consent_control(
        &self,
        sender: &PublicIdentity,
        file: ConversationId,
        manifest: EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(self
            .file_consent_events_for(sender, Some((file, manifest)))?
            .into_iter()
            .next())
    }

    fn file_consent_events_for(
        &self,
        sender: &PublicIdentity,
        requested: Option<(ConversationId, EventId)>,
    ) -> Result<Vec<Event>, LogError> {
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
            || (!self.known_account_allowed(sender) && !self.group_member(sender)?)
        {
            return Ok(Vec::new());
        }
        let Some(proof) = self
            .historical_author(&sender.ed25519_pub)
            .filter(|p| p.public() == *sender)
        else {
            return Ok(Vec::new());
        };
        let offers: Box<dyn Iterator<Item = Result<(ConversationId, EventId), LogError>>> =
            match requested {
                Some(pair) => Box::new(std::iter::once(Ok(pair))),
                None => Box::new(
                    self.files
                        .lock()
                        .expect("files lock")
                        .offers(
                            None,
                            Some(crate::eventlog::Author::from_ed25519(sender.ed25519_pub)),
                        )
                        .map(|offer| offer.map(|offer| (offer.file, offer.event))),
                ),
            };
        let mut controls = Vec::new();
        let scope = super::conversation::dm_conversation_id(&self.identity.public(), sender);
        for offer in offers {
            let (file, manifest) = offer?;
            let parent = self
                .log
                .lock()
                .expect("log lock")
                .event_conversation(&manifest)?;
            let group_offer = self.group_file_offer(file, manifest, sender)?;
            if !(parent == Some(scope) && self.known_account_allowed(sender))
                && group_offer.is_none()
            {
                continue;
            }
            if let Some((event, row)) = group_offer {
                self.retain_group_file_completion(&event, &row)?;
            }
            let Some((automatic, account, _, _)) =
                self.incoming_offer(file, Some(manifest), Some(sender))?
            else {
                continue;
            };
            let decision = if automatic {
                ReceiveDecision::Accepted
            } else {
                self.consent_for_file_account(self.equivalent_offer_decision(file)?, &account)
            };
            let delivery = self.delivery.lock().expect("delivery lock not poisoned");
            if self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return Ok(Vec::new());
            }
            let policy = self
                .attachment_consent
                .lock()
                .expect("consent lock not poisoned");
            let generation = policy.generation();
            let mut store = self
                .file_consent
                .lock()
                .expect("file consent lock not poisoned");
            {
                if delivery.manifest_event_erased(manifest) {
                    continue;
                }
                let completion = delivery
                    .file_card(manifest)
                    .and_then(|card| card.completion_binding.as_ref())
                    .filter(|binding| {
                        binding.owner_account == self.account_id()
                            && binding.source == *sender
                            && binding.certificate == proof.account_cert
                    })
                    .and_then(|_| delivery.file_completion(manifest));
                let payload = Decision {
                    file,
                    manifest,
                    sender: sender.clone(),
                    recipient: self.identity.public(),
                    sender_account: proof.account_id(),
                    recipient_account: self.account_id(),
                    decision,
                    completion,
                };
                let previous = store.published_control(manifest);
                if decision == ReceiveDecision::Pending && previous.is_none() && requested.is_none()
                {
                    continue;
                }
                let event = if let Some(event) = previous.filter(|event| {
                    store.published_generation(manifest) == Some(generation)
                        && Decision::open(event).as_ref() == Some(&payload)
                }) {
                    event.clone()
                } else {
                    let revision = previous
                        .map_or(Some(1), |event| event.seq.checked_add(1))
                        .ok_or_else(|| {
                            LogError::Serialization("file consent revision exhausted".into())
                        })?;
                    let event = payload.event(&self.identity, revision)?;
                    store.publish_control(event.clone(), generation)?;
                    event
                };
                controls.push(event);
            }
        }
        Ok(controls)
    }

    /// Return true for this reserved control frame, including invalid/unrelated
    /// controls, so none can be interpreted as a chat message or file chunk.
    pub(in crate::node) fn accept_file_consent_event(
        &self,
        event: &Event,
        peer: &PublicIdentity,
    ) -> Result<bool, LogError> {
        self.accept_file_consent_control(event, peer, false)
    }

    fn accept_file_consent_control(
        &self,
        event: &Event,
        peer: &PublicIdentity,
        direct: bool,
    ) -> Result<bool, LogError> {
        if !event.ciphertext.starts_with(FRAME) {
            return Ok(false);
        }
        let Some(payload) = Decision::open(event) else {
            return Ok(true);
        };
        if payload.sender != self.identity.public()
            || payload.recipient != *peer
            || payload
                .sender_account
                .as_deref()
                .is_some_and(|id| id != self.account_id())
            || (!self.known_account_allowed(peer)
                && self
                    .group_file_offer(payload.file, payload.manifest, peer)?
                    .is_none())
            || self.historical_author(&peer.ed25519_pub).is_none_or(|p| {
                p.public() != *peer
                    || p.account_id()
                        .is_some_and(|id| id != payload.recipient_account)
            })
        {
            return Ok(true);
        }
        if let Some((original, row)) =
            self.group_file_offer(payload.file, payload.manifest, peer)?
        {
            let invalid_completion = if let Some(id) = payload.completion {
                let log = self.log.lock().expect("log lock");
                let chunks = log.events(&payload.file)?;
                let expected = super::files::validated_manifest(&row.plaintext)
                    .map(|m| m.chunk_count() as usize);
                Some(chunks.len()) != expected || chunks.last().map(|event| event.id) != Some(id)
            } else {
                false
            };
            if original.author.ed25519_pub() != &self.identity.public().ed25519_pub
                || invalid_completion
            {
                return Ok(true);
            }
            let accepted = {
                let mut store = self.file_consent.lock().expect("file consent lock");
                if direct {
                    store.receive_direct_control(event.clone())?
                } else {
                    store.receive_control(event.clone())?
                }
            };
            if accepted {
                self.file_pull_notify.notify_one();
                self.note_file_change();
            }
            return Ok(true);
        }
        let delivery = self.delivery.lock().expect("delivery lock not poisoned");
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(true);
        }
        let card = delivery
            .cards_for_file(payload.file)
            .into_iter()
            .find(|card| {
                card.destinations.iter().any(|d| {
                    d.binding.event_id == payload.manifest
                        && d.binding.device == *peer
                        && d.binding
                            .account
                            .as_ref()
                            .is_none_or(|id| id == &payload.recipient_account)
                })
            })
            .filter(|card| {
                payload
                    .completion
                    .is_none_or(|chunk| card.final_chunk == Some(chunk))
            })
            .map(|card| card.id);
        let Some(card) = card else { return Ok(true) };
        let accepted = {
            let mut store = self.file_consent.lock().expect("consent lock not poisoned");
            if direct {
                store.receive_direct_control(event.clone())?
            } else {
                store.receive_control(event.clone())?
            }
        };
        drop(delivery);
        if accepted {
            self.outgoing_file_consents
                .lock()
                .expect("outgoing consent lock not poisoned")
                .insert((card, payload.manifest), status(payload.decision));
            self.delivery_notify.notify_one();
            self.note_file_change();
        }
        Ok(true)
    }

    /// Prefer an ordered signed answer. Legacy endpoints reconnect before their
    /// original query, whose restrictive answers remain authoritative until a
    /// causally fresh direct answer is obtained.
    pub(in crate::node) async fn query_file_consent(
        &self,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        peer: &PublicIdentity,
        card: EventId,
        file: ConversationId,
        manifest: EventId,
    ) -> Result<crate::eventlog::sync::FileConsentStatus, super::session::SessionError> {
        use super::session::{self, SessionError};
        let pending = crate::eventlog::sync::FileConsentStatus::Pending;
        match session::request_signed_file_consent(channel, file, manifest).await {
            Ok(Some(event)) => {
                let Some(payload) = Decision::open(&event).filter(|p| {
                    p.file == file
                        && p.manifest == manifest
                        && p.sender == self.identity.public()
                        && p.recipient == *peer
                }) else {
                    return Ok(pending);
                };
                if self
                    .accept_file_consent_control(&event, peer, true)
                    .is_err()
                {
                    return Ok(pending);
                }
                // Use the validated durable decision, never an unadmitted payload.
                let answer = self
                    .outgoing_file_consent_status(card, manifest)
                    .unwrap_or(pending);
                if answer != status(payload.decision) {
                    return Ok(pending);
                }
                return Ok(answer);
            }
            Ok(None) => {}
            Err(_) => {
                let route = self
                    .routing_peer(&peer.user_id())
                    .filter(|route| route.public == *peer)
                    .ok_or(SessionError::UnexpectedMessage)?;
                *channel = self.privacy_dial(route.addr, peer).await?;
            }
        }
        let answer = session::request_file_consent(channel, file, manifest).await?;
        if self
            .record_outgoing_file_consent(card, manifest, answer)
            .is_err()
        {
            return Ok(pending);
        }
        Ok(answer)
    }

    pub(in crate::node) fn record_outgoing_file_consent(
        &self,
        card: EventId,
        manifest: EventId,
        answer: crate::eventlog::sync::FileConsentStatus,
    ) -> Result<(), LogError> {
        let decision = match answer {
            crate::eventlog::sync::FileConsentStatus::Pending => ReceiveDecision::Pending,
            crate::eventlog::sync::FileConsentStatus::Accepted => ReceiveDecision::Accepted,
            crate::eventlog::sync::FileConsentStatus::Declined => ReceiveDecision::Declined,
        };
        self.file_consent
            .lock()
            .expect("consent lock not poisoned")
            .observe_remote_decision(manifest, decision)?;
        let previous = self
            .outgoing_file_consents
            .lock()
            .expect("outgoing consent lock not poisoned")
            .insert((card, manifest), answer);
        if previous != Some(answer) {
            self.note_file_change();
            self.delivery_notify.notify_one();
        }
        Ok(())
    }

    pub(in crate::node) fn outgoing_file_consent_status(
        &self,
        card: EventId,
        manifest: EventId,
    ) -> Option<crate::eventlog::sync::FileConsentStatus> {
        let durable = self
            .file_consent
            .lock()
            .expect("consent lock not poisoned")
            .remote_decision(manifest)
            .map(status);
        durable.or_else(|| {
            self.outgoing_file_consents
                .lock()
                .expect("outgoing consent lock not poisoned")
                .get(&(card, manifest))
                .copied()
        })
    }

    pub(in crate::node) fn recipient_completed_file(
        &self,
        file: &super::delivery_store::FileCard,
        destination: &super::delivery_store::FileDestination,
    ) -> bool {
        let binding = &destination.binding;
        let control = self
            .file_consent
            .lock()
            .expect("file consent lock not poisoned")
            .received_control(binding.event_id)
            .cloned();
        control
            .as_ref()
            .and_then(Decision::open)
            .is_some_and(|payload| {
                payload.file == file.file_conversation
                    && payload.manifest == binding.event_id
                    && payload.sender == self.identity.public()
                    && payload.recipient == binding.device
                    && payload
                        .sender_account
                        .as_deref()
                        .is_none_or(|id| id == self.account_id())
                    && binding
                        .account
                        .as_ref()
                        .is_none_or(|id| id == &payload.recipient_account)
                    && payload.completion.is_some()
                    && payload.completion == file.final_chunk
            })
    }
}

fn status(decision: ReceiveDecision) -> crate::eventlog::sync::FileConsentStatus {
    match decision {
        ReceiveDecision::Pending => crate::eventlog::sync::FileConsentStatus::Pending,
        ReceiveDecision::Accepted => crate::eventlog::sync::FileConsentStatus::Accepted,
        ReceiveDecision::Declined => crate::eventlog::sync::FileConsentStatus::Declined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::file_consent_store::FileConsentStore;

    fn payload(recipient: &DeviceIdentity) -> Decision {
        Decision {
            file: ConversationId::new([1; 32]),
            manifest: EventId::new([2; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: recipient.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        }
    }

    #[test]
    fn signed_consent_rejects_another_author_or_scope_and_allows_revocation_to_pending() {
        let recipient = DeviceIdentity::generate();
        let mut payload = payload(&recipient);
        let accepted = payload.event(&recipient, 1).unwrap();
        assert!(Decision::open(&accepted).is_some());
        let wrong_author = payload.event(&DeviceIdentity::generate(), 1).unwrap();
        assert!(Decision::open(&wrong_author).is_none());
        let wrong_scope = Event::new(
            &recipient,
            ConversationId::new([9; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            accepted.ciphertext.clone(),
        );
        assert!(Decision::open(&wrong_scope).is_none());
        payload.decision = ReceiveDecision::Pending;
        assert!(
            Decision::open(&payload.event(&recipient, 2).unwrap()).is_some(),
            "turning off automatic receive must revoke an earlier automatic acceptance"
        );
    }

    #[test]
    fn decline_revision_survives_compaction_restart_and_delayed_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let recipient = DeviceIdentity::generate();
        let mut payload = payload(&recipient);
        let accepted = payload.event(&recipient, 1).unwrap();
        payload.decision = ReceiveDecision::Declined;
        let declined = payload.event(&recipient, 2).unwrap();
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(store.receive_control(accepted.clone()).unwrap());
        assert!(store.receive_control(declined.clone()).unwrap());
        assert!(!store.receive_control(accepted.clone()).unwrap());
        drop(store);
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(!store.receive_control(accepted).unwrap());
        assert_eq!(
            store.received_control(payload.manifest).unwrap().id,
            declined.id
        );
    }
}

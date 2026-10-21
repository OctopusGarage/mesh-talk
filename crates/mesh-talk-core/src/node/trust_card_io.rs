//! Admission, durable publication and independent signed evidence for trust cards.
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::node::profile::ProfilePayload;

impl Node {
    pub fn trust_card(&self, account_id: &str) -> Option<TrustCard> {
        self.trust_cards
            .lock()
            .expect("trust cards mutex not poisoned")
            .get(account_id)
    }

    /// Called only by a creation path which knows it just generated this account.
    pub fn initialize_new_trust_card(
        &self,
        display_name: String,
        created_at_ms: u64,
    ) -> Result<(), NodeError> {
        if display_name.len() > 256 {
            return Err(NodeError::InvalidInput(
                "display name exceeds 256 bytes".into(),
            ));
        }
        let mut store = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned");
        if store.get(&self.account_id()).is_some() {
            return Ok(());
        }
        let card = TrustCard::new(
            &self.account,
            super::node::now_millis().max(1),
            display_name,
            String::new(),
            Some(created_at_ms),
            self.account_profiles.avatar(&self.account_id()),
        );
        if !store.record(card).map_err(NodeError::Log)? {
            return Err(NodeError::InvalidInput("invalid initial trust card".into()));
        }
        Ok(())
    }

    pub(super) fn prepare_trust_card_delivery(
        &self,
        peer: &PeerRecord,
        card: &TrustCard,
    ) -> Option<crate::eventlog::event::ConversationId> {
        // Keep legacy avatars readable, but causally require the signed card first.
        // Callers hold the publication mutex across both synchronous appends.
        let legacy = self
            .account_profiles
            .current()
            .filter(|p| p.updated_at > card.revision)
            .unwrap_or_else(|| {
                ProfilePayload::new(&self.account, card.avatar.clone(), card.revision)
            });
        let (conv, card_event) =
            self.prepare_profile_delivery_with_parent(peer, &card.encode(), None)?;
        self.prepare_profile_delivery_with_parent(peer, &legacy.encode(), Some(card_event))?;
        Some(conv)
    }

    pub(super) fn prepare_current_profile_delivery(
        &self,
        peer: &PeerRecord,
    ) -> Option<crate::eventlog::event::ConversationId> {
        let store = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned");
        if let Some(card) = store.get(&self.account_id()) {
            return self.prepare_trust_card_delivery(peer, &card);
        }
        let payload = self.account_profiles.current()?;
        self.prepare_profile_delivery(peer, &payload.encode())
    }

    async fn edit_trust_card_if(
        &self,
        edit: impl Fn(&mut TrustCard) -> Result<(), NodeError> + Send,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let mut deliveries = Vec::new();
        authorize(&mut || {
            let mut store = self
                .trust_cards
                .lock()
                .expect("trust cards mutex not poisoned");
            let mut card = store.get(&self.account_id()).unwrap_or_else(|| {
                TrustCard::new(
                    &self.account,
                    0,
                    String::new(),
                    String::new(),
                    None,
                    self.account_profiles.avatar(&self.account_id()),
                )
            });
            let previous_creation = card.created_at_ms;
            edit(&mut card)?;
            if previous_creation.is_some() && previous_creation != card.created_at_ms {
                return Err(NodeError::InvalidInput(
                    "the initialized creation claim cannot change".into(),
                ));
            }
            let last = self
                .account_profiles
                .current()
                .map_or(card.revision, |p| p.updated_at.max(card.revision));
            card.revision = last
                .checked_add(1)
                .ok_or_else(|| NodeError::InvalidInput("trust card revision exhausted".into()))?
                .max(super::node::now_millis());
            card.resign(&self.account);
            if !card.verify() || !store.record(card.clone()).map_err(NodeError::Log)? {
                return Err(NodeError::InvalidInput(
                    "invalid or over-budget trust card".into(),
                ));
            }
            self.account_profiles
                .receive_trust_card(&card)
                .map_err(NodeError::Log)?;
            for peer in self.profile_fanout_targets() {
                if let Some(conv) = self.prepare_trust_card_delivery(&peer, &card) {
                    deliveries.push((peer, conv));
                }
            }
            Ok(())
        })?;
        for (peer, conv) in deliveries {
            let _ = self.deliver_direct(&peer, conv).await;
            let _ = self.replicate_to_post_office(conv).await;
        }
        Ok(())
    }

    pub async fn publish_trust_card_if(
        &self,
        display_name: String,
        bio: String,
        created_at_ms: Option<u64>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        if display_name.len() > 256 || bio.len() > 2048 {
            return Err(NodeError::InvalidInput(
                "trust card text exceeds byte limits".into(),
            ));
        }
        self.edit_trust_card_if(
            move |card| {
                card.display_name = display_name.clone();
                card.bio = bio.clone();
                card.created_at_ms = created_at_ms;
                Ok(())
            },
            authorize,
        )
        .await
    }

    pub async fn attach_introduction_if(
        &self,
        introduction: Introduction,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        self.edit_trust_card_if(
            move |card| {
                if !introduction.verify() || introduction.subject_account_id != card.account_id() {
                    return Err(NodeError::InvalidInput(
                        "invalid introduction subject or signature".into(),
                    ));
                }
                card.introductions
                    .retain(|p| p.issuer_ed25519_pub != introduction.issuer_ed25519_pub);
                card.introductions.push(introduction.clone());
                Ok(())
            },
            authorize,
        )
        .await
    }

    pub async fn attach_timestamp_witness_if(
        &self,
        witness: TimestampWitness,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        self.edit_trust_card_if(
            move |card| {
                if !witness.verify()
                    || witness.subject_account_id != card.account_id()
                    || Some(witness.created_at_ms) != card.created_at_ms
                {
                    return Err(NodeError::InvalidInput(
                        "invalid witness subject, creation claim or signature".into(),
                    ));
                }
                card.witnesses
                    .retain(|p| p.issuer_ed25519_pub != witness.issuer_ed25519_pub);
                card.witnesses.push(witness.clone());
                Ok(())
            },
            authorize,
        )
        .await
    }

    pub fn create_introduction(
        &self,
        subject_account_id: &str,
        note: String,
    ) -> Result<Introduction, NodeError> {
        if subject_account_id.len() != 32 || note.len() > 2048 {
            return Err(NodeError::InvalidInput(
                "introduction exceeds byte limits".into(),
            ));
        }
        let proof = Introduction::new(
            &self.account,
            subject_account_id.into(),
            note,
            super::node::now_millis(),
        );
        if !proof.verify() {
            return Err(NodeError::InvalidInput("invalid introduction".into()));
        }
        Ok(proof)
    }

    pub fn witness_creation(
        &self,
        subject_account_id: &str,
    ) -> Result<TimestampWitness, NodeError> {
        let card = self
            .trust_card(subject_account_id)
            .ok_or_else(|| NodeError::InvalidInput("subject trust card unknown".into()))?;
        let created_at_ms = card
            .created_at_ms
            .ok_or_else(|| NodeError::InvalidInput("subject creation claim unknown".into()))?;
        let proof = TimestampWitness::new(
            &self.account,
            subject_account_id.into(),
            created_at_ms,
            super::node::now_millis(),
        );
        if !proof.verify() {
            return Err(NodeError::InvalidInput("invalid creation witness".into()));
        }
        Ok(proof)
    }

    pub async fn import_trust_card_if(
        &self,
        card: TrustCard,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        authorize(&mut || {
            let applied = self.receive_trust_card(&card)?;
            if !applied && self.trust_card(&card.account_id()).as_ref() != Some(&card) {
                return Err(NodeError::InvalidInput(
                    "stale, incompatible or over-budget trust card".into(),
                ));
            }
            Ok(())
        })
    }

    pub(super) fn receive_trust_card(&self, card: &TrustCard) -> Result<bool, NodeError> {
        if !card.verify() {
            return Err(NodeError::InvalidInput(
                "invalid trust card signature or bounds".into(),
            ));
        }
        let mut store = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned");
        let applied = store.record(card.clone()).map_err(NodeError::Log)?;
        let is_current = store.get(&card.account_id()).as_ref() == Some(card);
        if applied || is_current {
            let projected = self
                .account_profiles
                .receive_trust_card(card)
                .map_err(NodeError::Log)?;
            if applied || projected {
                let _ = self.profile_incoming.send(ReceivedProfile {
                    account_id: card.account_id(),
                    avatar: self.account_profiles.avatar(&card.account_id()),
                });
            }
        }
        Ok(applied)
    }

    pub(super) fn receive_legacy_profile(
        &self,
        payload: &ProfilePayload,
    ) -> Result<bool, NodeError> {
        let mut store = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned");
        let applied = self
            .account_profiles
            .receive(payload)
            .map_err(NodeError::Log)?;
        if payload.account_id() == self.account_id() {
            self.update_trust_avatar(&mut store, self.account_profiles.avatar(&self.account_id()))?;
        }
        Ok(applied)
    }

    pub(super) fn reconcile_trust_profiles_on_open(&self) -> Result<(), NodeError> {
        let mut store = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned");
        for card in store.all() {
            self.account_profiles
                .receive_trust_card(&card)
                .map_err(NodeError::Log)?;
        }
        self.update_trust_avatar(&mut store, self.account_profiles.avatar(&self.account_id()))?;
        Ok(())
    }

    /// Called under the shared publication lock after accepting an avatar edit.
    pub(super) fn update_trust_avatar(
        &self,
        store: &mut super::trust_card::TrustCardStore,
        avatar: Option<Vec<u8>>,
    ) -> Result<Option<TrustCard>, NodeError> {
        let Some(mut card) = store.get(&self.account_id()) else {
            return Ok(None);
        };
        if card.avatar == avatar {
            self.account_profiles
                .receive_trust_card(&card)
                .map_err(NodeError::Log)?;
            return Ok(Some(card));
        }
        card.avatar = avatar;
        let last = self
            .account_profiles
            .current()
            .map_or(card.revision, |p| p.updated_at.max(card.revision));
        card.revision = last
            .checked_add(1)
            // A legacy avatar at the terminal revision can still advance an older
            // card to that revision; further edits remain exhausted.
            .or_else(|| (card.revision < last).then_some(last))
            .ok_or_else(|| NodeError::InvalidInput("trust card revision exhausted".into()))?
            .max(super::node::now_millis());
        card.resign(&self.account);
        if !store.record(card.clone()).map_err(NodeError::Log)? {
            return Err(NodeError::InvalidInput("trust card update rejected".into()));
        }
        self.account_profiles
            .receive_trust_card(&card)
            .map_err(NodeError::Log)?;
        Ok(Some(card))
    }
}

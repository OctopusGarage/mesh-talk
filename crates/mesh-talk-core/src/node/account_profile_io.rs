//! Durable ordinary profiles and automatic authenticated publication.
use super::account_profile_data::AccountProfileStore;
use super::*;
use crate::discovery::roster::PeerRecord;
use crate::node::profile::ProfilePayload;

impl Node {
    pub fn account_profile(&self, account_id: &str) -> Option<AccountProfile> {
        #[cfg(test)]
        if let Some(hook) = self.profile_read_hook.lock().unwrap().take() {
            hook();
        }
        self.profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .get(account_id)
    }

    /// Frozen compatibility records are readable for old events and backups only.
    pub fn trust_card(&self, account_id: &str) -> Option<TrustCard> {
        self.trust_cards
            .lock()
            .expect("legacy cards mutex not poisoned")
            .get(account_id)
    }

    pub(super) fn prepare_account_profile_delivery(
        &self,
        peer: &PeerRecord,
        profile: &AccountProfile,
    ) -> Option<crate::eventlog::event::ConversationId> {
        let legacy = self
            .account_profiles
            .current()
            .filter(|value| value.updated_at > profile.revision)
            .unwrap_or_else(|| {
                ProfilePayload::new(&self.account, profile.avatar.clone(), profile.revision)
            });
        let (conversation, parent) =
            self.prepare_profile_delivery_with_parent(peer, &profile.encode(), None)?;
        self.prepare_profile_delivery_with_parent(peer, &legacy.encode(), Some(parent))?;
        Some(conversation)
    }

    pub(super) fn prepare_current_profile_delivery(
        &self,
        peer: &PeerRecord,
    ) -> Option<crate::eventlog::event::ConversationId> {
        // Delivery acquires channel state; retain only an owned profile snapshot.
        let profile = self.account_profile(&self.account_id());
        if let Some(profile) = profile {
            return self.prepare_account_profile_delivery(peer, &profile);
        }
        self.prepare_profile_delivery(peer, &self.account_profiles.current()?.encode())
    }

    /// Called under the publication lock. The profile is the durable intent;
    /// projection failures are repairable on retry/reopen before fanout resumes.
    fn save_profile_local(
        &self,
        store: &mut AccountProfileStore,
        display_name: String,
        bio: String,
    ) -> Result<AccountProfile, NodeError> {
        if display_name.trim().is_empty()
            || display_name.len() > 256
            || bio.len() > super::account_profile_data::MAX_BIO_BYTES
        {
            return Err(NodeError::InvalidInput(
                "profile text exceeds limits or has an empty name".into(),
            ));
        }
        let old = store.get(&self.account_id());
        if let Some(profile) = &old {
            self.account_profiles
                .receive_account_profile(profile)
                .map_err(NodeError::Log)?;
        }
        let avatar = self.account_profiles.avatar(&self.account_id());
        if let Some(profile) = &old
            && profile.display_name == display_name
            && profile.bio == bio
            && profile.avatar == avatar
            && profile.created_at_ms == self.account.created_at_ms()
        {
            self.account_profiles
                .receive_account_profile(profile)
                .map_err(NodeError::Log)?;
            return Ok(profile.clone());
        }
        let last = old.as_ref().map_or(0, |value| value.revision).max(
            self.account_profiles
                .current()
                .map_or(0, |value| value.updated_at),
        );
        let revision = last
            .checked_add(1)
            .ok_or_else(|| NodeError::InvalidInput("account profile revision exhausted".into()))?
            .max(super::node::now_millis());
        let profile = AccountProfile::new(&self.account, revision, display_name, bio, avatar);
        if !store.record(profile.clone()).map_err(NodeError::Log)? {
            return Err(NodeError::InvalidInput(
                "incompatible or over-budget account profile".into(),
            ));
        }
        self.account_profiles
            .receive_account_profile(&profile)
            .map_err(NodeError::Log)?;
        let _ = self.profile_incoming.send(ReceivedProfile {
            account_id: self.account_id(),
            avatar: profile.avatar.clone(),
        });
        Ok(profile)
    }

    /// Keep the ordinary nickname and the signed profile together. Networking is
    /// picked up by the runtime's bounded revision-based publication scheduler.
    pub fn set_profile_display_name(&self, display_name: &str) -> Result<(), NodeError> {
        let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
        let bio = store
            .get(&self.account_id())
            .map(|profile| profile.bio)
            .or_else(|| self.trust_card(&self.account_id()).map(|card| card.bio))
            .unwrap_or_default();
        self.save_profile_local(&mut store, display_name.to_string(), bio)?;
        Ok(())
    }

    /// Compatibility entry point for authorized desktop nickname edits. The
    /// ordinary account-profile owner retains biography, avatar and metadata.
    pub fn set_account_display_name(&self, display_name: &str) -> Result<(), NodeError> {
        if crate::identity::user::User::validate_display_name(display_name).as_deref()
            != Some(display_name)
        {
            return Err(NodeError::InvalidInput("invalid display name".into()));
        }
        self.set_profile_display_name(display_name)
    }

    /// Biography edits derive the latest nickname inside the publication mutex;
    /// a concurrent ordinary rename cannot be overwritten by a stale UI read.
    pub async fn save_account_bio_if(
        &self,
        bio: String,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        self.edit_account_profile_if(None, None, bio, authorize)
            .await
    }

    /// An explicit profile edit may initialize a missing legacy profile using the
    /// host's current account nickname. Existing signed names always win at commit.
    pub async fn save_account_bio_with_initial_name_if(
        &self,
        initial_display_name: String,
        bio: String,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        self.edit_account_profile_if(None, Some(initial_display_name), bio, authorize)
            .await
    }

    /// Timestamps and proof lists are intentionally absent from the edit API.
    pub async fn save_account_profile_if(
        &self,
        display_name: String,
        bio: String,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        self.edit_account_profile_if(Some(display_name), None, bio, authorize)
            .await
    }

    async fn edit_account_profile_if(
        &self,
        display_name: Option<String>,
        initial_display_name: Option<String>,
        bio: String,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<(), NodeError> {
        let mut deliveries = Vec::new();
        authorize(&mut || {
            let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
            let name = display_name
                .clone()
                .or_else(|| {
                    store
                        .get(&self.account_id())
                        .map(|profile| profile.display_name)
                })
                .or_else(|| initial_display_name.clone())
                .ok_or_else(|| {
                    NodeError::InvalidInput("account profile is not initialized".into())
                })?;
            let profile = self.save_profile_local(&mut store, name, bio.clone())?;
            drop(store);
            for peer in self.profile_fanout_targets() {
                if let Some(conversation) = self.prepare_account_profile_delivery(&peer, &profile) {
                    deliveries.push((peer, conversation));
                }
            }
            Ok(())
        })?;
        for (peer, conversation) in deliveries {
            let _ = self.deliver_direct(&peer, conversation).await;
            let _ = self.replicate_to_post_office(conversation).await;
        }
        Ok(())
    }

    pub(super) fn receive_account_profile(
        &self,
        profile: &AccountProfile,
    ) -> Result<bool, NodeError> {
        if !profile.verify() {
            return Err(NodeError::InvalidInput("invalid account profile".into()));
        }
        if profile.account_id() == self.account_id()
            && self.account.created_at_ms() != profile.created_at_ms
        {
            return Ok(false);
        }
        let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
        let applied = store.record(profile.clone()).map_err(NodeError::Log)?;
        if applied || store.get(&profile.account_id()).as_ref() == Some(profile) {
            let projected = self
                .account_profiles
                .receive_account_profile(profile)
                .map_err(NodeError::Log)?;
            if applied || projected {
                let _ = self.profile_incoming.send(ReceivedProfile {
                    account_id: profile.account_id(),
                    avatar: self.account_profiles.avatar(&profile.account_id()),
                });
            }
        }
        Ok(applied)
    }

    pub(super) fn receive_trust_card(&self, card: &TrustCard) -> Result<bool, NodeError> {
        let mut store = self
            .trust_cards
            .lock()
            .expect("legacy cards mutex not poisoned");
        let applied = store.record(card.clone()).map_err(NodeError::Log)?;
        let current = store.get(&card.account_id()).as_ref() == Some(card);
        drop(store);
        if applied || current {
            let mut profiles = self.profiles.lock().expect("profiles mutex not poisoned");
            let projected = self
                .account_profiles
                .receive_trust_card(card)
                .map_err(NodeError::Log)?;
            if card.account_id() == self.account_id() {
                self.update_profile_avatar(
                    &mut profiles,
                    self.account_profiles.avatar(&self.account_id()),
                )?;
            }
            if applied || projected {
                let _ = self.profile_incoming.send(ReceivedProfile {
                    account_id: card.account_id(),
                    avatar: self.account_profiles.avatar(&card.account_id()),
                });
            }
        }
        // Legacy creation claims and proofs never become new account metadata.
        Ok(applied)
    }

    pub(super) fn receive_legacy_profile(
        &self,
        payload: &ProfilePayload,
    ) -> Result<bool, NodeError> {
        let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
        let applied = self
            .account_profiles
            .receive(payload)
            .map_err(NodeError::Log)?;
        if payload.account_id() == self.account_id() {
            self.update_profile_avatar(
                &mut store,
                self.account_profiles.avatar(&self.account_id()),
            )?;
        }
        Ok(applied)
    }

    pub(super) fn reconcile_account_profiles_on_open(&self) -> Result<(), NodeError> {
        // Read old records solely to preserve avatars. New dates are never derived
        // from old cards, login time, file timestamps, or backup creation time.
        for card in self
            .trust_cards
            .lock()
            .expect("legacy cards mutex not poisoned")
            .all()
        {
            self.account_profiles
                .receive_trust_card(&card)
                .map_err(NodeError::Log)?;
        }
        let mut store = self.profiles.lock().expect("profiles mutex not poisoned");
        for profile in store.all() {
            self.account_profiles
                .receive_account_profile(&profile)
                .map_err(NodeError::Log)?;
        }
        self.update_profile_avatar(&mut store, self.account_profiles.avatar(&self.account_id()))?;
        Ok(())
    }

    pub(super) fn update_profile_avatar(
        &self,
        store: &mut AccountProfileStore,
        avatar: Option<Vec<u8>>,
    ) -> Result<Option<AccountProfile>, NodeError> {
        let Some(mut profile) = store.get(&self.account_id()) else {
            return Ok(None);
        };
        if profile.avatar != avatar {
            let last = profile.revision.max(
                self.account_profiles
                    .current()
                    .map_or(0, |value| value.updated_at),
            );
            profile.revision = last
                .checked_add(1)
                .or_else(|| (profile.revision < last).then_some(last))
                .ok_or_else(|| {
                    NodeError::InvalidInput("account profile revision exhausted".into())
                })?
                .max(super::node::now_millis());
            profile.avatar = avatar;
            profile.resign(&self.account);
            if !store.record(profile.clone()).map_err(NodeError::Log)? {
                return Err(NodeError::InvalidInput(
                    "account profile could not be saved".into(),
                ));
            }
        }
        self.account_profiles
            .receive_account_profile(&profile)
            .map_err(NodeError::Log)?;
        Ok(Some(profile))
    }
}

#[cfg(test)]
#[path = "account_profile_io_tests.rs"]
mod tests;

//! Account-owned language preference and endpoint presentation decisions.
use super::{
    Node, NodeError,
    language_policy::{self, LanguageVerdict},
    profile::ProfilePayload,
};
use crate::eventlog::EventId;

pub(super) const HIDDEN_TEXT: &[u8] = b"[Message hidden by your language setting]";

impl Node {
    pub fn dm_language_policy(&self, account: &str) -> Option<String> {
        self.language_state
            .lock()
            .expect("language state mutex not poisoned")
            .profile(account)
            .and_then(|profile| profile.language)
    }

    pub fn validate_dm_language(
        &self,
        account: &str,
        text: &[u8],
    ) -> Result<LanguageVerdict, NodeError> {
        let Some(language) = self.dm_language_policy(account) else {
            return Ok(LanguageVerdict {
                allowed: true,
                target_percent: 100,
                language: String::new(),
                reason: None,
            });
        };
        let text = std::str::from_utf8(text)
            .map_err(|_| NodeError::InvalidInput("message is not UTF-8".into()))?;
        Ok(language_policy::evaluate(&language, text))
    }

    pub(super) fn check_dm_language(&self, account: &str, text: &[u8]) -> Result<(), NodeError> {
        let verdict = self.validate_dm_language(account, text)?;
        if verdict.allowed {
            Ok(())
        } else {
            Err(NodeError::InvalidInput(format!(
                "Recipient requires at least {}% {} text (estimated {}%). Please revise your message.",
                language_policy::MIN_TARGET_PERCENT,
                verdict.language,
                verdict.target_percent
            )))
        }
    }

    pub(super) fn authorize_dm_language(
        &self,
        account: &str,
        text: &[u8],
        accept: &mut dyn FnMut() -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let state = self
            .language_state
            .lock()
            .expect("language state mutex not poisoned");
        if let Some(language) = state.profile(account).and_then(|profile| profile.language) {
            let text = std::str::from_utf8(text)
                .map_err(|_| NodeError::InvalidInput("message is not UTF-8".into()))?;
            if !language_policy::evaluate(&language, text).allowed {
                return Err(NodeError::InvalidInput(format!(
                    "Recipient requires at least {}% {language} text. Please revise your message.",
                    language_policy::MIN_TARGET_PERCENT
                )));
            }
        }
        // Keep the preference stable through the synchronous WAL acceptance.
        accept()
    }

    pub async fn set_dm_language_policy(&self, language: Option<String>) -> Result<(), NodeError> {
        self.set_dm_language_policy_if(language, |accept| accept())
            .await
    }

    pub async fn set_dm_language_policy_if(
        &self,
        language: Option<String>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        if language
            .as_deref()
            .is_some_and(|code| !language_policy::is_supported(code))
        {
            return Err(NodeError::InvalidInput(
                "unsupported message language".into(),
            ));
        }
        let mut payload = None;
        authorize(&mut || {
            let avatar = self
                .profiles
                .lock()
                .expect("profiles mutex not poisoned")
                .avatar(&self.account_id());
            payload = Some(self.prepare_own_profile(avatar, Some(language.clone()))?);
            Ok(())
        })?;
        if let Some(payload) = payload {
            self.publish_profile_to_peers(&payload).await;
        }
        Ok(())
    }

    pub(super) fn prepare_own_profile(
        &self,
        avatar: Option<Vec<u8>>,
        language: Option<Option<String>>,
    ) -> Result<ProfilePayload, NodeError> {
        // One lock order for all own updates: current profile, language state, avatars.
        let mut current = self
            .my_profile
            .lock()
            .expect("my_profile mutex not poisoned");
        let mut state = self
            .language_state
            .lock()
            .expect("language state mutex not poisoned");
        let mut avatars = self.profiles.lock().expect("profiles mutex not poisoned");
        let account = self.account_id();
        let previous = state.profile(&account);
        let modern = language.is_some() || previous.is_some();
        let language =
            language.unwrap_or_else(|| previous.as_ref().and_then(|p| p.language.clone()));
        let revision = super::node::now_millis().max(
            avatars
                .updated_at(&account)
                .unwrap_or(0)
                .max(previous.as_ref().map_or(0, |p| p.updated_at))
                .saturating_add(1),
        );
        let payload = if modern {
            ProfilePayload::new_with_language(&self.account, avatar.clone(), revision, language)
        } else {
            ProfilePayload::new(&self.account, avatar.clone(), revision)
        };
        if modern {
            state.record_profile(&payload).map_err(NodeError::Log)?;
        }
        avatars
            .record(&account, revision, avatar)
            .map_err(NodeError::Log)?;
        *current = Some(payload.clone());
        Ok(payload)
    }

    pub(super) fn hidden_by_language(&self, id: EventId) -> bool {
        self.language_state
            .lock()
            .expect("language state mutex not poisoned")
            .hidden(id)
    }

    /// Called before the incoming WAL transaction. Recording the decision first is
    /// replay-safe: on a crash the immutable event is retried with the same decision.
    pub(super) fn classify_incoming_language(
        &self,
        id: EventId,
        from: &str,
        body: &super::MessageBody,
    ) -> Result<(), crate::eventlog::LogError> {
        if from == self.account_id() || body.sticker.is_some() {
            return Ok(());
        }
        let allowed = self
            .validate_dm_language(&self.account_id(), &body.text)
            .is_ok_and(|verdict| verdict.allowed);
        self.language_state
            .lock()
            .expect("language state mutex not poisoned")
            .decide(id, !allowed)
    }
}

//! Account-owned language preference and endpoint presentation decisions.
use super::{
    Node, NodeError,
    language_policy::{self, LanguageVerdict},
};
use crate::eventlog::EventId;

pub(super) const HIDDEN_TEXT: &[u8] = b"[Message hidden by your language setting]";

impl Node {
    pub fn dm_language_policy(&self, account: &str) -> Option<String> {
        self.account_profiles.language(account)
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
        self.account_profiles
            .authorize_language(account, text, accept)
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
        super::account_profile::AccountProfiles::validate_language(language.as_deref())?;
        let mut payload = None;
        authorize(&mut || {
            payload = Some(self.account_profiles.set_language(language.clone())?);
            Ok(())
        })?;
        if let Some(payload) = payload {
            self.publish_profile_to_peers(&payload).await;
        }
        Ok(())
    }

    pub(super) fn hidden_by_language(&self, id: EventId) -> bool {
        self.account_profiles.hidden(id)
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
        self.account_profiles.decide(id, !allowed)
    }
}

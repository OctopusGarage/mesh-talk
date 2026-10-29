//! Raw account message intent uses the existing deterministic message encoder.
use super::*;
use crate::node::{MessageProtection, message::MessageBody};

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountGroupTextInput {
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
    pub expires_at_ms: Option<u64>,
    pub protection: Option<MessageProtection>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AccountGroupStickerInput {
    pub sticker_id: String,
    pub fallback: Vec<u8>,
    pub expires_at_ms: Option<u64>,
}

impl PreparedAccountGroupMessage {
    /// A strict owning conversion shared by admission and durable effect binding.
    /// Unlike the historical raw-text decoder, this admits only canonical new
    /// text/sticker bodies, with the original protection and deadline framing.
    pub(in crate::node) fn from_plaintext(
        kind: AccountGroupOperationKind,
        encoded_body: Vec<u8>,
    ) -> Option<Self> {
        let body = MessageBody::decode_revealed(&encoded_body);
        let matches = match kind {
            AccountGroupOperationKind::Text => body.sticker.is_none(),
            AccountGroupOperationKind::Sticker => {
                body.sticker.is_some() && body.protection.is_none()
            }
            _ => false,
        };
        let deadline = crate::expiry::deadline(&encoded_body);
        (matches && crate::expiry::wrap(&body.encode(), deadline) == encoded_body).then_some(Self {
            kind,
            encoded_body,
            staged_assets: Vec::new(),
        })
    }

    pub(in crate::node) fn encoded_body(&self) -> &[u8] {
        &self.encoded_body
    }

    pub(in crate::node) fn expires_at_ms(&self) -> Option<u64> {
        if self.kind == AccountGroupOperationKind::File {
            return super::file_preparation::GroupFilePayload::decode(&self.encoded_body)
                .and_then(|payload| payload.logical.expires_at_ms);
        }
        crate::expiry::deadline(&self.encoded_body)
    }

    pub(in crate::node) fn validate_new_admission(&self) -> Result<(), NodeError> {
        if self.kind == AccountGroupOperationKind::File {
            let payload = super::file_preparation::GroupFilePayload::decode(&self.encoded_body)
                .filter(|payload| {
                    payload.descriptor.is_some() && self.staged_assets == [payload.logical.checksum]
                })
                .ok_or_else(|| NodeError::InvalidInput("invalid prepared group File".into()))?;
            return crate::node::expiry_policy::validate_outgoing(payload.logical.expires_at_ms);
        }
        if !self.staged_assets.is_empty()
            || Self::from_plaintext(self.kind, self.encoded_body.clone()).is_none()
        {
            return Err(NodeError::InvalidInput(
                "invalid prepared group message".into(),
            ));
        }
        crate::node::expiry_policy::validate_outgoing(self.expires_at_ms())
    }
}

impl Node {
    pub fn submit_account_group_text_if(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        input: AccountGroupTextInput,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        let body = match input.protection {
            Some(protection) => MessageBody::protected(input.text, input.reply_to, protection),
            None => MessageBody::new(input.text, input.reply_to),
        };
        let message = PreparedAccountGroupMessage::from_plaintext(
            AccountGroupOperationKind::Text,
            crate::expiry::wrap(&body.encode(), input.expires_at_ms),
        )
        .ok_or_else(|| NodeError::InvalidInput("invalid group text".into()))?;
        // Existing-ID lookup precedes fresh-expiry validation in the facade:
        // retrying an already accepted message cannot become a new admission.
        self.submit_account_group_operation_if(
            expected_account,
            id,
            group,
            AccountGroupOperation::Send { message },
            authorize,
        )
    }

    pub fn submit_account_group_sticker_if(
        &self,
        expected_account: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        input: AccountGroupStickerInput,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountGroupOperationStatus, NodeError> {
        let message = PreparedAccountGroupMessage::from_plaintext(
            AccountGroupOperationKind::Sticker,
            crate::expiry::wrap(
                &MessageBody::sticker(input.sticker_id, input.fallback).encode(),
                input.expires_at_ms,
            ),
        )
        .ok_or_else(|| NodeError::InvalidInput("invalid group sticker".into()))?;
        self.submit_account_group_operation_if(
            expected_account,
            id,
            group,
            AccountGroupOperation::Send { message },
            authorize,
        )
    }
}

#[cfg(test)]
impl Node {
    /// Fixture-only: bypass client admission to exercise the receiving source rule.
    pub(in crate::node) fn seed_account_group_request_for_test(
        &self,
        reference: journal::OperationReference,
        body: AccountGroupOperation,
    ) {
        let phase = AccountGroupOperationPhase::AwaitingAcceptance;
        self.group_operations
            .lock()
            .unwrap()
            .put(journal::Entry {
                file_copy: None,
                status: AccountGroupOperationStatus {
                    id: reference.id,
                    account_id: self.account_id(),
                    group: reference.group,
                    kind: reference.kind,
                    phase: phase.clone(),
                    executions: vec![AccountGroupOperationExecution {
                        device_id: reference.executor.device.user_id(),
                        phase,
                        committed_events: vec![],
                    }],
                    committed_events: vec![],
                    primary_event: None,
                },
                reference,
                body: Some(body),
                source: false,
                cancel: false,
                sequence: 0,
            })
            .unwrap();
    }
}

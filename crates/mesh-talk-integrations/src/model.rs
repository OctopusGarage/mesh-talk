//! Transport-independent commands and safe errors.
use mesh_talk_core::eventlog::event::{ConversationId, EventId};
use mesh_talk_core::node::{MessageDestination, NodeError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub const MAX_TEXT_BYTES: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntegrationError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
impl IntegrationError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }
}
impl std::fmt::Display for IntegrationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for IntegrationError {}
impl From<std::io::Error> for IntegrationError {
    fn from(_: std::io::Error) -> Self {
        Self::new("IO_ERROR", "Local integration I/O failed")
    }
}
impl From<NodeError> for IntegrationError {
    fn from(error: NodeError) -> Self {
        match error {
            NodeError::Authorization(_) => Self::new(
                "SCOPE_DENIED",
                "The current grant or host session does not allow this operation",
            ),
            NodeError::UnknownPeer(_) => {
                Self::new("DESTINATION_UNAVAILABLE", "Destination unavailable")
            }
            NodeError::RequestConflict => Self::new(
                "IDEMPOTENCY_CONFLICT",
                "This retry key was already used for different content",
            ),
            NodeError::InvalidInput(_) => Self::new(
                "INVALID_INPUT",
                "The message does not satisfy messaging limits",
            ),
            NodeError::Channel(_) => Self::new(
                "DESTINATION_UNAVAILABLE",
                "Channel unavailable or invitation not accepted",
            ),
            _ => Self::new(
                "STORAGE_FAILURE",
                "The operation could not be completed; retry using the same key",
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Destination {
    Account { id: String },
    Channel { id: String },
}
impl Destination {
    pub fn validate(&self) -> Result<(), IntegrationError> {
        let (id, length) = match self {
            Self::Account { id } => (id, 32),
            Self::Channel { id } => (id, 64),
        };
        if exact_hex(id, length) {
            Ok(())
        } else {
            Err(IntegrationError::new(
                "INVALID_INPUT",
                "Use a full lowercase destination fingerprint",
            ))
        }
    }
    pub fn core(&self) -> Result<MessageDestination, IntegrationError> {
        self.validate()?;
        match self {
            Self::Account { id } => Ok(MessageDestination::Account(id.clone())),
            Self::Channel { id } => Ok(MessageDestination::Channel(ConversationId::new(
                decode_id(id)?,
            ))),
        }
    }
}
pub(crate) fn exact_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn decode_id(value: &str) -> Result<[u8; 32], IntegrationError> {
    if !exact_hex(value, 64) {
        return Err(IntegrationError::new(
            "INVALID_INPUT",
            "Use a full lowercase event fingerprint",
        ));
    }
    let mut out = [0; 32];
    hex::decode_to_slice(value, &mut out)
        .map_err(|_| IntegrationError::new("INVALID_INPUT", "Invalid fingerprint"))?;
    Ok(out)
}
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendMessage {
    pub destination: Destination,
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    pub idempotency_key: String,
}
/// The retry receipt query shared by all integration adapters.
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct StatusRequest {
    pub idempotency_key: String,
}

impl SendMessage {
    pub fn validate(&self) -> Result<(), IntegrationError> {
        self.destination.validate()?;
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(IntegrationError::new(
                "PAYLOAD_TOO_LARGE",
                "Message text exceeds 8192 UTF-8 bytes",
            ));
        }
        if self.text.trim().is_empty()
            || self.idempotency_key.is_empty()
            || self.idempotency_key.len() > 256
            || self.idempotency_key.chars().any(char::is_control)
        {
            return Err(IntegrationError::new(
                "INVALID_INPUT",
                "Nonblank text and a retry key of 1–256 bytes are required",
            ));
        }
        self.reply()?;
        Ok(())
    }
    pub fn reply(&self) -> Result<Option<EventId>, IntegrationError> {
        self.reply_to
            .as_deref()
            .map(|id| decode_id(id).map(EventId::new))
            .transpose()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantSpec {
    pub client_id: String,
    pub destinations: Vec<Destination>,
    #[serde(default)]
    pub describe: bool,
    pub expires_at_ms: u64,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn destination_requires_exact_typed_lowercase_fingerprint() {
        let account = Destination::Account {
            id: "ab".repeat(16),
        };
        assert!(account.validate().is_ok());
        assert!(
            Destination::Channel {
                id: "cd".repeat(32)
            }
            .validate()
            .is_ok()
        );
        assert!(
            Destination::Account { id: "ab".repeat(4) }
                .validate()
                .is_err()
        );
        assert!(
            Destination::Channel {
                id: "CD".repeat(32)
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn send_rejects_blank_content_and_missing_retry_key() {
        let mut request = SendMessage {
            destination: Destination::Account {
                id: "ab".repeat(16),
            },
            text: "Build finished".into(),
            reply_to: None,
            expires_at_ms: None,
            idempotency_key: "build:1".into(),
        };
        assert!(request.validate().is_ok());
        request.text = " \n ".into();
        assert_eq!(request.validate().unwrap_err().code, "INVALID_INPUT");
        request.text = "ok".into();
        request.idempotency_key.clear();
        assert!(request.validate().is_err());
    }
    #[test]
    fn send_rejects_oversized_utf8_and_unknown_json_fields() {
        let value = serde_json::json!({"destination":{"kind":"account","id":"ab".repeat(16)},"text":"ok","idempotency_key":"one","author":"someone else"});
        assert!(serde_json::from_value::<SendMessage>(value).is_err());
        let request = SendMessage {
            destination: Destination::Account {
                id: "ab".repeat(16),
            },
            text: "界".repeat(3000),
            reply_to: None,
            expires_at_ms: None,
            idempotency_key: "build:1".into(),
        };
        assert_eq!(request.validate().unwrap_err().code, "PAYLOAD_TOO_LARGE");
    }
}

//! Shared content presentation access policy.

use serde::{Deserialize, Serialize};

/// Presentation access required before a host reveals retained text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageProtection {
    Click,
    Authenticate,
}

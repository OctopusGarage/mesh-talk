//! Account group role and operation admission, independent of the signing device.

use crate::{
    eventlog::{ConversationId, EventId},
    identity::device::PublicIdentity,
};
use serde::{Deserialize, Serialize};

mod access;
pub(in crate::node) mod operations;
pub use operations::{
    AccountGroupOperation, AccountGroupOperationExecution, AccountGroupOperationId,
    AccountGroupOperationKind, AccountGroupOperationPhase, AccountGroupOperationQuery,
    AccountGroupOperationRejection, AccountGroupOperationStatus, PreparedAccountGroupMessage,
};
pub(in crate::node) mod wire;

/// Account permission and executor reachability have independent lifetimes.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountGroupRole {
    Invited,
    Member,
    Owner,
    Left,
    Dissolved,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountGroupExecution {
    Local,
    ViaDevice(PublicIdentity),
    WaitingForDevice,
    RequiresUpgrade,
    Reconciling,
}

/// The original validated public membership and explicit acceptance, if needed.
/// Epoch changes do not create a new membership lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MembershipLifetime {
    pub member: PublicIdentity,
    pub membership: EventId,
    pub acceptance: Option<EventId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorityExpectation {
    pub authority: Option<EventId>,
    pub epoch: u64,
}

/// One physical source's validated authority. Account aggregation never changes
/// this source role or allows it to sign another device's public effects.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountGroupSource {
    pub device: PublicIdentity,
    pub role: AccountGroupRole,
    pub lifetime: MembershipLifetime,
    pub authority: AuthorityExpectation,
    pub execution: AccountGroupExecution,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountGroupAccess {
    pub group: ConversationId,
    pub account_id: String,
    pub role: AccountGroupRole,
    pub execution: AccountGroupExecution,
    pub sources: Vec<AccountGroupSource>,
}

impl AccountGroupAccess {
    pub fn can_leave_account(&self) -> bool {
        matches!(
            self.role,
            AccountGroupRole::Member | AccountGroupRole::Invited
        )
    }
}

#[cfg(test)]
mod tests;

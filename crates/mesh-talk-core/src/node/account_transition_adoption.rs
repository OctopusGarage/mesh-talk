//! Stages exact-device account transitions before authenticated keystore adoption.

use super::{Node, NodeError};
use crate::identity::{account::Account, account_transition::AccountBindingTransition};

impl Node {
    /// Keep the proof private until a subsequent startup loads the adopted root.
    /// A failed keystore write can already have replaced the file, so recovery
    /// must consult the loaded keystore rather than the write's return value.
    pub(in crate::node) fn prepare_account_transition(
        &self,
        account: &Account,
    ) -> Result<(), NodeError> {
        let current = self.account.public().ed25519_pub;
        if current == account.public().ed25519_pub {
            return Ok(());
        }
        let public = self.identity.public();
        let mut transitions = self
            .account_transitions
            .lock()
            .expect("account transition lock not poisoned");
        let previous = transitions
            .own_chain(&public, &current)
            .last()
            .map(AccountBindingTransition::id);
        transitions
            .prepare(AccountBindingTransition::create(
                &self.account,
                account,
                &self.identity,
                previous,
            ))
            .map_err(NodeError::Log)
    }
}

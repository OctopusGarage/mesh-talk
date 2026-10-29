//! Temporary transfer eligibility; never mutates signed sharing intent or retention.
use super::Node;
use crate::{eventlog::ConversationId, identity::device::PublicIdentity};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};

/// Host observation, not proof of the carrier's billing or hotspot identity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum NetworkCost {
    /// Compatibility default for SDK/headless hosts without a cost provider.
    #[default]
    Unrestricted = 0,
    Unknown = 1,
    Restricted = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct GroupFileSharingStatus {
    pub network_cost: NetworkCost,
    pub paused: bool,
    pub override_enabled: bool,
}

const OVERRIDE: u8 = 4;

#[derive(Default)]
pub(super) struct GroupFileSharingPolicy(AtomicU8);

impl GroupFileSharingPolicy {
    fn status(&self) -> GroupFileSharingStatus {
        let value = self.0.load(Ordering::Acquire);
        let network_cost = match value & !OVERRIDE {
            0 => NetworkCost::Unrestricted,
            2 => NetworkCost::Restricted,
            _ => NetworkCost::Unknown,
        };
        GroupFileSharingStatus {
            network_cost,
            override_enabled: value & OVERRIDE != 0,
            paused: network_cost != NetworkCost::Unrestricted && value & OVERRIDE == 0,
        }
    }

    fn observe(&self, cost: NetworkCost) -> bool {
        let mut old = self.0.load(Ordering::Acquire);
        while old & !OVERRIDE != cost as u8 {
            match self
                .0
                .compare_exchange_weak(old, cost as u8, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(current) => old = current,
            }
        }
        false
    }

    fn override_until_change(&self, expected: NetworkCost, enabled: bool) -> bool {
        let mut old = self.0.load(Ordering::Acquire);
        while old & !OVERRIDE == expected as u8 {
            let next = expected as u8 | if enabled { OVERRIDE } else { 0 };
            match self
                .0
                .compare_exchange_weak(old, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(current) => old = current,
            }
        }
        false
    }
}

impl Node {
    pub fn group_file_sharing_status(&self) -> GroupFileSharingStatus {
        self.group_file_sharing.status()
    }

    /// Update this runtime's cost observation. A changed class expires overrides.
    pub fn set_network_cost(&self, cost: NetworkCost) {
        if self.group_file_sharing.observe(cost) {
            self.note_group_file_sharing_change();
        }
    }

    /// Session-local exception; stale cost observations cannot grant an exception.
    pub fn set_group_file_sharing_override(&self, expected: NetworkCost, enabled: bool) -> bool {
        if !self
            .group_file_sharing
            .override_until_change(expected, enabled)
        {
            return false;
        }
        self.note_group_file_sharing_change();
        true
    }

    fn note_group_file_sharing_change(&self) {
        let groups = self
            .channels
            .lock()
            .expect("channels lock not poisoned")
            .channel_ids();
        for group in groups {
            let _ = self.channel_changes.send(group);
        }
        self.note_ui_change();
    }

    pub(super) fn restricts_group_file_replica(
        &self,
        file: &ConversationId,
        peer: &PublicIdentity,
    ) -> Result<bool, crate::eventlog::LogError> {
        if !self.group_file_sharing_status().paused || self.certified_sibling(peer) {
            return Ok(false);
        }
        let manifests = self
            .files
            .lock()
            .expect("files lock not poisoned")
            .manifest_events(*file);
        if manifests.is_empty() {
            return Ok(false);
        }
        let parents: Vec<_> = {
            let log = self.log.lock().expect("log lock not poisoned");
            let mut parents = Vec::new();
            for id in &manifests {
                if let Some(conversation) = log.event_conversation(id)? {
                    parents.push(conversation);
                }
            }
            parents
        };
        let book = self.channels.lock().expect("channels lock not poisoned");
        Ok(parents
            .into_iter()
            .any(|parent| book.state(&parent).is_some()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_cost_override_expires_on_change_and_rejects_stale_observation() {
        let policy = GroupFileSharingPolicy::default();
        assert!(!policy.status().paused);
        assert!(policy.observe(NetworkCost::Unknown));
        assert!(policy.status().paused);
        assert!(!policy.override_until_change(NetworkCost::Unrestricted, true));
        assert!(policy.override_until_change(NetworkCost::Unknown, true));
        assert!(!policy.status().paused);
        assert!(
            !policy.observe(NetworkCost::Unknown),
            "identical refresh retains the exception"
        );
        assert!(policy.observe(NetworkCost::Restricted));
        assert!(policy.status().paused);
        assert!(!policy.status().override_enabled);
        assert!(policy.override_until_change(NetworkCost::Restricted, true));
        assert!(policy.override_until_change(NetworkCost::Restricted, false));
        assert!(policy.status().paused);
        assert!(policy.observe(NetworkCost::Unrestricted));
        assert!(!policy.status().paused);
    }
}

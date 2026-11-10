//! Shared mailbox direction policy. Discovery consent is independent of data directions.

use super::Node;

impl Node {
    /// Enable or disable use of discovered post offices for this node.
    pub fn set_post_office_enabled(&self, enabled: bool) {
        let changed = self
            .post_office_enabled
            .swap(enabled, std::sync::atomic::Ordering::AcqRel)
            != enabled;
        if !enabled && changed {
            self.privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.sessions.clear();
            self.clear_bridge_introductions();
        }
        if changed {
            self.post_office_receive_notify.notify_one();
        }
        self.bridge_notify.notify_one();
        self.delivery_notify.notify_one();
    }

    /// Independently permit opaque-mailbox publication and retrieval. Disabling a
    /// direction leaves accepted local work pending for later retry.
    pub fn set_post_office_permissions(&self, send: bool, receive: bool) {
        use std::sync::atomic::Ordering;
        let send_changed = self.post_office_send_enabled.swap(send, Ordering::AcqRel) != send;
        let receive_changed = self
            .post_office_receive_enabled
            .swap(receive, Ordering::AcqRel)
            != receive;
        if send_changed || receive_changed {
            self.privacy
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            self.sessions.clear();
            self.post_office_receive_notify.notify_one();
        }
        self.delivery_notify.notify_one();
    }

    pub(in crate::node) fn relay_send_allowed(
        &self,
        relay: &crate::identity::device::PublicIdentity,
    ) -> bool {
        self.post_office_send_enabled
            .load(std::sync::atomic::Ordering::Acquire)
            && self.relay_allowed(relay)
    }

    pub(in crate::node) fn relay_receive_allowed(
        &self,
        relay: &crate::identity::device::PublicIdentity,
    ) -> bool {
        self.post_office_receive_enabled
            .load(std::sync::atomic::Ordering::Acquire)
            && self.relay_allowed(relay)
    }
}

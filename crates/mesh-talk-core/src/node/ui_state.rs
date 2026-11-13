//! Coalesced host invalidation. Queries remain authoritative; no plaintext is queued.
use super::Node;
use tokio::sync::watch;

/// Cumulative desktop invalidations. Queries, not these counters, own state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DesktopUiRevisions {
    pub state: u64,
    pub documents: u64,
    pub sync_status: u64,
}

#[derive(Clone)]
pub(in crate::node) struct UiChanges {
    legacy: watch::Sender<u64>,
    desktop: watch::Sender<DesktopUiRevisions>,
}
impl Default for UiChanges {
    fn default() -> Self {
        watch::channel(0).0.into()
    }
}
impl From<watch::Sender<u64>> for UiChanges {
    fn from(legacy: watch::Sender<u64>) -> Self {
        Self {
            legacy,
            desktop: watch::channel(DesktopUiRevisions::default()).0,
        }
    }
}
impl UiChanges {
    pub(in crate::node) fn subscribe(&self) -> watch::Receiver<u64> {
        self.legacy.subscribe()
    }
    fn subscribe_desktop(&self) -> watch::Receiver<DesktopUiRevisions> {
        self.desktop.subscribe()
    }
    // Unclassified producer notifications conservatively invalidate broad state.
    pub(in crate::node) fn send_modify(&self, update: impl FnOnce(&mut u64)) {
        self.legacy.send_modify(update);
        self.desktop.send_modify(|revisions| {
            revisions.state = revisions.state.wrapping_add(1);
        });
    }
    fn notify(&self, update: impl FnOnce(&mut DesktopUiRevisions)) {
        self.legacy
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        self.desktop.send_modify(update);
    }
    pub(in crate::node) fn sync_status(&self) {
        self.notify(|revision| revision.sync_status = revision.sync_status.wrapping_add(1));
    }
    pub(in crate::node) fn documents(&self, narrow: bool, status_changed: bool) {
        self.notify(|revision| {
            if narrow {
                revision.documents = revision.documents.wrapping_add(1);
            } else {
                revision.state = revision.state.wrapping_add(1);
            }
            if status_changed {
                revision.sync_status = revision.sync_status.wrapping_add(1);
            }
        });
    }
}

impl Node {
    /// Durable receipt producers publish only a coalesced observation wake.
    pub(in crate::node) fn note_reception_change(&self) {
        self.note_ui_change();
    }
    /// True when this instance has no legacy message/profile payload queues.
    pub fn bounded_delivery(&self) -> bool {
        !self.incoming.is_legacy()
    }
    /// Ephemeral calls are a separately bounded admission class. A too-small
    /// page leaves the first signal queued; accepted signals are not durable.
    pub fn take_call_signals(
        &self,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<super::ReceivedCallSignal>, crate::eventlog::LogError> {
        self.call_signal_incoming.take(rows, bytes)
    }
    /// Retains only the newest generation even when a host is slow or absent.
    pub fn subscribe_ui_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.ui_changes.subscribe()
    }
    /// Desktop consumers retain all coalesced domains independently. Internal
    /// workers keep the legacy subscription and its existing wake semantics.
    /// Copy the revision value before running queries or awaiting other work.
    pub fn subscribe_desktop_ui_changes(&self) -> watch::Receiver<DesktopUiRevisions> {
        self.ui_changes.subscribe_desktop()
    }
    pub fn subscribe_file_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.file_changes.subscribe()
    }
    pub fn subscribe_roster_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.roster
            .lock()
            .expect("roster lock not poisoned")
            .subscribe_changes()
    }
    pub(in crate::node) fn note_file_change(&self) {
        self.file_changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    pub(in crate::node) fn note_ui_change(&self) {
        self.ui_changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_domains_preserve_broad_and_narrow_work_in_one_observation() {
        let changes = UiChanges::default();
        let legacy = changes.subscribe();
        let desktop = changes.subscribe_desktop();
        changes.sync_status();
        changes.documents(true, true);
        changes.send_modify(|revision| *revision = revision.wrapping_add(1));
        assert_eq!(
            *legacy.borrow(),
            3,
            "one legacy wake per committed publication"
        );
        assert_eq!(
            *desktop.borrow(),
            DesktopUiRevisions {
                state: 1,
                documents: 1,
                sync_status: 2,
            },
            "a later broad event must retain both narrow domains"
        );
        changes.documents(false, false);
        assert_eq!(
            *desktop.borrow(),
            DesktopUiRevisions {
                state: 2,
                documents: 1,
                sync_status: 2,
            },
            "unclassified documents stay broad without manufacturing status work"
        );
        assert_eq!(*legacy.borrow(), 4);
    }

    #[test]
    fn legacy_sender_conversion_and_late_desktop_subscription_retain_revisions() {
        let (legacy, observer) = watch::channel(7u64);
        let changes = UiChanges::from(legacy);
        changes.documents(true, false);
        changes.sync_status();
        assert_eq!(*observer.borrow(), 9);
        assert_eq!(
            *changes.subscribe_desktop().borrow(),
            DesktopUiRevisions {
                state: 0,
                documents: 1,
                sync_status: 1,
            },
            "a slow or not-yet-installed desktop loses no coalesced domain"
        );
    }
}

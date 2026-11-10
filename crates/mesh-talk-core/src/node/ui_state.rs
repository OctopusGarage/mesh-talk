//! Coalesced host invalidation. Queries remain authoritative; no plaintext is queued.
use super::Node;
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

//! Bounded, account-scoped reception deduplication and backlog notification policy.
use std::collections::{HashMap, HashSet, VecDeque};

const MAX_RECEPTION_IDS: usize = 4096;
const MAX_BATCHES: usize = 128;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Alert {
    Silent,
    Live,
    Summary(usize),
}

pub(crate) struct ReceptionDecision {
    pub emit: bool,
    pub alert: Alert,
}

#[derive(Default)]
struct Batch {
    external: usize,
    complete: bool,
}

#[derive(Default)]
pub(crate) struct ReceptionLedger {
    owner: String,
    seen: HashSet<String>,
    order: VecDeque<String>,
    batches: HashMap<(String, String), Batch>,
    batch_order: VecDeque<(String, String)>,
}

impl ReceptionLedger {
    /// Completion carries no message identity or text and never increments unread
    /// or the external-message count, even when its template ID was erased.
    pub(crate) fn complete(
        &mut self,
        owner: &str,
        channel: &str,
        batch_id: &str,
    ) -> ReceptionDecision {
        if self.owner != owner {
            *self = Self {
                owner: owner.to_owned(),
                ..Default::default()
            };
        }
        let Some(batch) = self
            .batches
            .get_mut(&(channel.to_owned(), batch_id.to_owned()))
        else {
            return ReceptionDecision {
                emit: true,
                alert: Alert::Silent,
            };
        };
        if batch.complete {
            return ReceptionDecision {
                emit: false,
                alert: Alert::Silent,
            };
        }
        batch.complete = true;
        ReceptionDecision {
            emit: true,
            alert: if batch.external > 0 {
                Alert::Summary(batch.external)
            } else {
                Alert::Silent
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn accept(
        &mut self,
        owner: &str,
        channel: &str,
        id: &str,
        kind: &str,
        own_device: bool,
        batch_id: Option<&str>,
        complete: bool,
    ) -> ReceptionDecision {
        if self.owner != owner {
            *self = Self {
                owner: owner.to_owned(),
                ..Default::default()
            };
        }
        let fresh = self.seen.insert(id.to_owned());
        if fresh {
            self.order.push_back(id.to_owned());
            while self.order.len() > MAX_RECEPTION_IDS {
                if let Some(expired) = self.order.pop_front() {
                    self.seen.remove(&expired);
                }
            }
        }
        let mut alert = Alert::Silent;
        if kind == "catch_up" {
            let key = (channel.to_owned(), batch_id.unwrap_or(id).to_owned());
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.batches.entry(key.clone())
            {
                self.batch_order.push_back(key.clone());
                entry.insert(Batch::default());
            }
            let batch = self.batches.get_mut(&key).expect("batch inserted");
            if !batch.complete {
                if fresh && !own_device {
                    batch.external += 1;
                }
                if complete {
                    batch.complete = true;
                    if batch.external > 0 {
                        alert = Alert::Summary(batch.external);
                    }
                }
            }
            while self.batch_order.len() > MAX_BATCHES {
                if let Some(expired) = self.batch_order.pop_front() {
                    self.batches.remove(&expired);
                }
            }
        } else if fresh && !own_device && kind == "live" {
            alert = Alert::Live;
        }
        ReceptionDecision {
            emit: fresh || matches!(alert, Alert::Summary(_)),
            alert,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_completes_without_counting_erased_or_unknown_template_identity() {
        let mut ledger = ReceptionLedger::default();
        ledger.accept(
            "alice",
            "group",
            "seen",
            "catch_up",
            false,
            Some("batch"),
            false,
        );
        let terminal = ledger.complete("alice", "group", "batch");
        assert!(terminal.emit);
        assert_eq!(terminal.alert, Alert::Summary(1));
        assert_eq!(ledger.seen.len(), 1);
        assert!(!ledger.complete("alice", "group", "batch").emit);
        assert_eq!(
            ledger.complete("alice", "group", "never-started").alert,
            Alert::Silent
        );
        assert_eq!(ledger.seen.len(), 1);
    }

    #[test]
    fn catch_up_has_one_summary_and_duplicate_paths_stay_silent() {
        let mut ledger = ReceptionLedger::default();
        assert_eq!(
            ledger
                .accept(
                    "alice",
                    "group",
                    "a",
                    "catch_up",
                    false,
                    Some("batch"),
                    false
                )
                .alert,
            Alert::Silent
        );
        assert!(
            !ledger
                .accept(
                    "alice",
                    "group",
                    "a",
                    "catch_up",
                    false,
                    Some("batch"),
                    false
                )
                .emit
        );
        assert_eq!(
            ledger
                .accept(
                    "alice",
                    "group",
                    "b",
                    "catch_up",
                    false,
                    Some("batch"),
                    false
                )
                .alert,
            Alert::Silent
        );
        assert_eq!(
            ledger
                .accept(
                    "alice",
                    "group",
                    "c",
                    "catch_up",
                    false,
                    Some("batch"),
                    true
                )
                .alert,
            Alert::Summary(3)
        );
    }

    #[test]
    fn own_devices_and_history_are_silent_and_owner_switch_resets_dedup() {
        let mut ledger = ReceptionLedger::default();
        assert_eq!(
            ledger
                .accept("alice", "group", "a", "live", true, None, true)
                .alert,
            Alert::Silent
        );
        assert_eq!(
            ledger
                .accept("alice", "group", "b", "history_sync", false, None, true)
                .alert,
            Alert::Silent
        );
        assert_eq!(
            ledger
                .accept("bob", "group", "a", "live", false, None, true)
                .alert,
            Alert::Live
        );
    }

    #[test]
    fn duplicate_or_own_final_frame_completes_only_its_channel_batch() {
        let mut ledger = ReceptionLedger::default();
        ledger.accept("alice", "one", "a", "catch_up", false, Some("batch"), false);
        ledger.accept("alice", "two", "b", "catch_up", false, Some("batch"), false);
        let decision = ledger.accept("alice", "one", "a", "catch_up", false, Some("batch"), true);
        assert!(
            decision.emit,
            "completion metadata must reach the frontend even on a duplicate"
        );
        assert_eq!(decision.alert, Alert::Summary(1));
        assert_eq!(
            ledger
                .accept(
                    "alice",
                    "two",
                    "self",
                    "catch_up",
                    true,
                    Some("batch"),
                    true
                )
                .alert,
            Alert::Summary(1)
        );
        assert_eq!(
            ledger
                .accept(
                    "alice",
                    "two",
                    "self",
                    "catch_up",
                    true,
                    Some("batch"),
                    true
                )
                .alert,
            Alert::Silent
        );
    }

    #[test]
    fn file_and_text_batches_are_independent_and_runtime_replacement_resets_pending() {
        let mut ledger = ReceptionLedger::default();
        ledger.accept(
            "alice:1:1",
            "group",
            "text",
            "catch_up",
            false,
            Some("text:batch"),
            false,
        );
        assert_eq!(
            ledger
                .accept(
                    "alice:1:1",
                    "group",
                    "file",
                    "catch_up",
                    false,
                    Some("file:batch"),
                    true
                )
                .alert,
            Alert::Summary(1)
        );
        assert_eq!(
            ledger
                .accept(
                    "alice:1:1",
                    "group",
                    "file",
                    "catch_up",
                    false,
                    Some("file:batch"),
                    true
                )
                .alert,
            Alert::Silent
        );
        assert_eq!(
            ledger
                .accept("alice:1:2", "group", "text", "live", false, None, true)
                .alert,
            Alert::Live
        );
        assert!(ledger.batches.is_empty());
    }

    #[test]
    fn dedup_and_batch_memory_are_bounded() {
        let mut ledger = ReceptionLedger::default();
        for id in 0..MAX_RECEPTION_IDS + 10 {
            ledger.accept(
                "alice",
                "group",
                &id.to_string(),
                "catch_up",
                false,
                None,
                true,
            );
        }
        assert_eq!(ledger.seen.len(), MAX_RECEPTION_IDS);
        assert_eq!(ledger.order.len(), MAX_RECEPTION_IDS);
        assert_eq!(ledger.batches.len(), MAX_BATCHES);
        assert_eq!(ledger.batch_order.len(), MAX_BATCHES);
    }
}

//! One bounded local-storage turn, independent of account network scheduling.
use super::*;
use crate::node::account_group::operations::AccountGroupDataWorkTurn;

#[derive(Default)]
pub(super) struct LocalMaintenance {
    custody: Option<(EventId, String)>,
    inventory: Option<ConversationId>,
    profile: Option<String>,
    personal: Option<DocumentConversation>,
    personal_retry_at: Option<std::time::Instant>,
    access: Option<ConversationId>,
    operation_data: Option<super::super::AccountGroupOperationId>,
}

pub(super) struct LocalMaintenanceTurn {
    pub state: LocalMaintenance,
    pub fanout_after: u64,
    pub fanout: Vec<(u64, ConversationId)>,
    pub affected: Vec<(ConversationId, u64)>,
    pub data: Option<AccountGroupDataWorkTurn>,
}

impl LocalMaintenance {
    pub(super) fn run(
        mut self,
        node: Arc<Node>,
        fanout_cursor: u64,
        select_data: bool,
    ) -> LocalMaintenanceTurn {
        if self
            .personal_retry_at
            .is_none_or(|at| at <= std::time::Instant::now())
            && let Some(conversation) = node.personal_cleanup_next(self.personal.as_ref())
        {
            self.personal = Some(conversation.clone());
            self.personal_retry_at = node
                .retry_personal_cleanup_for(&conversation)
                .is_err()
                .then(|| std::time::Instant::now() + Duration::from_secs(5));
        }
        let affected = node.maintain_account_group_operation_effects();
        let fanout = node.account_group_fanout_page(fanout_cursor, fanout::CAPACITY);
        let affected = affected
            .into_iter()
            .filter_map(|group| {
                node.account_group_fanout_sequence(group)
                    .map(|sequence| (group, sequence))
            })
            .collect();
        node.migrate_channel_custody(&mut self.custody);
        node.publish_account_group_access(&mut self.access);
        node.publish_group_inventory(&mut self.inventory);
        if let Err(error) = node.publish_public_profile_documents_page(&mut self.profile) {
            log::debug!("contact profile publication deferred: {error}");
        }
        let data = select_data.then(|| {
            let turn = node.account_group_data_work(self.operation_data);
            self.operation_data = turn.after;
            if turn.more {
                node.account_sync_notify.notify_one();
            }
            turn
        });
        LocalMaintenanceTurn {
            state: self,
            fanout_after: fanout_cursor,
            fanout,
            affected,
            data,
        }
    }
}

/// A completed read-only selection reserves only currently available slots.
/// Skipped jobs remain durable and must not pin the rotating scan's next page.
pub(super) fn reserve_data<T>(
    work: impl IntoIterator<Item = T>,
    available: usize,
    active: &mut HashSet<String>,
    retry: &HashMap<String, std::time::Instant>,
    now: std::time::Instant,
    worker_key: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut reserved = Vec::new();
    for job in work {
        if reserved.len() == available {
            break;
        }
        let key = worker_key(&job);
        if !active.contains(&key) && retry.get(&key).is_none_or(|at| *at <= now) {
            active.insert(key);
            reserved.push(job);
        }
    }
    reserved
}

/// The parent may have returned overflow ownership to an earlier durable row
/// while this scan ran. Observing its bounded snapshot cannot erase that rewind.
pub(super) fn observe_fanout_turn(
    queue: &mut fanout::FanoutQueue,
    cursor: &mut u64,
    started_after: u64,
    page: Vec<(u64, ConversationId)>,
    affected: Vec<(ConversationId, u64)>,
) {
    let advance = *cursor == started_after;
    for (sequence, group) in page {
        if !queue.observe(group, sequence) {
            break;
        }
        if advance {
            *cursor = sequence;
        }
    }
    for (group, sequence) in affected {
        if !queue.observe(group, sequence) {
            *cursor = (*cursor).min(sequence.saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn actual_local_turn_coalesces_inflight_wakes_without_completion_driven_work() {
        use crate::discovery::Roster;
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::sync::{
            Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::sync::{Notify, mpsc};
        struct Release(Arc<(Mutex<bool>, Condvar)>);
        impl Drop for Release {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap() = true;
                self.0.1.notify_all();
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            files,
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let _release = Release(gate.clone());
        let entered = Arc::new(Notify::new());
        let turns = Arc::new(AtomicUsize::new(0));
        let completions = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let gate = gate.clone();
            let entered = entered.clone();
            let turns = turns.clone();
            let completions = completions.clone();
            move |_, phase| {
                if phase == "operation_owner phase=local_turn_completed" {
                    completions.fetch_add(1, Ordering::AcqRel);
                }
                if phase.starts_with("operation_owner phase=legacy_publication_end ")
                    && turns.fetch_add(1, Ordering::AcqRel) == 0
                {
                    let mut released = gate.0.lock().unwrap();
                    entered.notify_one();
                    while !*released {
                        released = gate.1.wait(released).unwrap();
                    }
                }
            }
        }));
        // No periodic tick or peers can manufacture another ordinary turn.
        // Manual polling distinguishes actual worker completion from parent
        // consumption without sleeps or a separate coordination hook.
        let mut owner = Box::pin(node.clone().run_account_history_loop_with_tick(None));
        assert!(futures_util::poll!(owner.as_mut()).is_pending());
        tokio::time::timeout(Duration::from_secs(30), entered.notified())
            .await
            .unwrap();
        node.account_sync_notify.notify_one();
        node.account_sync_notify.notify_one();
        assert!(futures_util::poll!(owner.as_mut()).is_pending());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        // Retirement is not JoinSet result delivery. Drive the actual owner
        // with the current waker until it consumes both completed turns, then
        // inspect admission after the rest of that parent poll has run.
        tokio::time::timeout(
            Duration::from_secs(30),
            futures_util::future::poll_fn(|cx| {
                assert!(owner.as_mut().poll(cx).is_pending());
                if completions.load(Ordering::Acquire) >= 2 {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }),
        )
        .await
        .unwrap();
        // If completion wrongly starts a third queued closure, its actual
        // admission must retire before this counter is accepted.
        tokio::time::timeout(Duration::from_secs(30), node.runtime_work.drain())
            .await
            .unwrap();
        assert_eq!(
            turns.load(Ordering::Acquire),
            2,
            "inflight wakes retain one turn; local completion alone retains none"
        );
        drop(owner);
        node.runtime_work.close();
        tokio::time::timeout(Duration::from_secs(30), node.runtime_work.drain())
            .await
            .unwrap();
        *node.account_client_test_hook.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn capacity_change_at_selection_handoff_revisits_durable_work_on_ordinary_turn() {
        let (selected, received) = tokio::sync::oneshot::channel();
        let durable = vec!["peer-a".to_owned()];
        selected.send(durable.clone()).unwrap();
        let mut active = HashSet::new();
        let now = std::time::Instant::now();
        let retry = HashMap::new();
        assert!(
            reserve_data(
                received.await.unwrap(),
                0,
                &mut active,
                &retry,
                now,
                Clone::clone
            )
            .is_empty()
        );
        // The result's skipped selection is not a queue or destructive read.
        // A real capacity-release wake admits the next ordinary durable scan.
        assert_eq!(
            reserve_data(durable, 1, &mut active, &retry, now, Clone::clone),
            ["peer-a"]
        );
    }

    #[test]
    fn one_peer_batch_cannot_pin_later_peer_or_bypass_current_retry() {
        let now = std::time::Instant::now();
        let retry_at = now + RETRY_INTERVAL;
        let mut active = HashSet::new();
        let retry = HashMap::from([("peer-a".to_owned(), retry_at)]);
        let first = vec!["peer-a".to_owned(); 128];
        assert!(reserve_data(first, 2, &mut active, &retry, now, Clone::clone).is_empty());
        // Keep the source's advanced cursor: the next bounded page can use the
        // free slot rather than waiting for all of A's prior files to finish.
        assert_eq!(
            reserve_data(
                ["peer-b".to_owned()],
                2,
                &mut active,
                &retry,
                now,
                Clone::clone
            ),
            ["peer-b"]
        );
        assert!(
            reserve_data(
                ["peer-b".to_owned()],
                1,
                &mut active,
                &retry,
                now,
                Clone::clone
            )
            .is_empty()
        );
        assert_eq!(
            reserve_data(
                ["peer-a".to_owned()],
                1,
                &mut active,
                &retry,
                retry_at,
                Clone::clone
            ),
            ["peer-a"]
        );
    }

    #[test]
    fn stale_fanout_handoff_preserves_concurrent_overflow_rewind() {
        let mut queue = fanout::FanoutQueue::default();
        for sequence in 1..=fanout::CAPACITY {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(sequence as u64).to_be_bytes());
            assert!(queue.observe(ConversationId::new(bytes), sequence as u64));
        }
        let started_after = 500;
        // Overflow returned an earlier revision to the durable source, then an
        // existing fanout freed one queue slot before the scan result arrived.
        let mut cursor = 12;
        assert!(queue.start_next().is_some());
        observe_fanout_turn(
            &mut queue,
            &mut cursor,
            started_after,
            vec![(501, ConversationId::new([250; 32]))],
            vec![],
        );
        assert_eq!(
            cursor, 12,
            "a stale completed scan cannot skip the returned durable prefix"
        );
    }
}

//! Explicit legacy payload delivery and bounded production observation targets.
use super::call::ReceivedCallSignal;
use std::{collections::VecDeque, sync::Mutex};
use tokio::sync::{mpsc, watch};

pub(super) struct HostPayload<T>(Option<mpsc::UnboundedSender<T>>, watch::Sender<u64>);
impl<T> HostPayload<T> {
    pub(super) fn legacy(sender: mpsc::UnboundedSender<T>, changes: watch::Sender<u64>) -> Self {
        Self(Some(sender), changes)
    }
    pub(super) fn projection(changes: watch::Sender<u64>) -> Self {
        Self(None, changes)
    }
    pub(super) fn is_legacy(&self) -> bool {
        self.0.is_some()
    }
    pub(super) fn send(&self, value: T) -> Result<(), mpsc::error::SendError<T>> {
        match &self.0 {
            Some(sender) => sender.send(value),
            None => {
                // The producer committed durable reception/current-profile state
                // before this wake. The payload itself never enters a queue.
                drop(value);
                self.1
                    .send_modify(|generation| *generation = generation.wrapping_add(1));
                Ok(())
            }
        }
    }
}

const CALL_ROWS: usize = 16;
const CALL_BYTES: usize = 256 * 1024;
#[derive(Default)]
struct CallQueue {
    rows: VecDeque<ReceivedCallSignal>,
    bytes: usize,
}
enum CallDelivery {
    Legacy(mpsc::UnboundedSender<ReceivedCallSignal>),
    Bounded {
        queue: Mutex<CallQueue>,
        changes: watch::Sender<u64>,
    },
}
pub(super) struct CallInbox(CallDelivery);
impl CallInbox {
    pub(super) fn legacy(sender: mpsc::UnboundedSender<ReceivedCallSignal>) -> Self {
        Self(CallDelivery::Legacy(sender))
    }
    pub(super) fn bounded(changes: watch::Sender<u64>) -> Self {
        Self(CallDelivery::Bounded {
            queue: Mutex::new(CallQueue::default()),
            changes,
        })
    }
    pub(super) fn send(
        &self,
        signal: ReceivedCallSignal,
    ) -> Result<(), mpsc::error::SendError<ReceivedCallSignal>> {
        match &self.0 {
            CallDelivery::Legacy(sender) => sender.send(signal),
            CallDelivery::Bounded { queue, changes } => {
                let mut queue = queue.lock().expect("call inbox lock not poisoned");
                let charge = signal.payload.len().saturating_add(signal.from.len());
                if queue.rows.len() >= CALL_ROWS || charge > CALL_BYTES.saturating_sub(queue.bytes)
                {
                    return Err(mpsc::error::SendError(signal));
                }
                queue.bytes += charge;
                queue.rows.push_back(signal);
                drop(queue);
                changes.send_modify(|generation| *generation = generation.wrapping_add(1));
                Ok(())
            }
        }
    }
    pub(super) fn take(
        &self,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<ReceivedCallSignal>, crate::eventlog::LogError> {
        use crate::eventlog::LogError;
        if rows == 0 || rows > CALL_ROWS || bytes == 0 || bytes > CALL_BYTES {
            return Err(LogError::InvalidPageBudget);
        }
        let CallDelivery::Bounded { queue, .. } = &self.0 else {
            return Err(LogError::InvalidPageBudget);
        };
        let mut queue = queue.lock().expect("call inbox lock not poisoned");
        let mut result = Vec::new();
        let mut admitted = 0;
        while result.len() < rows {
            let Some(next) = queue.rows.front() else {
                break;
            };
            let charge = next.payload.len() + next.from.len();
            if charge > bytes.saturating_sub(admitted) {
                if result.is_empty() {
                    return Err(LogError::PageBudgetTooSmall { required: charge });
                }
                break;
            }
            admitted += charge;
            queue.bytes -= charge;
            result.push(queue.rows.pop_front().expect("front inspected"));
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adopted_bounded_constructor_exposes_only_its_committed_account_receipt_stream() {
        use crate::{
            discovery::Roster,
            identity::{account::Account, device::DeviceIdentity},
            node::{BoundedNodeOpenRequest, Node},
            storage::profile::ProfileStorageLease,
        };
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let open = |identity, account| {
            Node::open_bounded_with_profile_lease(
                ProfileStorageLease::acquire(directory.path()).unwrap(),
                BoundedNodeOpenRequest {
                    identity,
                    account,
                    roster: Arc::new(Mutex::new(Roster::default())),
                    log_path: &directory.path().join("messages.log"),
                    sent_path: &directory.path().join("sent.log"),
                    password: "pw",
                },
            )
            .unwrap()
        };
        let node = open(identity, Account::generate());
        let previous = node.reception_effects.head().unwrap();
        let next = Account::generate();
        node.delivery
            .lock()
            .unwrap()
            .prepare_profile_adoption(&next.account_id())
            .unwrap();
        crate::storage::profile::participant::prepare_adoption(
            node.storage.database(),
            &node.account.public(),
            &next.public(),
        )
        .unwrap();
        node.close_profile().unwrap();
        let node = open(DeviceIdentity::from_secret_bytes(keys.0, keys.1), next);
        let current = node.reception_effects.head().unwrap();
        assert_eq!(current.sequence, 0);
        assert_ne!(current.epoch, previous.epoch);
        assert!(matches!(
            node.reception_effects.register(
                super::super::reception_effects::ReceptionConsumer::Desktop,
                previous
            ),
            Err(crate::eventlog::LogError::StaleCursor)
        ));
        node.close_profile().unwrap();
    }
    #[test]
    fn explicit_legacy_target_retains_the_callers_collecting_contract() {
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let (generation, observer) = watch::channel(0);
        let target = HostPayload::legacy(sender, generation);
        target.send(vec![1, 2, 3]).unwrap();
        assert!(target.is_legacy());
        assert_eq!(receiver.try_recv().unwrap(), vec![1, 2, 3]);
        assert!(!observer.has_changed().unwrap());
    }
    #[test]
    fn projection_delivery_coalesces_without_allocating_payload_queue() {
        let (generation, mut observer) = watch::channel(0);
        let target = HostPayload::projection(generation);
        assert!(!target.is_legacy());
        for _ in 0..10_000 {
            target.send(vec![42; 4096]).unwrap();
        }
        assert!(observer.has_changed().unwrap());
        assert_eq!(*observer.borrow_and_update(), 10_000);
        assert!(!observer.has_changed().unwrap());
    }
    #[test]
    fn ephemeral_call_admission_rejects_pressure_and_retains_admitted_signals() {
        let (generation, _) = watch::channel(0);
        let inbox = CallInbox::bounded(generation);
        for n in 0..16 {
            inbox
                .send(ReceivedCallSignal {
                    from: n.to_string(),
                    payload: vec![n; 1024],
                })
                .unwrap();
        }
        assert!(
            inbox
                .send(ReceivedCallSignal {
                    from: "overflow".into(),
                    payload: vec![0; 1024]
                })
                .is_err()
        );
        let first = inbox.take(2, 4096).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].from, "0");
        assert_eq!(first[1].from, "1");
        assert_eq!(inbox.take(16, 256 * 1024).unwrap().len(), 14);
        assert!(inbox.take(16, 256 * 1024).unwrap().is_empty());
    }
    #[test]
    fn call_byte_budget_fails_without_dequeuing_first_signal() {
        let (generation, _) = watch::channel(0);
        let inbox = CallInbox::bounded(generation);
        inbox
            .send(ReceivedCallSignal {
                from: "peer".into(),
                payload: vec![0; 65_535],
            })
            .unwrap();
        assert!(inbox.take(16, 100).is_err());
        assert_eq!(inbox.take(16, 256 * 1024).unwrap()[0].payload.len(), 65_535);
        assert!(
            inbox
                .send(ReceivedCallSignal {
                    from: "too-large".into(),
                    payload: vec![0; 256 * 1024]
                })
                .is_err()
        );
    }
    #[tokio::test]
    async fn bounded_node_constructor_initializes_receipts_without_legacy_receivers() {
        use crate::{
            discovery::Roster,
            identity::{account::Account, device::DeviceIdentity},
            node::{BoundedNodeOpenRequest, Node},
            storage::profile::ProfileStorageLease,
        };
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let node = Node::open_bounded_with_profile_lease(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            BoundedNodeOpenRequest {
                identity: DeviceIdentity::generate(),
                account: Account::generate(),
                roster: Arc::new(Mutex::new(Roster::default())),
                log_path: &dir.path().join("messages.log"),
                sent_path: &dir.path().join("sent.log"),
                password: "pw",
            },
        )
        .unwrap();
        assert!(node.bounded_delivery());
        assert!(node.take_profile_receiver().is_none());
        assert!(node.take_call_signal_receiver().is_none());
        assert_eq!(node.reception_effects.head().unwrap().sequence, 0);
        node.close_profile().unwrap();
    }
    #[tokio::test]
    async fn bounded_runtime_startup_selects_no_payload_forwarders_and_stops_cooperatively() {
        use crate::node::{NodeRuntime, RuntimeConfig, RuntimeStartup};
        let dir = tempfile::tempdir().unwrap();
        let runtime = NodeRuntime::start_owned(
            RuntimeStartup::bounded(RuntimeConfig {
                base_dir: dir.path(),
                account_id: "owner",
                display_name: "Owner",
                password: "pw",
                discovery_port: 0,
                post_office_enabled: false,
                candidate_source: None,
            }),
            |_| Ok(()),
            |launch| launch("Owner"),
        )
        .await
        .unwrap();
        assert!(runtime.handle().bounded_delivery());
        assert!(runtime.handle().take_profile_receiver().is_none());
        assert!(runtime.handle().take_call_signal_receiver().is_none());
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.stop())
            .await
            .unwrap();
    }
}

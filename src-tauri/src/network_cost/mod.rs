//! Native observations feed the runtime's file-serving policy, never group authority.
use mesh_talk_core::node::{NetworkCost, Node};
use std::sync::Arc;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// Owned by NodeRuntime: aborting the task drops its native observer/connection.
pub(crate) async fn observe(node: Arc<Node>) {
    #[cfg(target_os = "macos")]
    macos::observe(node).await;
    #[cfg(target_os = "linux")]
    linux::observe(node).await;
    #[cfg(target_os = "windows")]
    windows::observe(node).await;
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    node.set_network_cost(NetworkCost::Unknown);
}

// Windows/Linux share one sequential query owner. Native adapters own their
// observations and retirement; macOS already receives classified path callbacks.
#[cfg(any(target_os = "windows", target_os = "linux", test))]
trait CostSource: Send {
    fn current(&mut self) -> impl std::future::Future<Output = NetworkCost> + Send;
    fn changed(&mut self) -> impl std::future::Future<Output = ()> + Send;
}

#[cfg(any(target_os = "windows", target_os = "linux", test))]
async fn observe_queries(mut source: impl CostSource, mut publish: impl FnMut(NetworkCost) + Send) {
    let mut repair = tokio::time::interval(std::time::Duration::from_secs(300));
    repair.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The first interval tick is immediate. Registration happens inside current
    // before its initial query, so changes during a read retain a follow-up.
    repair.tick().await;
    publish(source.current().await);
    loop {
        tokio::select! {
            _ = repair.tick() => {},
            _ = source.changed() => {},
        }
        publish(source.current().await);
    }
}

fn classify(available: bool, restricted: bool) -> NetworkCost {
    if restricted {
        NetworkCost::Restricted
    } else if available {
        NetworkCost::Unrestricted
    } else {
        NetworkCost::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Queries {
        started: AtomicUsize,
        active: AtomicUsize,
        maximum: AtomicUsize,
    }

    struct ActiveQuery(Arc<Queries>);
    impl Drop for ActiveQuery {
        fn drop(&mut self) {
            self.0.active.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct TestCost {
        queries: Arc<Queries>,
        permits: Arc<tokio::sync::Semaphore>,
        outcomes: std::collections::VecDeque<NetworkCost>,
        changes: tokio::sync::watch::Receiver<()>,
    }
    impl CostSource for TestCost {
        async fn changed(&mut self) {
            let _ = self.changes.changed().await;
        }
        async fn current(&mut self) -> NetworkCost {
            self.queries.started.fetch_add(1, Ordering::SeqCst);
            let active = self.queries.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.queries.maximum.fetch_max(active, Ordering::SeqCst);
            let _active = ActiveQuery(self.queries.clone());
            self.permits.acquire().await.unwrap().forget();
            self.outcomes.pop_front().unwrap_or(NetworkCost::Restricted)
        }
    }

    struct ObserverFixture {
        queries: Arc<Queries>,
        permits: Arc<tokio::sync::Semaphore>,
        changes: tokio::sync::watch::Sender<()>,
        published: tokio::sync::mpsc::UnboundedReceiver<NetworkCost>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for ObserverFixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    impl ObserverFixture {
        fn start(permits: usize, outcomes: &[NetworkCost]) -> Self {
            let queries = Arc::new(Queries::default());
            let permits = Arc::new(tokio::sync::Semaphore::new(permits));
            let (changes, receiver) = tokio::sync::watch::channel(());
            let source = TestCost {
                queries: queries.clone(),
                permits: permits.clone(),
                outcomes: outcomes.iter().copied().collect(),
                changes: receiver,
            };
            let (publish, published) = tokio::sync::mpsc::unbounded_channel();
            let task = tokio::spawn(observe_queries(source, move |cost| {
                let _ = publish.send(cost);
            }));
            Self {
                queries,
                permits,
                changes,
                published,
                task,
            }
        }
        fn count(&self) -> usize {
            self.queries.started.load(Ordering::SeqCst)
        }
        fn dirty(&self) {
            self.changes.send_modify(|_| {});
        }
    }
    async fn flush_observer() {
        // Yield without sleeping: paused-time auto-advance must not manufacture a
        // repair deadline while waiting for an event assertion.
        for _ in 0..32 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn observer_unchanged_cost_waits_for_five_minute_repair() {
        let mut observer = ObserverFixture::start(8, &[NetworkCost::Unrestricted]);
        flush_observer().await;
        assert_eq!(observer.count(), 1);
        assert_eq!(
            observer.published.try_recv().unwrap(),
            NetworkCost::Unrestricted
        );
        tokio::time::advance(std::time::Duration::from_secs(299)).await;
        flush_observer().await;
        assert_eq!(
            observer.count(),
            1,
            "idle observation must not poll before repair"
        );
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        flush_observer().await;
        assert_eq!(
            observer.count(),
            2,
            "repair must re-query current provider state"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn observer_event_burst_coalesces_into_one_current_query() {
        let mut observer =
            ObserverFixture::start(8, &[NetworkCost::Unrestricted, NetworkCost::Restricted]);
        flush_observer().await;
        assert_eq!(
            observer.published.try_recv().unwrap(),
            NetworkCost::Unrestricted
        );
        for _ in 0..64 {
            observer.dirty();
        }
        flush_observer().await;
        assert_eq!(
            observer.count(),
            2,
            "one coalesced native wake must query promptly"
        );
        assert_eq!(
            observer.published.try_recv().unwrap(),
            NetworkCost::Restricted
        );
        assert!(observer.published.try_recv().is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn observer_event_during_held_query_is_retained_and_serial() {
        let observer = ObserverFixture::start(1, &[]);
        flush_observer().await;
        observer.dirty();
        // The baseline's next2s tick also starts a held read, so the final
        // assertion specifically tests the retained event rather than startup.
        tokio::time::advance(std::time::Duration::from_secs(2)).await;
        flush_observer().await;
        assert_eq!(observer.count(), 2);
        assert_eq!(observer.queries.active.load(Ordering::SeqCst), 1);
        for _ in 0..64 {
            observer.dirty();
        }
        flush_observer().await;
        assert_eq!(observer.count(), 2, "events cannot overlap the held query");
        observer.permits.add_permits(1);
        flush_observer().await;
        assert_eq!(
            observer.count(),
            3,
            "event during read must retain one follow-up"
        );
        assert_eq!(observer.queries.maximum.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn observer_failure_publishes_unknown_then_event_recovers_current_cost() {
        let mut observer =
            ObserverFixture::start(8, &[NetworkCost::Unknown, NetworkCost::Restricted]);
        flush_observer().await;
        assert_eq!(observer.published.try_recv().unwrap(), NetworkCost::Unknown);
        observer.dirty();
        flush_observer().await;
        assert_eq!(
            observer.count(),
            2,
            "provider recovery event must not await repair"
        );
        assert_eq!(
            observer.published.try_recv().unwrap(),
            NetworkCost::Restricted
        );
    }

    #[tokio::test(start_paused = true)]
    async fn observer_cancellation_drops_held_query_and_its_owner() {
        let mut observer = ObserverFixture::start(0, &[]);
        flush_observer().await;
        assert_eq!(observer.queries.active.load(Ordering::SeqCst), 1);
        observer.task.abort();
        assert!((&mut observer.task).await.unwrap_err().is_cancelled());
        assert_eq!(observer.queries.active.load(Ordering::SeqCst), 0);
        assert!(
            observer.changes.send(()).is_err(),
            "retired observer must release subscription"
        );
        observer.permits.add_permits(1);
        flush_observer().await;
        assert_eq!(observer.count(), 1);
        assert!(observer.published.try_recv().is_err());
    }

    #[test]
    fn unavailable_cost_never_means_unrestricted_and_restriction_wins() {
        assert_eq!(classify(false, false), NetworkCost::Unknown);
        assert_eq!(classify(true, false), NetworkCost::Unrestricted);
        assert_eq!(classify(false, true), NetworkCost::Restricted);
        assert_eq!(classify(true, true), NetworkCost::Restricted);
    }

    #[tokio::test]
    async fn runtime_retirement_cancels_native_cost_observation() {
        let root = tempfile::tempdir().unwrap();
        let mut runtime = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "cost-owner",
            "Cost owner",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let node = runtime.handle();
        let weak = Arc::downgrade(&node);
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = started.send(());
            observe(node).await;
        });
        let observer = task.abort_handle();
        runtime.register_host_task(task);
        ready.await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.stop())
            .await
            .unwrap();
        assert!(
            observer.is_finished(),
            "native observer must be joined at retirement"
        );
        // A blocking worker can release its retirement permit immediately before
        // its captured Node is dropped. Observe final destruction separately from
        // the native observer's synchronous cancellation/join boundary above.
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("native observers cannot retain a retired Node");
    }
}

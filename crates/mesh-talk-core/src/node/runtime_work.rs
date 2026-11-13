//! In-process retirement barrier for runtime producers, including work whose
//! blocking-pool caller can be cancelled before the filesystem operation ends.
use std::sync::{Arc, Mutex};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};

pub(super) struct RuntimeWork {
    state: Mutex<State>,
    idle: Notify,
    local_repair: Arc<Semaphore>,
}

impl Default for RuntimeWork {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            idle: Notify::new(),
            local_repair: Arc::new(Semaphore::new(1)),
        }
    }
}

#[derive(Default)]
struct State {
    closed: bool,
    active: usize,
}

pub(super) struct WorkPermit(Arc<RuntimeWork>);

/// Releases admission only after the entire child future and its captures
/// have been destroyed, including cancellation before its first poll.
pub(super) struct TrackedFuture<F> {
    inner: Option<Pin<Box<F>>>,
    permit: Option<WorkPermit>,
}

impl WorkPermit {
    pub(super) fn track<F: Future>(self, future: F) -> TrackedFuture<F> {
        TrackedFuture {
            inner: Some(Box::pin(future)),
            permit: Some(self),
        }
    }
}

impl<F: Future> Future for TrackedFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut()
            .inner
            .as_mut()
            .expect("tracked future present")
            .as_mut()
            .poll(cx)
    }
}

impl<F> Drop for TrackedFuture<F> {
    fn drop(&mut self) {
        drop(self.inner.take());
        drop(self.permit.take());
    }
}

impl RuntimeWork {
    /// Fair capacity for top-level local repair only, never network or leaf work.
    /// Runtime shutdown cancels queued owners; running work retains its permit.
    pub(super) async fn local_repair_capacity(&self) -> Option<OwnedSemaphorePermit> {
        if self.is_closed() {
            return None;
        }
        let capacity = self.local_repair.clone().acquire_owned().await.ok()?;
        (!self.is_closed()).then_some(capacity)
    }

    pub(super) fn admit(self: &Arc<Self>) -> Option<WorkPermit> {
        let mut state = self.state.lock().expect("runtime work lock not poisoned");
        if state.closed {
            return None;
        }
        state.active += 1;
        Some(WorkPermit(self.clone()))
    }

    pub(super) fn is_closed(&self) -> bool {
        self.state
            .lock()
            .expect("runtime work lock not poisoned")
            .closed
    }

    pub(super) fn close(&self) {
        self.state
            .lock()
            .expect("runtime work lock not poisoned")
            .closed = true;
    }

    pub(super) async fn drain(&self) {
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            // Register before observing the counter: completion between the
            // observation and await must not be lost by notify_waiters.
            idle.as_mut().enable();
            if self
                .state
                .lock()
                .expect("runtime work lock not poisoned")
                .active
                == 0
            {
                return;
            }
            idle.await;
        }
    }
}

impl Drop for WorkPermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().expect("runtime work lock not poisoned");
        state.active -= 1;
        if state.active == 0 {
            self.0.idle.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct CaptureDropProbe {
        work: Arc<RuntimeWork>,
        observed: Option<tokio::sync::oneshot::Sender<bool>>,
    }

    impl Drop for CaptureDropProbe {
        fn drop(&mut self) {
            let held = self.work.state.lock().unwrap().active > 0;
            let _ = self.observed.take().unwrap().send(held);
        }
    }

    #[tokio::test]
    async fn connection_captures_are_destroyed_before_admission_is_released() {
        let work = Arc::new(RuntimeWork::default());
        let permit = work.admit().unwrap();
        let (observed, result) = tokio::sync::oneshot::channel();
        let probe = CaptureDropProbe {
            work: work.clone(),
            observed: Some(observed),
        };
        let inner = async move {
            let _probe = probe;
            std::future::pending::<()>().await;
        };
        let task = tokio::spawn(permit.track(inner));
        work.close();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        assert!(
            result.await.unwrap(),
            "inner capture outlived admission permit"
        );
        work.drain().await;
    }

    #[tokio::test]
    async fn blocking_captures_are_destroyed_before_admission_is_released() {
        let work = Arc::new(RuntimeWork::default());
        let permit = work.admit().unwrap();
        let (observed, result) = tokio::sync::oneshot::channel();
        let probe = CaptureDropProbe {
            work: work.clone(),
            observed: Some(observed),
        };
        let task = permit.spawn_blocking(move || {
            std::hint::black_box(&probe);
        });
        task.await.unwrap();
        assert!(
            result.await.unwrap(),
            "blocking capture outlived admission permit"
        );
        work.drain().await;
    }

    #[tokio::test]
    async fn cancellation_before_first_poll_releases_admission() {
        let work = Arc::new(RuntimeWork::default());
        let permit = work.admit().unwrap();
        let future = permit.track(async move {
            std::future::pending::<()>().await;
        });
        let task = tokio::spawn(future);
        work.close();
        task.abort();
        assert!(matches!(task.await, Err(error) if error.is_cancelled()));
        tokio::time::timeout(Duration::from_secs(1), work.drain())
            .await
            .unwrap();
        assert!(work.admit().is_none());
    }

    #[tokio::test]
    async fn blocking_unwind_releases_admission() {
        let work = Arc::new(RuntimeWork::default());
        let permit = work.admit().unwrap();
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            panic!("test blocking unwind");
        });
        work.close();
        assert!(task.await.unwrap_err().is_panic());
        tokio::time::timeout(Duration::from_secs(1), work.drain())
            .await
            .unwrap();
    }

    #[test]
    fn queued_blocking_closure_keeps_admission_when_awaiting_caller_is_aborted() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (occupied_tx, occupied_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                occupied_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            occupied_rx.await.unwrap();
            let work = Arc::new(RuntimeWork::default());
            let permit = work.admit().unwrap();
            let (finished_tx, finished_rx) = tokio::sync::oneshot::channel();
            let queued = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                finished_tx.send(()).unwrap();
            });
            let caller = tokio::spawn(async move { queued.await.unwrap() });
            work.close();
            caller.abort();
            assert!(caller.await.unwrap_err().is_cancelled());
            let finished_early = tokio::time::timeout(Duration::from_millis(50), work.drain())
                .await
                .is_ok();
            release_tx.send(()).unwrap();
            occupied.await.unwrap();
            finished_rx.await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), work.drain())
                .await
                .unwrap();
            assert!(!finished_early);
        });
    }
}

/// Blocking captures and undelivered results have the same lifetime contract as
/// TrackedFuture: their resources are destroyed before admission is released.
struct BlockingWork<F> {
    work: Option<F>,
    local_repair: Option<OwnedSemaphorePermit>,
    permit: Option<WorkPermit>,
}

pub(super) struct TrackedValue<T> {
    value: Option<T>,
    local_repair: Option<OwnedSemaphorePermit>,
    permit: Option<WorkPermit>,
}

impl<T> TrackedValue<T> {
    pub(super) fn into_inner(mut self) -> T {
        self.value.take().expect("blocking value present")
    }
}

impl<T> Drop for TrackedValue<T> {
    fn drop(&mut self) {
        drop(self.value.take());
        drop(self.local_repair.take());
        drop(self.permit.take());
    }
}

impl<F> Drop for BlockingWork<F> {
    fn drop(&mut self) {
        drop(self.work.take());
        drop(self.local_repair.take());
        drop(self.permit.take());
    }
}

impl WorkPermit {
    pub(super) fn spawn_blocking<T: Send + 'static>(
        self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> tokio::task::JoinHandle<TrackedValue<T>> {
        self.spawn_blocking_with_capacity(None, work)
    }

    pub(super) fn spawn_local_repair_blocking<T: Send + 'static>(
        self,
        capacity: OwnedSemaphorePermit,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> tokio::task::JoinHandle<TrackedValue<T>> {
        self.spawn_blocking_with_capacity(Some(capacity), work)
    }

    fn spawn_blocking_with_capacity<T: Send + 'static>(
        self,
        capacity: Option<OwnedSemaphorePermit>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> tokio::task::JoinHandle<TrackedValue<T>> {
        let mut owner = BlockingWork {
            work: Some(work),
            local_repair: capacity,
            permit: Some(self),
        };
        tokio::task::spawn_blocking(move || {
            let value = owner.work.take().expect("blocking work present")();
            TrackedValue {
                value: Some(value),
                local_repair: owner.local_repair.take(),
                permit: owner.permit.take(),
            }
        })
    }
}

#[cfg(test)]
mod blocking_result_tests {
    use super::*;

    struct LocalRepairDropProbe {
        work: Arc<RuntimeWork>,
        observed: Option<tokio::sync::oneshot::Sender<bool>>,
    }
    impl Drop for LocalRepairDropProbe {
        fn drop(&mut self) {
            let owned = self.work.local_repair.clone().try_acquire_owned().is_err()
                && self.work.state.lock().unwrap().active > 0;
            let _ = self.observed.take().unwrap().send(owned);
        }
    }

    #[tokio::test]
    async fn local_repair_capacity_outlives_blocking_captures_and_panic() {
        for panic in [false, true] {
            let work = Arc::new(RuntimeWork::default());
            let capacity = work.local_repair_capacity().await.unwrap();
            let (observed, result) = tokio::sync::oneshot::channel();
            let probe = LocalRepairDropProbe {
                work: work.clone(),
                observed: Some(observed),
            };
            let task = work
                .admit()
                .unwrap()
                .spawn_local_repair_blocking(capacity, move || {
                    std::hint::black_box(&probe);
                    assert!(!panic, "intentional local repair unwind");
                });
            let completion = task.await;
            assert!(
                result.await.unwrap(),
                "capture drop must precede both admission releases"
            );
            if panic {
                match completion {
                    Err(error) => assert!(error.is_panic()),
                    Ok(_) => panic!("intentional worker panic must reach its owner"),
                }
            } else {
                let value = completion.unwrap();
                assert!(
                    work.local_repair.clone().try_acquire_owned().is_err(),
                    "undelivered blocking output retains its capacity"
                );
                drop(value);
            }
            assert!(work.local_repair.clone().try_acquire_owned().is_ok());
            work.close();
            work.drain().await;
        }
    }
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Resource(Arc<AtomicBool>);
    impl Drop for Resource {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn canceled_successful_blocking_output_drops_resource_before_completion() {
        let work = Arc::new(RuntimeWork::default());
        let permit = work.admit().unwrap();
        let dropped = Arc::new(AtomicBool::new(false));
        let resource = Resource(dropped.clone());
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, released) = std::sync::mpsc::channel();
        let task = permit.spawn_blocking(move || {
            entered.send(()).unwrap();
            released.recv().unwrap();
            resource
        });
        let caller = tokio::spawn(async move { task.await.unwrap().into_inner() });
        started.await.unwrap();
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        work.close();
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), work.drain())
            .await
            .unwrap();
        assert!(
            dropped.load(Ordering::SeqCst),
            "undelivered output outlived admission"
        );
    }
}

#[cfg(test)]
mod queued_blocking_owner_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Capture(Arc<AtomicBool>);
    impl Drop for Capture {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[test]
    fn canceled_queued_blocking_owner_drops_captures_before_admission() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (entered, started) = tokio::sync::oneshot::channel();
            let (release, released) = std::sync::mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || {
                entered.send(()).unwrap();
                released.recv().unwrap();
            });
            started.await.unwrap();
            let work = Arc::new(RuntimeWork::default());
            let permit = work.admit().unwrap();
            let dropped = Arc::new(AtomicBool::new(false));
            let capture = Capture(dropped.clone());
            let task = permit.spawn_blocking(move || {
                let _capture = capture;
                panic!("canceled queued work must never run");
            });
            task.abort();
            work.close();
            release.send(()).unwrap();
            occupied.await.unwrap();
            assert!(matches!(task.await, Err(error) if error.is_cancelled()));
            tokio::time::timeout(std::time::Duration::from_secs(3), work.drain())
                .await
                .unwrap();
            assert!(dropped.load(Ordering::SeqCst));
        });
    }
}

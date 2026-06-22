//! Bounded-concurrency fan-out. Running an async op over many peers (a multi-peer
//! delivery fan-out, a /24 unicast scan) all at once can open hundreds of sockets at
//! a stroke; serially is needlessly slow. This runs them with at most `limit` in
//! flight, using only `tokio` (no `futures` dependency).

use std::future::Future;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Run `op` over every item in `items` with at most `limit` futures in flight,
/// returning the results in COMPLETION order (not input order). `op` and its output
/// must be `Send + 'static` because each runs on its own task.
///
/// `limit` is clamped to at least 1.
pub async fn bounded_for_each<I, T, F, Fut>(items: I, limit: usize, op: F) -> Vec<T>
where
    I: IntoIterator,
    I::Item: Send + 'static,
    F: Fn(I::Item) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = T> + Send,
    T: Send + 'static,
{
    let limit = limit.max(1);
    let sem = std::sync::Arc::new(Semaphore::new(limit));
    let op = std::sync::Arc::new(op);
    let mut set: JoinSet<T> = JoinSet::new();
    let mut out = Vec::new();

    for item in items {
        // Acquire a permit before spawning so no more than `limit` run concurrently.
        let permit = sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore not closed");
        let op = op.clone();
        set.spawn(async move {
            let _permit = permit; // held for the duration of the op
            op(item).await
        });
    }
    while let Some(joined) = set.join_next().await {
        if let Ok(v) = joined {
            out.push(v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn runs_every_item() {
        let out = bounded_for_each(vec![1, 2, 3, 4, 5], 2, |x| async move { x * 10 }).await;
        let mut sorted = out.clone();
        sorted.sort();
        assert_eq!(sorted, vec![10, 20, 30, 40, 50]);
    }

    #[tokio::test]
    async fn never_exceeds_the_limit() {
        let live = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let live2 = live.clone();
        let peak2 = peak.clone();
        let _ = bounded_for_each(0..20, 3, move |_| {
            let live = live2.clone();
            let peak = peak2.clone();
            async move {
                let n = live.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                live.fetch_sub(1, Ordering::SeqCst);
            }
        })
        .await;
        assert!(
            peak.load(Ordering::SeqCst) <= 3,
            "concurrency exceeded limit"
        );
        assert!(peak.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn empty_input_is_fine() {
        let out: Vec<i32> = bounded_for_each(Vec::<i32>::new(), 4, |x| async move { x }).await;
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn limit_zero_is_clamped_and_still_runs() {
        // Documented: `limit` is clamped to at least 1. A `0` must NOT deadlock on a
        // zero-permit semaphore (which would acquire forever) — it must run serially.
        let out = bounded_for_each(vec![1, 2, 3], 0, |x| async move { x * 2 }).await;
        let mut sorted = out;
        sorted.sort();
        assert_eq!(sorted, vec![2, 4, 6]);
    }
}

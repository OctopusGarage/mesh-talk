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
        runtime.register_host_task(tokio::spawn(async move {
            let _ = started.send(());
            observe(node).await;
        }));
        ready.await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.stop())
            .await
            .unwrap();
        assert!(
            weak.upgrade().is_none(),
            "native observers cannot retain a retired Node"
        );
    }
}

use super::*;
use ::windows::Networking::Connectivity::{
    NetworkCostType, NetworkInformation, NetworkStatusChangedEventHandler,
};

fn current() -> NetworkCost {
    let read = || -> ::windows::core::Result<NetworkCost> {
        let cost = NetworkInformation::GetInternetConnectionProfile()?.GetConnectionCost()?;
        let kind = cost.NetworkCostType()?;
        Ok(classify(
            kind == NetworkCostType::Unrestricted,
            kind == NetworkCostType::Fixed
                || kind == NetworkCostType::Variable
                || cost.Roaming()?
                || cost.OverDataLimit()?
                || cost.ApproachingDataLimit()?
                || cost.BackgroundDataUsageRestricted()?,
        ))
    };
    read().unwrap_or(NetworkCost::Unknown)
}

struct Registration(Option<i64>);
impl Registration {
    fn close(&mut self) -> ::windows::core::Result<()> {
        if let Some(token) = self.0 {
            NetworkInformation::RemoveNetworkStatusChanged(token)?;
            self.0 = None;
        }
        Ok(())
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        // A callback in flight owns only a sender, never the Node or this owner.
        let _ = self.close();
    }
}

struct WindowsCost {
    registration: Option<Registration>,
    updates: tokio::sync::watch::Sender<()>,
    changes: tokio::sync::watch::Receiver<()>,
}
impl WindowsCost {
    fn new() -> Self {
        let (updates, changes) = tokio::sync::watch::channel(());
        Self {
            registration: None,
            updates,
            changes,
        }
    }
}
impl CostSource for WindowsCost {
    async fn current(&mut self) -> NetworkCost {
        if self.registration.is_none() {
            let updates = self.updates.clone();
            let callback = NetworkStatusChangedEventHandler::new(move |_| {
                updates.send_modify(|_| {});
                Ok(())
            });
            match NetworkInformation::NetworkStatusChanged(&callback) {
                Ok(token) => self.registration = Some(Registration(Some(token))),
                // Without a registered observer, do not retain an unrestricted
                // observation. The sparse repair retries registration.
                Err(_) => return NetworkCost::Unknown,
            }
        }
        current()
    }

    async fn changed(&mut self) {
        let _ = self.changes.changed().await;
    }
}

pub(super) async fn observe(node: Arc<Node>) {
    observe_queries(WindowsCost::new(), move |cost| node.set_network_cost(cost)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_cost_registration_is_removed_by_its_owner() {
        let mut observer = WindowsCost::new();
        let _ = observer.current().await;
        let registration = observer.registration.as_mut().expect("native registration");
        assert!(registration.0.is_some());
        registration
            .close()
            .expect("remove owned native registration");
        assert!(registration.0.is_none());
        // Drop after explicit close must not unregister another observation.
        drop(observer);
    }
}

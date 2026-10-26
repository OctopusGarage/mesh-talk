use super::*;
use ::windows::Networking::Connectivity::{NetworkCostType, NetworkInformation};

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

pub(super) async fn observe(node: Arc<Node>) {
    // Re-query even if the interface/IP did not change. No stale profile is cached.
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        node.set_network_cost(current());
    }
}

use super::*;

async fn current(connection: &zbus::Connection) -> zbus::Result<NetworkCost> {
    let proxy = zbus::Proxy::new(
        connection,
        "org.freedesktop.NetworkManager",
        "/org/freedesktop/NetworkManager",
        "org.freedesktop.NetworkManager",
    )
    .await?;
    let value: u32 = proxy.get_property("Metered").await?;
    Ok(match value {
        1 | 3 => classify(true, true),
        2 | 4 => classify(true, false),
        _ => classify(false, false),
    })
}

pub(super) async fn observe(node: Arc<Node>) {
    let mut connection = None;
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            if connection.is_none() {
                connection = Some(zbus::Connection::system().await?);
            }
            current(connection.as_ref().expect("connected above")).await
        })
        .await;
        let cost = match observed {
            Ok(Ok(cost)) => cost,
            _ => {
                connection = None;
                NetworkCost::Unknown
            }
        };
        node.set_network_cost(cost);
    }
}

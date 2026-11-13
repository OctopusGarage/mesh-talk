use super::*;
use std::{collections::HashMap, pin::Pin, task::Poll};
use zbus::export::futures_core::Stream;

const SERVICE: &str = "org.freedesktop.NetworkManager";
const PATH: &str = "/org/freedesktop/NetworkManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

async fn current(connection: &zbus::Connection) -> zbus::Result<NetworkCost> {
    let proxy = zbus::Proxy::new(connection, SERVICE, PATH, PROPERTIES).await?;
    // Explicit Properties.Get: a repair query must not return a cached value.
    let value: zbus::zvariant::OwnedValue = proxy.call("Get", &(SERVICE, "Metered")).await?;
    let value = u32::try_from(value)?;
    Ok(match value {
        1 | 3 => classify(true, true),
        2 | 4 => classify(true, false),
        _ => classify(false, false),
    })
}

struct Observation {
    connection: zbus::Connection,
    properties: zbus::MessageStream,
    owners: zbus::MessageStream,
}
impl Observation {
    async fn register(connection: zbus::Connection) -> zbus::Result<Self> {
        // Install both matches before the first current read. A well-known
        // sender match survives NetworkManager disappearing and returning.
        let property_rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SERVICE)?
            .path(PATH)?
            .interface(PROPERTIES)?
            .member("PropertiesChanged")?
            .arg(0, SERVICE)?
            .build();
        let properties =
            zbus::MessageStream::for_match_rule(property_rule, &connection, Some(8)).await?;
        let owner_rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .path("/org/freedesktop/DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, SERVICE)?
            .build();
        let owners = zbus::MessageStream::for_match_rule(owner_rule, &connection, Some(8)).await?;
        Ok(Self {
            connection,
            properties,
            owners,
        })
    }
}

fn cost_changed(message: &zbus::Message) -> bool {
    type PropertiesChanged = (
        String,
        HashMap<String, zbus::zvariant::OwnedValue>,
        Vec<String>,
    );
    match message.body().deserialize::<PropertiesChanged>() {
        Ok((interface, changed, invalidated)) => {
            interface == SERVICE
                && (changed.contains_key("Metered")
                    || invalidated.iter().any(|name| name == "Metered"))
        }
        // Malformed provider input cannot justify retaining a cached cost.
        Err(_) => true,
    }
}

async fn next(stream: &mut zbus::MessageStream) -> Option<zbus::Result<zbus::Message>> {
    std::future::poll_fn(|cx| Pin::new(&mut *stream).poll_next(cx)).await
}

struct LinuxCost {
    observation: Option<Observation>,
}
impl CostSource for LinuxCost {
    async fn current(&mut self) -> NetworkCost {
        let observed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            if self.observation.is_none() {
                self.observation =
                    Some(Observation::register(zbus::Connection::system().await?).await?);
            }
            current(
                &self
                    .observation
                    .as_ref()
                    .expect("registered above")
                    .connection,
            )
            .await
        })
        .await;
        match observed {
            Ok(Ok(cost)) => cost,
            // Retain live name/property matches on service loss so its return
            // wakes recovery. A terminated stream is retired in changed().
            _ => NetworkCost::Unknown,
        }
    }

    async fn changed(&mut self) {
        let Some(observation) = self.observation.as_mut() else {
            // Registration/provider failures recover at the sparse backstop.
            std::future::pending::<()>().await;
            return;
        };
        loop {
            let relevant = tokio::select! {
                message = next(&mut observation.properties) => match message {
                    Some(Ok(message)) => cost_changed(&message),
                    _ => { self.observation = None; return; },
                },
                message = next(&mut observation.owners) => match message {
                    Some(Ok(_)) => true,
                    _ => { self.observation = None; return; },
                },
            };
            if relevant {
                break;
            }
        }
        // Coalesce the two bounded signal queues before reading current state.
        // Signals arriving during that read remain queued for one follow-up.
        for _ in 0..16 {
            let ready = std::future::poll_fn(|cx| {
                let property = Pin::new(&mut observation.properties).poll_next(cx);
                let owner = Pin::new(&mut observation.owners).poll_next(cx);
                Poll::Ready((property, owner))
            })
            .await;
            match ready {
                (Poll::Pending, Poll::Pending) => break,
                (Poll::Ready(None | Some(Err(_))), _) | (_, Poll::Ready(None | Some(Err(_)))) => {
                    self.observation = None;
                    return;
                }
                _ => {}
            }
        }
    }
}

pub(super) async fn observe(node: Arc<Node>) {
    observe_queries(LinuxCost { observation: None }, move |cost| {
        node.set_network_cost(cost)
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

    struct PrivateBus {
        child: std::process::Child,
        _directory: tempfile::TempDir,
        address: String,
    }
    impl Drop for PrivateBus {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
    impl PrivateBus {
        async fn start() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let address = format!("unix:path={}/bus", directory.path().display());
            let child = std::process::Command::new("dbus-daemon")
                .args(["--session", "--nofork", &format!("--address={address}")])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("Linux native fixture requires dbus-daemon");
            let bus = Self {
                child,
                _directory: directory,
                address,
            };
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    if bus.connect().await.is_ok() {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("private D-Bus daemon startup");
            bus
        }
        async fn connect(&self) -> zbus::Result<zbus::Connection> {
            zbus::connection::Builder::address(self.address.as_str())?
                .build()
                .await
        }
    }

    #[derive(Clone, Default)]
    struct MeteredService {
        value: Arc<AtomicU32>,
        failed: Arc<AtomicBool>,
        queries: Arc<AtomicUsize>,
    }
    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl MeteredService {
        #[zbus(property)]
        fn metered(&self) -> zbus::fdo::Result<u32> {
            self.queries.fetch_add(1, Ordering::SeqCst);
            if self.failed.load(Ordering::SeqCst) {
                Err(zbus::fdo::Error::Failed("cost provider unavailable".into()))
            } else {
                Ok(self.value.load(Ordering::SeqCst))
            }
        }
    }
    async fn install(connection: &zbus::Connection, service: MeteredService) {
        connection.object_server().at(PATH, service).await.unwrap();
        connection.request_name(SERVICE).await.unwrap();
    }
    async fn invalidate(connection: &zbus::Connection, property: &str) {
        connection
            .emit_signal(
                None::<&str>,
                PATH,
                PROPERTIES,
                "PropertiesChanged",
                &(
                    SERVICE,
                    HashMap::<&str, zbus::zvariant::Value<'_>>::new(),
                    vec![property],
                ),
            )
            .await
            .unwrap();
    }
    async fn wake(source: &mut LinuxCost) {
        tokio::time::timeout(std::time::Duration::from_secs(2), source.changed())
            .await
            .expect("native property/service owner wake");
    }

    #[tokio::test]
    async fn native_metered_property_and_owner_changes_requery_current_cost() {
        let bus = PrivateBus::start().await;
        let server = bus.connect().await.unwrap();
        let service = MeteredService::default();
        service.value.store(2, Ordering::SeqCst);
        install(&server, service.clone()).await;
        let observation = Observation::register(bus.connect().await.unwrap())
            .await
            .unwrap();
        let mut source = LinuxCost {
            observation: Some(observation),
        };
        assert_eq!(source.current().await, NetworkCost::Unrestricted);
        assert_eq!(service.queries.load(Ordering::SeqCst), 1);

        service.value.store(1, Ordering::SeqCst);
        invalidate(&server, "Metered").await;
        wake(&mut source).await;
        assert_eq!(source.current().await, NetworkCost::Restricted);
        assert_eq!(service.queries.load(Ordering::SeqCst), 2);

        // An explicit repair reads even without a signal; proxy cache must not
        // conceal changed provider data.
        service.value.store(2, Ordering::SeqCst);
        assert_eq!(source.current().await, NetworkCost::Unrestricted);
        assert_eq!(service.queries.load(Ordering::SeqCst), 3);
        invalidate(&server, "Connectivity").await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), source.changed())
                .await
                .is_err()
        );

        service.failed.store(true, Ordering::SeqCst);
        assert_eq!(source.current().await, NetworkCost::Unknown);
        service.failed.store(false, Ordering::SeqCst);
        invalidate(&server, "Metered").await;
        wake(&mut source).await;
        assert_eq!(source.current().await, NetworkCost::Unrestricted);
        assert!(server.release_name(SERVICE).await.unwrap());
        wake(&mut source).await;
        assert_eq!(source.current().await, NetworkCost::Unknown);

        let replacement = bus.connect().await.unwrap();
        let replacement_service = MeteredService::default();
        replacement_service.value.store(1, Ordering::SeqCst);
        install(&replacement, replacement_service.clone()).await;
        wake(&mut source).await;
        assert_eq!(source.current().await, NetworkCost::Restricted);
        assert_eq!(replacement_service.queries.load(Ordering::SeqCst), 1);
        drop(source); // Owns both match streams and the observation connection.
    }
}

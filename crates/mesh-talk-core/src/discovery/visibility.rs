//! Announcement disclosure control, separate from application admission.
use super::scope::{self, LanInterface};
use std::io;
use std::net::SocketAddr;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

/// A shared discovery-only control. Suppression does not block TCP, conceal
/// already advertised identities, or retract queued/transmitted datagrams.
pub struct DiscoveryVisibility {
    public: RwLock<bool>,
    physical_lan_only: AtomicBool,
    inventory: Arc<dyn Fn() -> Vec<LanInterface> + Send + Sync>,
}

impl DiscoveryVisibility {
    /// Public routed probing follows the same visibility gate as LAN announcing.
    pub async fn is_public(&self) -> bool {
        *self.public.read().await
    }
    pub fn new(public: bool) -> Self {
        Self::with_inventory(public, scope::inventory)
    }

    pub(crate) fn with_inventory(
        public: bool,
        inventory: impl Fn() -> Vec<LanInterface> + Send + Sync + 'static,
    ) -> Self {
        Self {
            public: RwLock::new(public),
            physical_lan_only: AtomicBool::new(false),
            inventory: Arc::new(inventory),
        }
    }

    pub fn physical_lan_only(&self) -> bool {
        self.physical_lan_only.load(Ordering::Acquire)
    }

    pub async fn set_physical_lan_only(&self, enabled: bool) {
        let _send_guard = self.public.write().await;
        self.physical_lan_only.store(enabled, Ordering::Release);
    }

    pub(crate) fn initialize_physical_lan_only(&self, enabled: bool) -> io::Result<()> {
        let _guard = self
            .public
            .try_write()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "discovery already active"))?;
        self.physical_lan_only.store(enabled, Ordering::Release);
        Ok(())
    }

    pub(crate) async fn routed_send_guard(&self) -> Option<tokio::sync::RwLockReadGuard<'_, bool>> {
        let guard = self.public.read().await;
        if !*guard || self.physical_lan_only() {
            None
        } else {
            Some(guard)
        }
    }

    pub async fn routed_allowed(&self) -> bool {
        *self.public.read().await && !self.physical_lan_only()
    }

    pub(crate) fn physical_ipv4_addrs(&self) -> Vec<std::net::Ipv4Addr> {
        (self.inventory)()
            .into_iter()
            .filter_map(|interface| match interface.address {
                std::net::IpAddr::V4(ip) => Some(ip),
                _ => None,
            })
            .collect()
    }

    /// Observed interface scope, not a claim about a peer's physical location.
    pub(crate) fn discovery_scope(&self) -> Vec<LanInterface> {
        (self.inventory)()
    }

    pub(crate) fn accepts_source(&self, source: SocketAddr) -> bool {
        !self.physical_lan_only() || scope::permits(&(self.inventory)(), source)
    }

    /// Wait for in-flight announcement sends before changing visibility. Once
    /// disabling returns, subsequent sends cannot start until enabled again;
    /// packets already handed to the OS may still arrive afterward.
    pub async fn set_public(&self, public: bool) {
        *self.public.write().await = public;
    }

    /// Initialize only before discovery tasks start. A live send means the host
    /// violated startup ordering; fail rather than expose persisted private mode.
    pub(crate) fn initialize_public(&self, public: bool) -> io::Result<()> {
        let mut current = self
            .public
            .try_write()
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "discovery already active"))?;
        *current = public;
        Ok(())
    }

    pub(crate) async fn send_announce(
        &self,
        socket: &UdpSocket,
        bytes: &[u8],
        target: SocketAddr,
    ) -> io::Result<bool> {
        let public = self.public.read().await;
        if !*public {
            return Ok(false);
        }
        if self.physical_lan_only() {
            if target.ip() == std::net::IpAddr::V4(crate::transport::net::DISCOVERY_MULTICAST_GROUP)
            {
                // Never let the default route select an overlay for a scoped
                // announce. Each physical IPv4 source gets its own LAN socket.
                for interface in (self.inventory)() {
                    if let std::net::IpAddr::V4(source) = interface.address {
                        let sender = UdpSocket::bind((source, 0)).await?;
                        socket2::SockRef::from(&sender).set_multicast_if_v4(&source)?;
                        sender.set_multicast_ttl_v4(1)?;
                        sender.send_to(bytes, target).await?;
                    }
                }
                return Ok(true);
            }
            if !self.accepts_source(target) {
                return Ok(false);
            }
        }
        socket.send_to(bytes, target).await?;
        drop(public);
        Ok(true)
    }
}

impl Default for DiscoveryVisibility {
    fn default() -> Self {
        Self::new(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn physical_scope_does_not_override_invisibility_or_an_active_send() {
        let visibility = DiscoveryVisibility::default();
        let send = visibility.routed_send_guard().await.unwrap();
        let change = visibility.set_physical_lan_only(true);
        tokio::pin!(change);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut change)
                .await
                .is_err()
        );
        drop(send);
        change.await;
        assert!(!visibility.routed_allowed().await);
        visibility.set_public(false).await;
        visibility.set_physical_lan_only(false).await;
        assert!(
            !visibility.routed_allowed().await,
            "scope changes must preserve invisibility"
        );
    }

    #[tokio::test]
    async fn physical_scope_is_initialized_before_first_send_and_changes_live() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let visibility = DiscoveryVisibility::with_inventory(true, || {
            vec![super::super::scope::LanInterface {
                address: "192.168.1.2".parse().unwrap(),
                prefix: 24,
                index: Some(1),
            }]
        });
        visibility.initialize_physical_lan_only(true).unwrap();
        assert!(
            !visibility
                .send_announce(&sender, b"identity", receiver.local_addr().unwrap())
                .await
                .unwrap()
        );
        visibility.set_physical_lan_only(false).await;
        assert!(
            visibility
                .send_announce(&sender, b"public", receiver.local_addr().unwrap())
                .await
                .unwrap()
        );
        let mut buf = [0; 128];
        receiver.recv_from(&mut buf).await.unwrap();
        visibility.set_physical_lan_only(true).await;
        assert!(
            !visibility
                .send_announce(&sender, b"identity", receiver.local_addr().unwrap())
                .await
                .unwrap()
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                receiver.recv_from(&mut buf)
            )
            .await
            .is_err()
        );
        assert!(!visibility.routed_allowed().await);
    }

    #[tokio::test]
    async fn private_discovery_never_sends_an_announce() {
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let visibility = DiscoveryVisibility::new(false);
        visibility
            .send_announce(&sender, b"identity", receiver.local_addr().unwrap())
            .await
            .unwrap();
        let mut buf = [0; 128];
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                receiver.recv_from(&mut buf)
            )
            .await
            .is_err(),
            "private discovery disclosed an announcement"
        );
    }

    #[tokio::test]
    async fn disabling_waits_for_active_send_guards_then_blocks_future_sends() {
        let visibility = DiscoveryVisibility::default();
        let in_flight_send = visibility.public.read().await;
        let disable = visibility.set_public(false);
        tokio::pin!(disable);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut disable)
                .await
                .is_err(),
            "disable must wait for the active send guard"
        );
        drop(in_flight_send);
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut disable)
            .await
            .unwrap();
        let receiver = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert!(
            !visibility
                .send_announce(&sender, b"identity", receiver.local_addr().unwrap())
                .await
                .unwrap()
        );
        let mut buf = [0; 128];
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                receiver.recv_from(&mut buf)
            )
            .await
            .is_err()
        );
    }
}

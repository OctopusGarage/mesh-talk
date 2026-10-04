//! Announcement disclosure control, separate from application admission.
use std::io;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

/// A shared discovery-only control. Suppression does not block TCP, conceal
/// already advertised identities, or retract queued/transmitted datagrams.
pub struct DiscoveryVisibility {
    public: RwLock<bool>,
}

impl DiscoveryVisibility {
    pub fn new(public: bool) -> Self {
        Self {
            public: RwLock::new(public),
        }
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
        assert!(!visibility
            .send_announce(&sender, b"identity", receiver.local_addr().unwrap())
            .await
            .unwrap());
        let mut buf = [0; 128];
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(100),
            receiver.recv_from(&mut buf)
        )
        .await
        .is_err());
    }
}

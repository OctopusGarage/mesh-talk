//! Low-level socket helpers shared by the node's discovery (the CLI binary and
//! the desktop runtime both bind the same kind of reuse UDP socket and join the
//! discovery multicast group on every local interface).

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UdpSocket};

/// The default UDP port for the node's signed-announce discovery. Distinct
/// from the legacy plaintext discovery port so the two protocols never collide.
pub const DEFAULT_DISCOVERY_PORT: u16 = 47474;

/// The IPv4 multicast group the node's discovery announces are sent to and
/// listened on. Mirrors LocalSend's `224.0.0.167`: a link-local (224.0.0.0/24)
/// group that stays on the LAN and survives where limited broadcast
/// (`255.255.255.255`) gets dropped or pinned to a single interface (e.g. macOS).
pub const DISCOVERY_MULTICAST_GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 167);

/// Bind the shared discovery UDP socket: `SO_REUSEADDR` + `SO_REUSEPORT` (so two
/// nodes on one host can share the port) + `SO_BROADCAST`, bound to
/// `0.0.0.0:<discovery_port>`, then join [`DISCOVERY_MULTICAST_GROUP`] on every
/// non-loopback IPv4 interface (plus an unspecified-interface fallback). The same
/// socket is used to both send announces and listen.
pub fn discovery_socket(discovery_port: u16) -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let bind_addr: SocketAddr = (Ipv4Addr::UNSPECIFIED, discovery_port).into();
    socket.bind(&bind_addr.into())?;
    socket.set_nonblocking(true)?;
    let sock = UdpSocket::from_std(socket.into())?;

    join_discovery_group_all_ifaces(&sock);

    Ok(sock)
}

/// Join [`DISCOVERY_MULTICAST_GROUP`] on every current non-loopback IPv4 interface
/// (plus an unspecified-interface fallback). A single interface (the default route)
/// is not enough on a multi-NIC / multi-WiFi host. Individual joins may fail (the
/// interface won't accept it, or it's already joined); that's fine, so per-join
/// errors are ignored. Called once at bind time and again periodically so interfaces
/// that come up after launch (e.g. WiFi associating late) get joined.
pub fn join_discovery_group_all_ifaces(sock: &UdpSocket) {
    for iface in ipv4_interface_addrs() {
        let _ = sock.join_multicast_v4(DISCOVERY_MULTICAST_GROUP, iface);
    }
    // Fallback: let the OS pick the interface, in case enumeration missed one.
    let _ = sock.join_multicast_v4(DISCOVERY_MULTICAST_GROUP, Ipv4Addr::UNSPECIFIED);
}

/// The local non-loopback IPv4 interface addresses, used as the per-interface
/// join points for the discovery multicast group and as the source set for the
/// unicast /24 scan. Errors enumerating interfaces degrade to an empty list (the
/// unspecified-interface fallback still applies).
pub fn ipv4_interface_addrs() -> Vec<Ipv4Addr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback())
        .filter_map(|iface| match iface.ip() {
            std::net::IpAddr::V4(addr) => Some(addr),
            std::net::IpAddr::V6(_) => None,
        })
        .collect()
}

/// Idle time before TCP keepalive probes start on a peer connection. The node's
/// connections are short request-response sync rounds, but a single round can stay
/// open across several recv `await`s; keepalive lets a dead peer (yanked cable, NAT
/// drop) be detected instead of hanging until the idle timeout, and holds NAT/firewall
/// state for a connection that legitimately pauses between rounds.
const TCP_KEEPALIVE_IDLE: Duration = Duration::from_secs(20);

/// Enable `SO_KEEPALIVE` (with a sane idle interval) on an established TCP stream.
/// Best-effort: a failure to set the option is not fatal to the connection, so the
/// error is swallowed. No wire-protocol change — this is purely socket-level.
pub fn set_tcp_keepalive(stream: &TcpStream) {
    use socket2::{SockRef, TcpKeepalive};
    let keepalive = TcpKeepalive::new().with_time(TCP_KEEPALIVE_IDLE);
    let _ = SockRef::from(stream).set_tcp_keepalive(&keepalive);
}

/// Bind the node's inbound TCP listener dual-stack: prefer IPv6 `[::]` with
/// IPv4-mapped addresses accepted (so a single socket serves both v4 and v6 peers,
/// including link-local IPv6), and fall back to IPv4 `0.0.0.0` if the v6 bind fails
/// (some hosts disable IPv6). Mirrors LocalSend's "bind v6, fall back so v4 still
/// serves" approach. `port` may be 0 for an OS-assigned port.
pub async fn bind_dual_stack_listener(port: u16) -> std::io::Result<TcpListener> {
    match bind_v6_dual_stack(port) {
        Ok(listener) => Ok(listener),
        // IPv6 unavailable/disabled on this host — fall back to IPv4 so the node still serves.
        Err(_) => TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await,
    }
}

/// Bind an IPv6 listener on `[::]:port` with `IPV6_V6ONLY` disabled, so the one
/// socket accepts both IPv6 and IPv4-mapped (v4) connections.
fn bind_v6_dual_stack(port: u16) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::STREAM, None)?;
    // Accept IPv4-mapped connections on this v6 socket (dual-stack).
    socket.set_only_v6(false)?;
    socket.set_reuse_address(true)?;
    let bind_addr: SocketAddr = (Ipv6Addr::UNSPECIFIED, port).into();
    socket.bind(&bind_addr.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binds_a_reuse_broadcast_socket() {
        // Port 0 → OS-assigned; just proves the bind + option-setting path works.
        let a = discovery_socket(0).unwrap();
        assert!(a.local_addr().is_ok());
    }

    #[tokio::test]
    async fn dual_stack_listener_binds() {
        // Port 0 → OS-assigned. Proves the dual-stack (or IPv4-fallback) bind path works
        // and yields a usable, listening socket.
        let listener = bind_dual_stack_listener(0).await.expect("bind dual-stack");
        let addr = listener.local_addr().expect("local addr");
        assert_ne!(addr.port(), 0);
    }

    #[tokio::test]
    async fn dual_stack_listener_accepts_an_ipv4_peer() {
        // A v6 dual-stack socket must still accept an IPv4 connection (IPv4-mapped); if the
        // host fell back to IPv4-only, this is a plain v4 connect. Either way v4 peers reach us.
        let listener = bind_dual_stack_listener(0).await.expect("bind dual-stack");
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move { listener.accept().await.map(|_| ()) });
        let client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("v4 connect");
        set_tcp_keepalive(&client); // exercises the keepalive helper on a live socket
        server.await.expect("join").expect("accept v4 peer");
    }
}

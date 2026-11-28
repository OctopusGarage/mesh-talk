//! Compatibility exports for Mesh Talk networking.

pub(crate) use mesh_connect::net::canonical_peer_ip;
#[cfg(windows)]
pub(crate) use mesh_connect::net::send_discovery_multicast;
pub use mesh_connect::net::{
    DISCOVERY_MULTICAST_GROUP, bind_dual_stack_listener, discovery_socket, ipv4_interface_addrs,
    join_discovery_group_all_ifaces, routed_ipv6_socket, set_tcp_keepalive,
};

/// The signed-announce discovery port.
pub const DEFAULT_DISCOVERY_PORT: u16 = 47474;

/// The managed post office's dedicated routed discovery port.
pub const POST_OFFICE_DISCOVERY_PORT: u16 = 47478;

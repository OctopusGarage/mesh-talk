//! Conservative interface-based discovery scope. This is disclosure policy, not
//! a firewall: routes, spoofed source addresses and already known contacts are
//! outside this policy's guarantees.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Clone, Debug)]
pub(crate) struct LanInterface {
    pub address: IpAddr,
    pub prefix: u8,
    pub index: Option<u32>,
}

/// Unknown/renamed adapters fail closed. A VPN's private IPv4 address does not
/// make it physical. Point-to-point adapters and common overlay names never pass.
fn physical_name(name: &str, point_to_point: bool) -> bool {
    let name = name.to_ascii_lowercase();
    !point_to_point
        && ![
            "tailscale",
            "wireguard",
            "wintun",
            "vpn",
            "tunnel",
            "virtual",
            "vethernet",
            "zerotier",
            "tap",
            "tun",
            "ppp",
            "hamachi",
            "hyper-v",
        ]
        .iter()
        .any(|part| name.contains(part))
        && ["en", "eth", "wlan", "wlp", "wlx", "wi-fi", "wifi"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

pub(crate) fn inventory() -> Vec<LanInterface> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| !iface.is_loopback() && physical_name(&iface.name, iface.is_p2p))
        .filter_map(|iface| match iface.addr {
            if_addrs::IfAddr::V4(addr) if !overlay(addr.ip) && addr.prefixlen > 0 => {
                Some(LanInterface {
                    address: addr.ip.into(),
                    prefix: addr.prefixlen,
                    index: iface.index,
                })
            }
            if_addrs::IfAddr::V6(addr) if addr.ip.is_unicast_link_local() => Some(LanInterface {
                address: addr.ip.into(),
                prefix: addr.prefixlen,
                index: iface.index,
            }),
            _ => None,
        })
        .collect()
}

fn overlay(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    a == 100 && (64..128).contains(&b)
}

pub(crate) fn permits(interfaces: &[LanInterface], target: SocketAddr) -> bool {
    let target = match target {
        SocketAddr::V6(addr) => addr
            .ip()
            .to_ipv4_mapped()
            .map(|ip| SocketAddr::new(ip.into(), addr.port()))
            .unwrap_or(target),
        _ => target,
    };
    match target {
        SocketAddr::V4(target) => {
            let ip = *target.ip();
            if overlay(ip) || ip.is_unspecified() {
                return false;
            }
            interfaces.iter().any(|iface| match iface.address {
                IpAddr::V4(own) if (1..=32).contains(&iface.prefix) => {
                    let mask = u32::MAX << (32 - iface.prefix);
                    (u32::from(own) & mask) == (u32::from(ip) & mask)
                }
                _ => false,
            })
        }
        SocketAddr::V6(target) => {
            target.ip().is_unicast_link_local()
                && target.scope_id() != 0
                && interfaces
                    .iter()
                    .any(|iface| iface.address.is_ipv6() && iface.index == Some(target.scope_id()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn subnet_and_interface_scope_rejects_overlay_and_routed_addresses() {
        let interfaces = vec![
            LanInterface {
                address: "192.168.10.5".parse().unwrap(),
                prefix: 24,
                index: Some(3),
            },
            LanInterface {
                address: "fe80::1".parse().unwrap(),
                prefix: 64,
                index: Some(3),
            },
        ];
        assert!(permits(
            &interfaces,
            "192.168.10.255:47474".parse().unwrap()
        ));
        for addr in [
            "192.168.11.4:47474",
            "100.64.1.2:47474",
            "[fd7a:115c:a1e0::1]:47474",
            "[2001:db8::1]:47474",
            "[fe80::2%4]:47474",
            "[fe80::2]:47474",
        ] {
            assert!(!permits(&interfaces, addr.parse().unwrap()), "{addr}");
        }
        assert!(permits(&interfaces, "[fe80::2%3]:47474".parse().unwrap()));
        assert!(!permits(&[], "192.168.10.2:47474".parse().unwrap()));
    }
    #[test]
    fn private_addresses_do_not_turn_virtual_adapters_into_physical_lan() {
        for name in [
            "utun0",
            "tun0",
            "wg0",
            "tailscale0",
            "Ethernet VPN",
            "vEthernet",
            "unknown0",
            "en-tunnel",
        ] {
            assert!(!physical_name(name, false), "{name}");
        }
        assert!(!physical_name("en0", true));
        for name in ["en0", "eth0", "wlp2s0", "Wi-Fi", "Ethernet 2"] {
            assert!(physical_name(name, false), "{name}");
        }
    }
}

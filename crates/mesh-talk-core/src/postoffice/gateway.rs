//! A local gateway to one explicitly pinned, remote post office.
//!
//! The gateway shares the remote relay's *own signed* identity with opted-in
//! local bridge clients and forwards TCP bytes to it. The clients' Noise
//! handshake runs end to end with that relay, so this node cannot impersonate
//! it or read its sessions.

use crate::discovery::Announce;
use crate::discovery::query::{decode_response, encode_query};
use crate::transport::net::bind_dual_stack_listener;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::task::JoinHandle;

const QUERY_TIMEOUT: Duration = Duration::from_secs(4);
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_LIMIT: Duration = Duration::from_secs(3600);
const MAX_PROXY_CONNECTIONS: usize = 256;

/// Probe one explicitly configured endpoint. A fresh nonce, the remote signed
/// announcement, and the configured fingerprint all have to agree.
pub async fn query_pinned_relay(remote: SocketAddr, expected_id: &str) -> Option<Announce> {
    let bind = match remote.ip() {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    let nonce = uuid::Uuid::new_v4().into_bytes();
    socket.send_to(&encode_query(nonce), remote).await.ok()?;
    let mut bytes = [0u8; 2048];
    let (len, source) = tokio::time::timeout(QUERY_TIMEOUT, socket.recv_from(&mut bytes))
        .await
        .ok()?
        .ok()?;
    if source != remote {
        return None;
    }
    let announce = decode_response(&bytes[..len], nonce)?;
    (announce.post_office
        && announce.user_id == expected_id
        && announce.account_id().is_some()
        && announce.name.len() <= 1024)
        .then_some(announce)
}

/// Repeatedly refresh one remote relay and expose its signed proof only to
/// opted-in clients through the local bridge directory. Its signed TCP port
/// must differ from this relay's own because the proxy binds that port locally.
pub async fn run_bridge_gateway(
    gateway_proof: Arc<Mutex<Option<Announce>>>,
    local_tcp_port: u16,
    remote_discovery: SocketAddr,
    expected_id: String,
) {
    let mut proxy: Option<(u16, JoinHandle<()>)> = None;
    let mut misses = 0u8;
    loop {
        if proxy.as_ref().is_some_and(|(_, task)| task.is_finished()) {
            proxy.take();
            *gateway_proof
                .lock()
                .expect("gateway proof lock not poisoned") = None;
        }
        if let Some(announce) = query_pinned_relay(remote_discovery, &expected_id).await {
            misses = 0;
            let port = announce.tcp_port;
            if port != local_tcp_port {
                if proxy.as_ref().is_some_and(|(active, _)| *active != port)
                    && let Some((_, task)) = proxy.take()
                {
                    task.abort();
                    *gateway_proof
                        .lock()
                        .expect("gateway proof lock not poisoned") = None;
                }
                if proxy.is_none() {
                    match bind_dual_stack_listener(port).await {
                        Ok(listener) => {
                            let target = SocketAddr::new(remote_discovery.ip(), port);
                            proxy = Some((port, tokio::spawn(proxy_connections(listener, target))));
                        }
                        Err(error) => {
                            log::warn!("bridge gateway could not bind tcp/{port}: {error}")
                        }
                    }
                }
                if proxy.is_some() {
                    *gateway_proof
                        .lock()
                        .expect("gateway proof lock not poisoned") = Some(announce);
                }
            } else {
                if let Some((_, task)) = proxy.take() {
                    task.abort();
                }
                *gateway_proof
                    .lock()
                    .expect("gateway proof lock not poisoned") = None;
                log::warn!("bridge peer uses this relay's tcp/{port}; configure distinct ports");
            }
        } else {
            misses = misses.saturating_add(1);
            if misses >= 3 {
                *gateway_proof
                    .lock()
                    .expect("gateway proof lock not poisoned") = None;
            }
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}

async fn proxy_connections(listener: TcpListener, remote: SocketAddr) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_PROXY_CONNECTIONS));
    loop {
        let Ok(permit) = Arc::clone(&slots).acquire_owned().await else {
            return;
        };
        let Ok((mut client, _)) = listener.accept().await else {
            return;
        };
        tokio::spawn(async move {
            let _permit = permit;
            let Ok(Ok(mut upstream)) =
                tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(remote)).await
            else {
                return;
            };
            let _ = tokio::time::timeout(
                SESSION_LIMIT,
                copy_bidirectional(&mut client, &mut upstream),
            )
            .await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::query::{decode_query, encode_response};
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn gateway_publishes_only_a_pinned_relay_with_a_distinct_port() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let port_holder = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote_tcp_port = port_holder.local_addr().unwrap().port();
        drop(port_holder);
        let proof = Announce::new_post_office_with_account(
            &identity,
            &account,
            "remote relay",
            remote_tcp_port,
        );
        let expected_id = identity.user_id();
        let local_tcp_port = if remote_tcp_port == u16::MAX {
            remote_tcp_port - 1
        } else {
            remote_tcp_port + 1
        };
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = socket.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (len, source) = socket.recv_from(&mut buf).await.unwrap();
            let nonce = decode_query(&buf[..len]).unwrap();
            socket
                .send_to(&encode_response(nonce, &proof, &identity).unwrap(), source)
                .await
                .unwrap();
        });
        let shared = Arc::new(Mutex::new(None));
        let gateway = tokio::spawn(run_bridge_gateway(
            Arc::clone(&shared),
            local_tcp_port,
            remote,
            expected_id,
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if shared.lock().unwrap().is_some() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pinned proof should become available after binding the proxy");
        assert_eq!(
            shared.lock().unwrap().as_ref().unwrap().tcp_port,
            remote_tcp_port
        );
        responder.await.unwrap();
        gateway.abort();
    }

    #[tokio::test]
    async fn proxy_forwards_bytes_to_its_explicit_upstream() {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote = upstream.local_addr().unwrap();
        let local = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_addr = local.local_addr().unwrap();
        let proxy = tokio::spawn(proxy_connections(local, remote));
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut request = [0u8; 4];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(&request, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });
        let mut client = TcpStream::connect(local_addr).await.unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut response = [0u8; 4];
        tokio::time::timeout(Duration::from_secs(5), client.read_exact(&mut response))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&response, b"pong");
        upstream_task.await.unwrap();
        proxy.abort();
    }

    #[tokio::test]
    async fn relay_query_rejects_a_valid_proof_sent_from_another_address() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let expected_id = identity.user_id();
        let proof = Announce::new_post_office_with_account(&identity, &account, "remote", 47475);
        let pinned = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let pinned_addr = pinned.local_addr().unwrap();
        let unexpected = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (len, source) = pinned.recv_from(&mut buf).await.unwrap();
            let nonce = decode_query(&buf[..len]).unwrap();
            unexpected
                .send_to(&encode_response(nonce, &proof, &identity).unwrap(), source)
                .await
                .unwrap();
        });
        assert!(
            query_pinned_relay(pinned_addr, &expected_id)
                .await
                .is_none()
        );
        responder.await.unwrap();
    }
}

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

#[derive(Clone, Debug)]
pub struct GatewayProof {
    pub announce: Announce,
    pub proxy_port: u16,
}

impl std::ops::Deref for GatewayProof {
    type Target = Announce;

    fn deref(&self) -> &Self::Target {
        &self.announce
    }
}

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
        && announce.valid_presence())
    .then_some(announce)
}

/// Repeatedly refresh one remote relay and expose its signed proof only to
/// opted-in clients through the local bridge directory. The default proxy
/// binds the remote signed port, so it must differ from this relay's own.
pub async fn run_bridge_gateway(
    gateway_proof: Arc<Mutex<Option<GatewayProof>>>,
    local_tcp_port: u16,
    remote_discovery: SocketAddr,
    expected_id: String,
) {
    run_bridge_gateway_on_port(
        gateway_proof,
        local_tcp_port,
        remote_discovery,
        expected_id,
        None,
    )
    .await;
}

/// `proxy_bind_port = Some(0)` asks the OS for an available local port. The
/// directory advertises the chosen port separately from the hub's signed port.
pub async fn run_bridge_gateway_on_port(
    gateway_proof: Arc<Mutex<Option<GatewayProof>>>,
    local_tcp_port: u16,
    remote_discovery: SocketAddr,
    expected_id: String,
    proxy_bind_port: Option<u16>,
) {
    let mut proxy: Option<(u16, u16, JoinHandle<()>)> = None;
    let mut misses = 0u8;
    loop {
        if proxy
            .as_ref()
            .is_some_and(|(_, _, task)| task.is_finished())
        {
            proxy.take();
            *gateway_proof
                .lock()
                .expect("gateway proof lock not poisoned") = None;
        }
        if let Some(announce) = query_pinned_relay(remote_discovery, &expected_id).await {
            misses = 0;
            let port = announce.tcp_port;
            let bind_port = proxy_bind_port.unwrap_or(port);
            if bind_port != local_tcp_port || bind_port == 0 {
                if proxy.as_ref().is_some_and(|(active, _, _)| *active != port)
                    && let Some((_, _, task)) = proxy.take()
                {
                    task.abort();
                    *gateway_proof
                        .lock()
                        .expect("gateway proof lock not poisoned") = None;
                }
                if proxy.is_none() {
                    match bind_dual_stack_listener(bind_port).await {
                        Ok(listener) => {
                            let proxy_port = listener.local_addr().expect("bound listener").port();
                            let target = SocketAddr::new(remote_discovery.ip(), port);
                            proxy = Some((
                                port,
                                proxy_port,
                                tokio::spawn(proxy_connections(listener, target)),
                            ));
                        }
                        Err(error) => {
                            log::warn!("bridge gateway could not bind tcp/{bind_port}: {error}")
                        }
                    }
                }
                if let Some((_, proxy_port, _)) = proxy.as_ref() {
                    *gateway_proof
                        .lock()
                        .expect("gateway proof lock not poisoned") = Some(GatewayProof {
                        announce,
                        proxy_port: *proxy_port,
                    });
                }
            } else {
                if let Some((_, _, task)) = proxy.take() {
                    task.abort();
                }
                *gateway_proof
                    .lock()
                    .expect("gateway proof lock not poisoned") = None;
                log::warn!(
                    "bridge proxy would use this relay's tcp/{local_tcp_port}; configure a different proxy port"
                );
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn respond_with_current_proof(
        socket: UdpSocket,
        identity: DeviceIdentity,
        current: Arc<Mutex<Option<Announce>>>,
        queries: Arc<AtomicUsize>,
    ) {
        let mut buf = [0u8; 2048];
        loop {
            let (len, source) = socket.recv_from(&mut buf).await.unwrap();
            let nonce = decode_query(&buf[..len]).unwrap();
            let proof = current.lock().unwrap().clone();
            let response = proof
                .as_ref()
                .map(|proof| encode_response(nonce, proof, &identity).unwrap())
                .unwrap_or_else(|| vec![0]);
            socket.send_to(&response, source).await.unwrap();
            queries.fetch_add(1, Ordering::Release);
        }
    }

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
    async fn gateway_advertises_an_independent_proxy_port_and_forwards_bytes() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let remote_port = upstream.local_addr().unwrap().port();
        let proof = Announce::new_post_office_with_account(&identity, &account, "hub", remote_port);
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = socket.local_addr().unwrap();
        let expected_id = identity.user_id();
        let responder = tokio::spawn(respond_with_current_proof(
            socket,
            identity,
            Arc::new(Mutex::new(Some(proof))),
            Arc::new(AtomicUsize::new(0)),
        ));
        let shared = Arc::new(Mutex::new(None));
        let gateway = tokio::spawn(run_bridge_gateway_on_port(
            Arc::clone(&shared),
            47475,
            remote,
            expected_id,
            Some(0),
        ));
        let proxy_port = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(proof) = shared.lock().unwrap().as_ref() {
                    break proof.proxy_port;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_ne!(proxy_port, remote_port);
        assert_ne!(proxy_port, 47475);
        let upstream_task = tokio::spawn(async move {
            let (mut stream, _) = upstream.accept().await.unwrap();
            let mut message = [0; 4];
            stream.read_exact(&mut message).await.unwrap();
            assert_eq!(&message, b"ping");
            stream.write_all(b"pong").await.unwrap();
        });
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, proxy_port))
            .await
            .unwrap();
        client.write_all(b"ping").await.unwrap();
        let mut response = [0; 4];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"pong");
        upstream_task.await.unwrap();
        gateway.abort();
        responder.abort();
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

    #[tokio::test]
    async fn gateway_waits_for_its_pinned_proxy_port_to_become_available() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let expected_id = identity.user_id();
        let occupied = bind_dual_stack_listener(0).await.unwrap();
        let remote_tcp_port = occupied.local_addr().unwrap().port();
        assert!(bind_dual_stack_listener(remote_tcp_port).await.is_err());
        let proof =
            Announce::new_post_office_with_account(&identity, &account, "remote", remote_tcp_port);
        let current = Arc::new(Mutex::new(Some(proof)));
        let queries = Arc::new(AtomicUsize::new(0));
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = socket.local_addr().unwrap();
        let responder = tokio::spawn(respond_with_current_proof(
            socket,
            identity,
            Arc::clone(&current),
            Arc::clone(&queries),
        ));
        let shared = Arc::new(Mutex::new(None));
        let gateway = tokio::spawn(run_bridge_gateway(
            Arc::clone(&shared),
            remote_tcp_port.wrapping_add(1),
            remote,
            expected_id,
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while queries.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(shared.lock().unwrap().is_none());
        drop(occupied);
        tokio::time::timeout(Duration::from_secs(7), async {
            while shared.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the next signed refresh should bind the now-free proxy port");
        responder.abort();
        gateway.abort();
    }

    #[tokio::test]
    async fn gateway_withdraws_stale_proof_after_repeated_invalid_replies() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let expected_id = identity.user_id();
        let reserved = bind_dual_stack_listener(0).await.unwrap();
        let remote_tcp_port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let proof =
            Announce::new_post_office_with_account(&identity, &account, "remote", remote_tcp_port);
        let current = Arc::new(Mutex::new(Some(proof)));
        let queries = Arc::new(AtomicUsize::new(0));
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = socket.local_addr().unwrap();
        let responder = tokio::spawn(respond_with_current_proof(
            socket,
            identity,
            Arc::clone(&current),
            Arc::clone(&queries),
        ));
        let shared = Arc::new(Mutex::new(None));
        let gateway = tokio::spawn(run_bridge_gateway_on_port(
            Arc::clone(&shared),
            remote_tcp_port.wrapping_add(1),
            remote,
            expected_id,
            // This test exercises proof withdrawal, not fixed-port binding.
            // Keep the proxy port reserved by the OS across parallel tests.
            Some(0),
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while shared.lock().unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the pinned relay should first appear in the bridge directory");

        *current.lock().unwrap() = None;
        tokio::time::timeout(Duration::from_secs(25), async {
            while shared.lock().unwrap().is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("invalid refreshes must withdraw the advertised relay proof");
        assert!(queries.load(Ordering::Acquire) >= 4);
        responder.abort();
        gateway.abort();
    }

    #[tokio::test]
    async fn gateway_does_not_proxy_on_its_own_relay_port() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let expected_id = identity.user_id();
        let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_tcp_port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let proof =
            Announce::new_post_office_with_account(&identity, &account, "remote", local_tcp_port);
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
        responder.await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(shared.lock().unwrap().is_none());
        assert!(
            TcpStream::connect((Ipv4Addr::LOCALHOST, local_tcp_port))
                .await
                .is_err()
        );
        gateway.abort();
    }

    #[tokio::test]
    async fn gateway_replaces_the_proxy_when_a_pinned_relay_changes_tcp_port() {
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let expected_id = identity.user_id();
        let first = bind_dual_stack_listener(0).await.unwrap();
        let second = bind_dual_stack_listener(0).await.unwrap();
        let first_port = first.local_addr().unwrap().port();
        let second_port = second.local_addr().unwrap().port();
        // Keep both remote ports reserved so parallel tests cannot claim them.
        // The gateway's independent local proxy port is assigned by the OS.
        let local_tcp_port = first_port;
        let first_proof =
            Announce::new_post_office_with_account(&identity, &account, "remote", first_port);
        let second_proof =
            Announce::new_post_office_with_account(&identity, &account, "remote", second_port);
        let current = Arc::new(Mutex::new(Some(first_proof)));
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let remote = socket.local_addr().unwrap();
        let responder = tokio::spawn(respond_with_current_proof(
            socket,
            identity,
            Arc::clone(&current),
            Arc::new(AtomicUsize::new(0)),
        ));
        let shared = Arc::new(Mutex::new(None));
        let gateway = tokio::spawn(run_bridge_gateway_on_port(
            Arc::clone(&shared),
            local_tcp_port,
            remote,
            expected_id,
            Some(0),
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while shared.lock().unwrap().as_ref().map(|proof| proof.tcp_port) != Some(first_port) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_ne!(
            shared.lock().unwrap().as_ref().unwrap().proxy_port,
            first_port
        );
        assert_ne!(
            shared.lock().unwrap().as_ref().unwrap().proxy_port,
            second_port
        );
        *current.lock().unwrap() = Some(second_proof);
        tokio::time::timeout(Duration::from_secs(12), async {
            while shared.lock().unwrap().as_ref().map(|proof| proof.tcp_port) != Some(second_port) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("a signed refresh should replace the old proxy");
        responder.abort();
        gateway.abort();
    }
}

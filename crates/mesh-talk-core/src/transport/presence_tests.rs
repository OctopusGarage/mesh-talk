use super::*;
use crate::{discovery::Announce, identity::account::Account};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
const DEADLINE: Duration = Duration::from_secs(3);

async fn legacy_handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    identity: &DeviceIdentity,
    initiator: bool,
) -> Session {
    let mut hs = if initiator {
        Handshake::initiator(&identity.secret_bytes().1)
    } else {
        Handshake::responder(&identity.secret_bytes().1)
    }
    .unwrap();
    if initiator {
        write_frame(stream, &hs.write_message().unwrap())
            .await
            .unwrap();
        hs.read_message(&read_frame(stream).await.unwrap()).unwrap();
        write_frame(stream, &hs.write_message().unwrap())
            .await
            .unwrap();
    } else {
        hs.read_message(&read_frame(stream).await.unwrap()).unwrap();
        write_frame(stream, &hs.write_message().unwrap())
            .await
            .unwrap();
        hs.read_message(&read_frame(stream).await.unwrap()).unwrap();
    }
    let mut out = hs.into_session().unwrap();
    let own = bincode::serialize(&build_auth(identity, &out.handshake_hash)).unwrap();
    if initiator {
        let encrypted = out.session.encrypt(&own).unwrap();
        write_frame(stream, &encrypted).await.unwrap();
    }
    let encrypted = read_frame(stream).await.unwrap();
    let plaintext = out.session.decrypt(&encrypted).unwrap();
    // Exact pre-extension parser and verifier from the legacy transport.
    let peer: crate::transport::auth::AuthMessage = bincode::deserialize(&plaintext).unwrap();
    crate::transport::auth::verify_auth(&peer, &out.handshake_hash, &out.remote_static).unwrap();
    assert!(
        plaintext.len() > bincode::serialize(&peer).unwrap().len(),
        "new peer sent appended presence"
    );
    if !initiator {
        let encrypted = out.session.encrypt(&own).unwrap();
        write_frame(stream, &encrypted).await.unwrap();
    }
    out.session
}

#[tokio::test]
async fn exact_old_parser_interoperates_with_presence_over_handshake_in_both_roles() {
    for legacy_initiator in [true, false] {
        let (mut client, mut server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let aa = Announce::new_with_account(&a, &Account::generate(), "Alice", 1234);
        let ba = Announce::new_with_account(&b, &Account::generate(), "Bob", 4321);
        let server = tokio::spawn(async move {
            if legacy_initiator {
                let mut ch = SecureChannel::accept_with_presence(server_io, &b, Some(&ba), |p| {
                    p.announcement().is_none()
                })
                .await
                .unwrap();
                assert_eq!(ch.recv().await.unwrap(), b"ping");
                ch.send(b"pong").await.unwrap();
            } else {
                let mut session = legacy_handshake(&mut server_io, &b, false).await;
                let encrypted = read_frame(&mut server_io).await.unwrap();
                assert_eq!(session.decrypt(&encrypted).unwrap(), b"ping");
                write_frame(&mut server_io, &session.encrypt(b"pong").unwrap())
                    .await
                    .unwrap();
            }
        });
        tokio::time::timeout(DEADLINE, async {
            if legacy_initiator {
                let mut session = legacy_handshake(&mut client, &a, true).await;
                write_frame(&mut client, &session.encrypt(b"ping").unwrap())
                    .await
                    .unwrap();
                let encrypted = read_frame(&mut client).await.unwrap();
                assert_eq!(session.decrypt(&encrypted).unwrap(), b"pong");
            } else {
                let mut ch = SecureChannel::connect_with_presence(client, &a, None, Some(&aa))
                    .await
                    .unwrap();
                assert!(ch.peer_announcement().is_none());
                ch.send(b"ping").await.unwrap();
                assert_eq!(ch.recv().await.unwrap(), b"pong");
            }
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn malformed_presence_never_reaches_admission_or_receives_responder_auth() {
    let (mut client, server_io) = tokio::io::duplex(64 * 1024);
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let aa = Announce::new_with_account(&a, &Account::generate(), "Alice", 1234);
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        SecureChannel::accept_with_presence(server_io, &b, None, |_| {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        })
        .await
    });
    tokio::time::timeout(DEADLINE, async {
        let mut hs = Handshake::initiator(&a.secret_bytes().1).unwrap();
        write_frame(&mut client, &hs.write_message().unwrap())
            .await
            .unwrap();
        hs.read_message(&read_frame(&mut client).await.unwrap())
            .unwrap();
        write_frame(&mut client, &hs.write_message().unwrap())
            .await
            .unwrap();
        let mut out = hs.into_session().unwrap();
        let mut bytes = presence_auth::build(&a, &out.handshake_hash, Some(&aa)).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        write_frame(&mut client, &out.session.encrypt(&bytes).unwrap())
            .await
            .unwrap();
        assert!(matches!(
            server.await.unwrap(),
            Err(TransportError::IdentityMismatch)
        ));
        assert!(matches!(
            read_frame(&mut client).await,
            Err(TransportError::Io(_))
        ));
    })
    .await
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn invalid_own_presence_rejected_before_any_handshake_traffic() {
    for initiator in [true, false] {
        let (client, mut observer) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let invalid = Announce::new_with_account(&a, &Account::generate(), "Alice", 0);
        let result = tokio::time::timeout(DEADLINE, async {
            if initiator {
                SecureChannel::connect_with_presence(client, &a, None, Some(&invalid)).await
            } else {
                SecureChannel::accept_with_presence(client, &a, Some(&invalid), |_| true).await
            }
        })
        .await
        .unwrap();
        assert!(matches!(result, Err(TransportError::IdentityMismatch)));
        assert!(matches!(
            read_frame(&mut observer).await,
            Err(TransportError::Io(_))
        ));
    }
}

#[tokio::test]
async fn presence_both_directions_over_tcp_with_account_admission() {
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let a_account = Account::generate();
    let b_account = Account::generate();
    let a_announce = Announce::new_with_account(&a, &a_account, "Alice", 1234);
    let b_announce = Announce::new_with_account(&b, &b_account, "Bob", 4321);
    let expected_a = a_announce.clone();
    let expected_b = b_announce.clone();
    let a_id = a_account.account_id();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ch = SecureChannel::accept_with_presence(stream, &b, Some(&b_announce), |peer| {
            assert_eq!(peer.public_identity(), &expected_a.public());
            peer.announcement().and_then(Announce::account_id) == Some(a_id)
        })
        .await
        .unwrap();
        assert_eq!(ch.peer_announcement(), Some(&expected_a));
        assert_eq!(ch.recv().await.unwrap(), b"ping");
        ch.send(b"pong").await.unwrap();
    });
    tokio::time::timeout(DEADLINE, async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut ch = SecureChannel::connect_with_presence(
            stream,
            &a,
            Some(&expected_b.public()),
            Some(&a_announce),
        )
        .await
        .unwrap();
        assert_eq!(ch.peer_announcement(), Some(&expected_b));
        ch.send(b"ping").await.unwrap();
        assert_eq!(ch.recv().await.unwrap(), b"pong");
        server.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn legacy_and_presence_interoperate_in_both_directions() {
    for legacy_initiator in [true, false] {
        let (client, server_io) = tokio::io::duplex(64 * 1024);
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let aa = Announce::new_with_account(&a, &Account::generate(), "Alice", 1234);
        let ba = Announce::new_with_account(&b, &Account::generate(), "Bob", 4321);
        let expected_a = aa.clone();
        let expected_b = ba.clone();
        let server = tokio::spawn(async move {
            let mut ch = if legacy_initiator {
                SecureChannel::accept_with_presence(server_io, &b, Some(&ba), |peer| {
                    peer.announcement().is_none()
                })
                .await
                .unwrap()
            } else {
                SecureChannel::accept(server_io, &b).await.unwrap()
            };
            assert_eq!(
                ch.peer_announcement(),
                if legacy_initiator {
                    None
                } else {
                    Some(&expected_a)
                }
            );
            assert_eq!(ch.recv().await.unwrap(), b"ping");
            ch.send(b"pong").await.unwrap();
        });
        tokio::time::timeout(DEADLINE, async {
            let mut ch = if legacy_initiator {
                SecureChannel::connect(client, &a, None).await.unwrap()
            } else {
                SecureChannel::connect_with_presence(client, &a, None, Some(&aa))
                    .await
                    .unwrap()
            };
            assert_eq!(
                ch.peer_announcement(),
                if legacy_initiator {
                    Some(&expected_b)
                } else {
                    None
                }
            );
            ch.send(b"ping").await.unwrap();
            assert_eq!(ch.recv().await.unwrap(), b"pong");
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn verified_account_denial_happens_before_responder_auth() {
    let (client, server_io) = tokio::io::duplex(64 * 1024);
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let aa = Announce::new_with_account(&a, &Account::generate(), "Alice", 1234);
    let ba = Announce::new_with_account(&b, &Account::generate(), "Bob", 4321);
    let a_account = aa.account_id();
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let server = tokio::spawn(async move {
        SecureChannel::accept_with_presence(server_io, &b, Some(&ba), |peer| {
            assert_eq!(
                peer.announcement().and_then(Announce::account_id),
                a_account
            );
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            false
        })
        .await
    });
    tokio::time::timeout(DEADLINE, async {
        assert!(matches!(
            SecureChannel::connect_with_presence(client, &a, None, Some(&aa)).await,
            Err(TransportError::Io(_))
        ));
        assert!(matches!(
            server.await.unwrap(),
            Err(TransportError::AdmissionDenied)
        ));
    })
    .await
    .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

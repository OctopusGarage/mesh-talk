//! Device-only enrollment. No local account, profile or public presence is created.

use super::{
    AccountAccessSnapshot, AccountProfile, BackfillRecord, LinkingError, Node, NodeError,
    PairingCode,
    account_devices::AccountDevice,
    linking::{pairing_exchange, receive_pairing_backfill},
    session_pool::SessionPool,
};
use crate::{
    discovery::{Announce, Roster, query},
    identity::{
        account::Account,
        account_keystore,
        device::{DeviceIdentity, PublicIdentity},
        keystore,
    },
    transport::SecureChannel,
};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    net::{TcpStream, UdpSocket},
    sync::mpsc,
};

/// Signed, account-bound source selected before an accountless enrollment.
#[derive(Clone, Debug)]
pub struct EnrollmentSource {
    announce: Announce,
    address: SocketAddr,
}
impl EnrollmentSource {
    pub fn from_announce(announce: Announce, ip: IpAddr) -> Result<Self, NodeError> {
        if !announce.valid_presence()
            || announce.account_cert.is_none()
            || announce.post_office
            || announce.tcp_port == 0
        {
            return Err(NodeError::Linking(LinkingError::InvalidResponse));
        }
        let address = SocketAddr::new(ip, announce.tcp_port);
        Ok(Self { announce, address })
    }
    pub fn public_identity(&self) -> PublicIdentity {
        self.announce.public()
    }
    pub fn account_id(&self) -> String {
        self.announce
            .account_id()
            .expect("validated source account")
    }
    pub fn name(&self) -> &str {
        &self.announce.name
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

/// An accountless device. It sends anonymous discovery queries and pinned Noise
/// pairing requests, and never opens a Node or advertises a temporary account.
pub struct DeviceEnrollment {
    identity: DeviceIdentity,
    sessions: SessionPool,
}
impl DeviceEnrollment {
    pub fn new(identity: DeviceIdentity) -> Self {
        Self {
            identity,
            sessions: SessionPool::default(),
        }
    }
    pub fn public_identity(&self) -> PublicIdentity {
        self.identity.public()
    }
    /// Queries explicit LAN/VPN endpoints without publishing presence. An empty
    /// target list selects LAN multicast; callers can include multicast explicitly.
    /// Bounded to 64 targets, five seconds and 64 verified sources per call.
    pub async fn probe_sources(
        &self,
        targets: &[SocketAddr],
        timeout: Duration,
    ) -> Result<Vec<EnrollmentSource>, NodeError> {
        if targets.len() > 64 {
            return Err(NodeError::InvalidInput(
                "too many enrollment discovery targets".into(),
            ));
        }
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|e| NodeError::Channel(e.to_string()))?;
        socket
            .set_broadcast(true)
            .map_err(|e| NodeError::Channel(e.to_string()))?;
        let nonce = uuid::Uuid::new_v4().into_bytes();
        let packet = query::encode_query(nonce);
        let multicast = SocketAddr::new(
            crate::transport::net::DISCOVERY_MULTICAST_GROUP.into(),
            crate::transport::net::DEFAULT_DISCOVERY_PORT,
        );
        let selected = if targets.is_empty() {
            std::slice::from_ref(&multicast)
        } else {
            targets
        };
        let ipv6 = if selected.iter().any(SocketAddr::is_ipv6) {
            Some(
                UdpSocket::bind("[::]:0")
                    .await
                    .map_err(|e| NodeError::Channel(e.to_string()))?,
            )
        } else {
            None
        };
        for target in selected {
            let sender = if target.is_ipv4() {
                &socket
            } else {
                ipv6.as_ref().expect("IPv6 selected")
            };
            let _ = sender.send_to(&packet, target).await;
        }
        let deadline = tokio::time::Instant::now() + timeout.min(Duration::from_secs(5));
        let mut buffer = [0u8; 2048];
        let mut buffer_v6 = [0u8; 2048];
        let mut sources: Vec<EnrollmentSource> = Vec::new();
        while sources.len() < 64 {
            let receive = async {
                if let Some(ipv6) = &ipv6 {
                    tokio::select! {
                        value = socket.recv_from(&mut buffer) => value.map(|(size, origin)| (size, origin, false)),
                        value = ipv6.recv_from(&mut buffer_v6) => value.map(|(size, origin)| (size, origin, true)),
                    }
                } else {
                    socket
                        .recv_from(&mut buffer)
                        .await
                        .map(|(size, origin)| (size, origin, false))
                }
            };
            let Ok(Ok((size, origin, is_v6))) = tokio::time::timeout_at(deadline, receive).await
            else {
                break;
            };
            let bytes = if is_v6 {
                &buffer_v6[..size]
            } else {
                &buffer[..size]
            };
            let Some(announce) = query::decode_response(bytes, nonce) else {
                continue;
            };
            let Ok(mut source) = EnrollmentSource::from_announce(announce, origin.ip()) else {
                continue;
            };
            source.address = origin;
            source.address.set_port(source.announce.tcp_port);
            if source.public_identity() != self.identity.public()
                && !sources
                    .iter()
                    .any(|old| old.public_identity() == source.public_identity())
            {
                sources.push(source);
            }
        }
        Ok(sources)
    }
    pub async fn enroll(
        &self,
        source: &EnrollmentSource,
        code_hex: &str,
        history: bool,
    ) -> Result<EnrolledDevice, NodeError> {
        let code = PairingCode::from_hex(code_hex)
            .ok_or_else(|| NodeError::InvalidInput("invalid pairing code".into()))?;
        let public = source.public_identity();
        let permits = self
            .sessions
            .outbound()
            .await
            .map_err(|e| NodeError::Linking(LinkingError::Connection(e)))?;
        let started = Instant::now();
        let mut channel = tokio::time::timeout(
            self.sessions
                .authentication_timeout(&public, source.address),
            async {
                let stream = TcpStream::connect(source.address).await?;
                stream.set_nodelay(true)?;
                crate::transport::net::set_tcp_keepalive(&stream);
                SecureChannel::connect(stream, &self.identity, Some(&public)).await
            },
        )
        .await
        .map_err(|_| {
            self.sessions
                .authentication_timed_out(&public, source.address);
            NodeError::Linking(LinkingError::ResponseTimeout)
        })?
        .map_err(|e| NodeError::Linking(LinkingError::Connection(e)))?;
        channel.retain_connection_permits(permits);
        self.sessions
            .authentication_succeeded(&public, source.address, started.elapsed());
        // The responder's authenticated account proof must match the selected signed source.
        if channel.peer_announcement().is_none_or(|announce| {
            announce.public() != public
                || !announce.valid_presence()
                || announce.account_id() != Some(source.account_id())
        }) {
            return Err(NodeError::Linking(LinkingError::InvalidResponse));
        }
        let (response, created_at, access, profile) = pairing_exchange(
            &mut channel,
            &self.identity.public(),
            &public,
            &code,
            history,
            true,
            self.sessions.control_timeout(&public, source.address),
        )
        .await
        .map_err(|error| {
            NodeError::Linking(
                if matches!(error, LinkingError::Closed | LinkingError::InvalidResponse) {
                    LinkingError::UpgradeRequired
                } else {
                    error
                },
            )
        })?;
        let account =
            Account::from_secret_bytes_with_created_at(response.account_secret, created_at)
                .ok_or(NodeError::Linking(LinkingError::InvalidResponse))?;
        let access = access.ok_or(NodeError::Linking(LinkingError::UpgradeRequired))?;
        if !response.cert.verify()
            || response.cert.device_ed25519_pub != self.identity.public().ed25519_pub
            || response.cert.account_ed25519_pub != response.account_ed25519_pub
            || account.public().ed25519_pub != response.account_ed25519_pub
            || account.account_id() != source.account_id()
            || !access.valid_for(&response.account_ed25519_pub)
            || profile.as_ref().is_some_and(|profile| {
                !profile.verify()
                    || profile.account_id() != account.account_id()
                    || profile.created_at_ms != created_at
            })
        {
            return Err(NodeError::Linking(LinkingError::InvalidResponse));
        }
        let backfill = receive_pairing_backfill(&mut channel, history, || true).await?;
        let secrets = zeroize::Zeroizing::new(self.identity.secret_bytes());
        Ok(EnrolledDevice {
            identity: DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            account,
            source: source.clone(),
            certificate: response.cert,
            access,
            profile,
            history,
            backfill,
        })
    }
}
/// Validated enrollment material. Secrets are never included in Debug or exposed
/// as raw keys; the host publishes its private staging directory separately.
pub struct EnrolledDevice {
    identity: DeviceIdentity,
    account: Account,
    source: EnrollmentSource,
    certificate: crate::identity::account::DeviceCertificate,
    access: AccountAccessSnapshot,
    profile: Option<AccountProfile>,
    history: bool,
    backfill: Vec<BackfillRecord>,
}
impl std::fmt::Debug for EnrolledDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnrolledDevice")
            .field("account_id", &self.account_id())
            .field("secrets", &"[REDACTED]")
            .finish()
    }
}
impl EnrolledDevice {
    pub fn account_id(&self) -> String {
        self.account.account_id()
    }
    pub fn created_at_ms(&self) -> Option<u64> {
        self.account.created_at_ms()
    }
    pub fn access_snapshot(&self) -> &AccountAccessSnapshot {
        &self.access
    }
    pub fn profile(&self) -> Option<&AccountProfile> {
        self.profile.as_ref()
    }
    pub fn source_public_identity(&self) -> PublicIdentity {
        self.source.public_identity()
    }
    pub fn device_public_identity(&self) -> PublicIdentity {
        self.identity.public()
    }
    /// The caller owns an empty private staging directory and publishes it only
    /// after host registration/envelope validation. Existing material is refused.
    pub fn persist_profile(&self, directory: &Path, password: &str) -> Result<(), NodeError> {
        let invalid =
            || NodeError::InvalidInput("enrollment requires an empty staging directory".into());
        let meta = std::fs::symlink_metadata(directory).map_err(|_| invalid())?;
        if !meta.is_dir()
            || meta.file_type().is_symlink()
            || std::fs::read_dir(directory)
                .map_err(|_| invalid())?
                .next()
                .is_some()
        {
            return Err(invalid());
        }
        let secrets = zeroize::Zeroizing::new(self.identity.secret_bytes());
        let identity = DeviceIdentity::from_secret_bytes(secrets.0, secrets.1);
        let account = Account::from_secret_bytes_with_created_at(
            self.account.secret_bytes(),
            self.account.created_at_ms(),
        )
        .ok_or_else(invalid)?;
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &directory.join("events.log"),
            &directory.join("sent.log"),
            password,
        )
        .map_err(NodeError::Log)?;
        node.account_access
            .lock()
            .expect("account access mutex")
            .merge(self.access.clone())
            .map_err(NodeError::Log)?;
        let source = AccountDevice {
            public: self.source.public_identity(),
            certificate: self
                .source
                .announce
                .account_cert
                .clone()
                .expect("validated source certificate"),
        };
        {
            let mut devices = node.account_devices.lock().expect("account devices mutex");
            devices
                .insert(AccountDevice {
                    public: self.identity.public(),
                    certificate: self.certificate.clone(),
                })
                .map_err(NodeError::Log)?;
            devices.insert(source).map_err(NodeError::Log)?;
        }
        if let Some(profile) = &self.profile {
            node.receive_account_profile(profile)?;
        }
        node.set_device_sync_category(super::SyncCategory::History, self.history)?;
        node.import_account_backfill_for_account(&self.account_id(), &self.backfill)?;
        // Publish the account keystore last inside staging: interruption cannot
        // turn an unvalidated response into a runnable local account.
        keystore::save(
            &directory.join("identity.keystore"),
            password,
            &self.identity,
        )
        .map_err(|e| NodeError::Channel(e.to_string()))?;
        account_keystore::save(&directory.join("account.keystore"), password, &self.account)
            .map_err(|e| NodeError::Channel(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::{DiscoveryVisibility, Roster},
        identity::{account::Account, device::DeviceIdentity},
        node::Node,
    };
    use std::sync::{Arc, Mutex};
    use tokio::{net::TcpListener, sync::mpsc};

    #[tokio::test]
    async fn device_only_enrollment_persists_validated_source_without_target_account() {
        let source_dir = tempfile::tempdir().unwrap();
        let stage = tempfile::tempdir().unwrap();
        let (d, _) = mpsc::unbounded_channel();
        let (c, _) = mpsc::unbounded_channel();
        let (f, _) = mpsc::unbounded_channel();
        let source = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            d,
            c,
            f,
            &source_dir.path().join("events.log"),
            &source_dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        source.set_profile_display_name("Source nickname").unwrap();
        source
            .set_avatar(Some(vec![7; super::super::profile::MAX_AVATAR_BYTES]))
            .await
            .unwrap();
        let expected = source
            .publish_account_access_if(b"opaque credential bytes".to_vec(), |commit| commit())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let announce = source.signed_announce("Source nickname", addr.port());
        source
            .configure_privacy(
                source_dir.path(),
                "pw",
                &announce,
                Arc::new(DiscoveryVisibility::new(true)),
            )
            .unwrap();
        let selected = EnrollmentSource::from_announce(announce, addr.ip()).unwrap();
        let code = source.start_linking();
        let serving = source.clone();
        let task = tokio::spawn(async move {
            for _ in 0..2 {
                let (socket, _) = listener.accept().await.unwrap();
                let channel = serving.privacy_accept(socket).await.unwrap();
                assert!(channel.pairing_only());
                serving.serve_connection(channel).await;
            }
        });
        let device = DeviceIdentity::generate();
        let expected_device = device.public();
        let enrollment = DeviceEnrollment::new(device);
        let wrong = if let Some(rest) = code.strip_prefix('0') {
            format!("1{rest}")
        } else {
            format!("0{}", &code[1..])
        };
        assert!(enrollment.enroll(&selected, &wrong, false).await.is_err());
        assert!(std::fs::read_dir(stage.path()).unwrap().next().is_none());
        assert_eq!(
            source.linked_devices().len(),
            1,
            "wrong code cannot enroll an accountless device"
        );
        let enrolled = enrollment.enroll(&selected, &code, false).await.unwrap();
        assert_eq!(enrolled.account_id(), source.account_id());
        assert_eq!(enrolled.access_snapshot(), &expected);
        assert_eq!(enrolled.profile().unwrap().display_name, "Source nickname");
        assert_eq!(
            enrolled.profile().unwrap().avatar.as_ref().unwrap().len(),
            super::super::profile::MAX_AVATAR_BYTES
        );
        assert!(std::fs::read_dir(stage.path()).unwrap().next().is_none());
        enrolled.persist_profile(stage.path(), "pw").unwrap();
        assert_eq!(
            crate::identity::keystore::load(&stage.path().join("identity.keystore"), "pw")
                .unwrap()
                .public(),
            expected_device
        );
        let account =
            crate::identity::account_keystore::load(&stage.path().join("account.keystore"), "pw")
                .unwrap();
        assert_eq!(account.account_id(), source.account_id());
        assert_eq!(account.created_at_ms(), source.account.created_at_ms());
        let devices = super::super::account_devices::AccountDevices::open(
            &super::super::account_devices::file_path(stage.path(), &account.account_id()),
            "pw",
            &account.account_id(),
        )
        .unwrap();
        assert_eq!(devices.devices().len(), 2);
        assert!(
            enrolled.persist_profile(stage.path(), "pw").is_err(),
            "must refuse overwriting existing staged material"
        );
        task.await.unwrap();
    }
    #[tokio::test]
    async fn anonymous_discovery_rejects_unbound_responses_and_creates_no_account() {
        let responder = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = responder.local_addr().unwrap();
        let identity = DeviceIdentity::generate();
        let announce =
            Announce::new_with_account(&identity, &Account::generate(), "Signed source", 43210);
        let expected = announce.public();
        let replay_identity = DeviceIdentity::generate();
        let replay_announce = Announce::new_with_account(
            &replay_identity,
            &Account::generate(),
            "Stale response",
            54321,
        );
        let stale = replay_announce.public();
        let task = tokio::spawn(async move {
            let mut bytes = [0; 2048];
            let (size, target) = responder.recv_from(&mut bytes).await.unwrap();
            assert_eq!(
                size, 64,
                "query contains no local account or public identity"
            );
            let nonce = query::decode_query(&bytes[..size]).unwrap();
            let replay =
                query::encode_response([99; 16], &replay_announce, &replay_identity).unwrap();
            responder.send_to(&replay, target).await.unwrap();
            let valid = query::encode_response(nonce, &announce, &identity).unwrap();
            responder.send_to(&valid, target).await.unwrap();
        });
        let enrollment = DeviceEnrollment::new(DeviceIdentity::generate());
        let sources = enrollment
            .probe_sources(&[address], Duration::from_millis(100))
            .await
            .unwrap();
        assert_eq!(
            sources
                .iter()
                .filter(|source| source.public_identity() == expected)
                .count(),
            1
        );
        assert!(
            !sources
                .iter()
                .any(|source| source.public_identity() == stale)
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn enrollment_rejects_signed_other_account_and_tampered_profile() {
        use super::super::pairing::{PairingAccessResponse, PairingResponse};
        for fault in 0..4 {
            let wrong_account = fault == 0;
            let directory = tempfile::tempdir().unwrap();
            let (d, _) = mpsc::unbounded_channel();
            let (c, _) = mpsc::unbounded_channel();
            let (f, _) = mpsc::unbounded_channel();
            let source = Node::open_with_account(
                DeviceIdentity::generate(),
                Account::generate(),
                Arc::new(Mutex::new(Roster::default())),
                d,
                c,
                f,
                &directory.path().join("events.log"),
                &directory.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            source.set_profile_display_name("Source").unwrap();
            let access = source
                .publish_account_access_if(b"private".to_vec(), |commit| commit())
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let announce = source.signed_announce("Source", addr.port());
            let selected = EnrollmentSource::from_announce(announce.clone(), addr.ip()).unwrap();
            let enrollment = DeviceEnrollment::new(DeviceIdentity::generate());
            let joiner = enrollment.public_identity();
            let serving = source.clone();
            let task = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept_with_presence(
                    socket,
                    &serving.identity,
                    Some(&announce),
                    |_| true,
                )
                .await
                .unwrap();
                channel.recv().await.unwrap();
                let other = Account::generate();
                let account = if wrong_account {
                    &other
                } else {
                    &serving.account
                };
                let mut profile = serving.account_profile(&serving.account_id()).unwrap();
                if !wrong_account {
                    profile.display_name = "Unsigned edit".into();
                }
                let response = PairingAccessResponse {
                    account: PairingResponse {
                        account_secret: account.secret_bytes(),
                        account_ed25519_pub: account.public().ed25519_pub,
                        cert: account.certify(&joiner.ed25519_pub),
                    },
                    created_at_ms: account.created_at_ms(),
                    access,
                    profile: Some(profile),
                };
                match fault {
                    2 => {
                        let mut chunk = b"MTPC4".to_vec();
                        chunk.extend((1024u32 * 1024).to_be_bytes());
                        chunk.extend([0, 3]);
                        channel.send(&chunk).await.unwrap();
                    }
                    3 => {
                        let mut chunk = b"MTPC4".to_vec();
                        chunk.extend((70_000u32).to_be_bytes());
                        chunk.extend([0, 2]);
                        chunk.resize(11 + 60 * 1024, 0);
                        channel.send(&chunk).await.unwrap();
                        chunk.truncate(11);
                        chunk[9] = 0;
                        chunk.resize(11 + 70_000 - 60 * 1024, 0);
                        channel.send(&chunk).await.unwrap();
                    }
                    _ => response.send(&mut channel).await.unwrap(),
                }
            });
            assert!(
                enrollment
                    .enroll(&selected, "000102030405060708090a0b0c0d0e0f", false)
                    .await
                    .is_err()
            );
            task.await.unwrap();
        }
    }
}

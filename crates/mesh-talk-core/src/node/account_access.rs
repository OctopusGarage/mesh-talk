//! Private account credentials: opaque to core, signed and encrypted at rest.
use super::{Node, NodeError};
use crate::{
    eventlog::LogError,
    identity::{
        account::{Account, AccountPublic},
        device::DeviceIdentity,
    },
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::Path};
const MAX_PAYLOAD: usize = 8 * 1024;
const DOMAIN: &[u8] = b"mesh-talk-account-access-v1";
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAccessSnapshot {
    version: u8,
    account_pub: [u8; 32],
    revision: u64,
    payload: Vec<u8>,
    signature: Vec<u8>,
}
impl Drop for AccountAccessSnapshot {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.payload);
    }
}
impl std::fmt::Debug for AccountAccessSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountAccessSnapshot")
            .field("account_id", &self.account_id())
            .field("revision", &self.revision)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}
impl AccountAccessSnapshot {
    pub fn account_id(&self) -> String {
        AccountPublic {
            ed25519_pub: self.account_pub,
        }
        .account_id()
    }
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    fn message(&self) -> zeroize::Zeroizing<Vec<u8>> {
        let mut bytes = zeroize::Zeroizing::new(DOMAIN.to_vec());
        bytes.push(self.version);
        bytes.extend(self.account_pub);
        bytes.extend(self.revision.to_be_bytes());
        bytes.extend((self.payload.len() as u64).to_be_bytes());
        bytes.extend(&self.payload);
        bytes
    }
    fn new(account: &Account, revision: u64, payload: Vec<u8>) -> Result<Self, NodeError> {
        if payload.is_empty() || payload.len() > MAX_PAYLOAD || revision == 0 {
            return Err(NodeError::InvalidInput(
                "invalid account access record".into(),
            ));
        }
        let mut snapshot = Self {
            version: 1,
            account_pub: account.public().ed25519_pub,
            revision,
            payload,
            signature: Vec::new(),
        };
        snapshot.signature = account.sign(&snapshot.message()).to_vec();
        Ok(snapshot)
    }
    pub(crate) fn valid_for(&self, account: &[u8; 32]) -> bool {
        self.version == 1
            && self.account_pub == *account
            && self.revision > 0
            && !self.payload.is_empty()
            && self.payload.len() <= MAX_PAYLOAD
            && self
                .signature
                .as_slice()
                .try_into()
                .is_ok_and(|signature| DeviceIdentity::verify(account, &self.message(), signature))
    }
}
/// Public, secret-free observation of one certified sibling's essential sync.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccessSyncPhase {
    Synchronizing,
    UpToDate,
    WaitingForSource,
    RetryScheduled,
    Unsupported,
    PermissionDenied,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountAccessSyncFailure {
    Connection,
    ResponseClosed,
    UnsupportedProtocol,
    InvalidRecord,
    PermissionDenied,
    Storage,
    Timeout,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountAccessSyncStatus {
    pub device_id: String,
    pub phase: AccountAccessSyncPhase,
    pub revision: u64,
    pub failure: Option<AccountAccessSyncFailure>,
    pub attempts: u32,
    pub retry_delay_ms: u64,
}

pub(super) struct AccountAccessStore {
    file: EncryptedRecordLog<AccountAccessSnapshot>,
    current: HashMap<String, AccountAccessSnapshot>,
    sync_status: HashMap<String, AccountAccessSyncStatus>,
}
impl AccountAccessStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) =
            EncryptedRecordLog::<AccountAccessSnapshot>::open(path, password, b"MTACCS")?;
        let mut current: HashMap<String, AccountAccessSnapshot> = HashMap::new();
        for record in records {
            if !record.valid_for(&record.account_pub) {
                return Err(LogError::CorruptFile(
                    "invalid account access record".into(),
                ));
            }
            let id = record.account_id();
            if current
                .get(&id)
                .is_none_or(|old| old.revision < record.revision)
            {
                current.insert(id, record);
            }
        }
        Ok(Self {
            file,
            current,
            sync_status: HashMap::new(),
        })
    }
    pub(super) fn current(&self, account: &str) -> Option<AccountAccessSnapshot> {
        self.current.get(account).cloned()
    }
    pub(super) fn merge(&mut self, record: AccountAccessSnapshot) -> Result<bool, LogError> {
        if !record.valid_for(&record.account_pub) {
            return Err(LogError::CorruptFile(
                "invalid account access record".into(),
            ));
        }
        let id = record.account_id();
        if self
            .current
            .get(&id)
            .is_some_and(|old| old.revision >= record.revision)
        {
            return Ok(false);
        }
        self.file.append_durable(&record)?;
        self.current.insert(id, record);
        Ok(true)
    }
}
impl Node {
    pub fn account_access_snapshot(&self) -> Option<AccountAccessSnapshot> {
        self.account_access
            .lock()
            .expect("account access mutex")
            .current(&self.account_id())
    }
    pub fn account_access_sync_status(&self) -> Vec<AccountAccessSyncStatus> {
        let devices = self.linked_devices();
        let mut statuses: Vec<_> = self
            .account_access
            .lock()
            .expect("account access mutex")
            .sync_status
            .values()
            .filter(|status| {
                devices
                    .iter()
                    .any(|device| device.public.user_id() == status.device_id)
            })
            .cloned()
            .collect();
        statuses.sort_by(|left, right| left.device_id.cmp(&right.device_id));
        statuses
    }
    fn set_account_access_sync_status(&self, status: AccountAccessSyncStatus) {
        let devices = self.linked_devices();
        if !devices
            .iter()
            .any(|device| device.public.user_id() == status.device_id)
        {
            return;
        }
        let mut store = self.account_access.lock().expect("account access mutex");
        store
            .sync_status
            .retain(|id, _| devices.iter().any(|device| device.public.user_id() == *id));
        store.sync_status.insert(status.device_id.clone(), status);
    }
    pub fn subscribe_account_access(
        &self,
    ) -> tokio::sync::watch::Receiver<Option<AccountAccessSnapshot>> {
        self.account_access_updates.subscribe()
    }
    pub fn publish_account_access_if(
        &self,
        payload: Vec<u8>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountAccessSnapshot, NodeError> {
        if payload.is_empty() || payload.len() > MAX_PAYLOAD {
            return Err(NodeError::InvalidInput(
                "invalid account access record".into(),
            ));
        }
        let payload = zeroize::Zeroizing::new(payload);
        let mut published = None;
        authorize(&mut || {
            let mut store = self.account_access.lock().expect("account access mutex");
            let next = store
                .current(&self.account_id())
                .map_or(0, |r| r.revision)
                .checked_add(1)
                .ok_or_else(|| {
                    NodeError::InvalidInput("account access revision exhausted".into())
                })?;
            let snapshot = AccountAccessSnapshot::new(
                &self.account,
                super::node::now_millis().max(next),
                payload.to_vec(),
            )?;
            store.merge(snapshot.clone()).map_err(NodeError::Log)?;
            self.account_access_updates
                .send_replace(Some(snapshot.clone()));
            published = Some(snapshot);
            Ok(())
        })?;
        published.ok_or_else(|| {
            NodeError::Authorization("account access publication was not authorized".into())
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authenticated_record_rejects_tamper_oversize_and_stale_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let path = dir.path().join("access.log");
        let mut store = AccountAccessStore::open(&path, "pw").unwrap();
        let first =
            AccountAccessSnapshot::new(&account, 1, b"private credential".to_vec()).unwrap();
        assert!(store.merge(first.clone()).unwrap());
        assert!(!store.merge(first.clone()).unwrap());
        let mut forged = first.clone();
        forged.payload[0] ^= 1;
        assert!(store.merge(forged).is_err());
        assert!(AccountAccessSnapshot::new(&account, 2, vec![0; MAX_PAYLOAD + 1]).is_err());
        assert!(!format!("{first:?}").contains("private credential"));
        drop(store);
        assert!(
            !std::fs::read(&path)
                .unwrap()
                .windows(18)
                .any(|v| v == b"private credential")
        );
        let store = AccountAccessStore::open(&path, "pw").unwrap();
        assert_eq!(store.current(&account.account_id()), Some(first));
        assert!(store.current(&Account::generate().account_id()).is_none());
    }
    #[tokio::test]
    async fn v4_invalid_private_records_never_persist_or_adopt() {
        use super::super::pairing::{PairingAccessResponse, PairingRequest, PairingResponse};
        use crate::transport::SecureChannel;
        use tokio::net::TcpListener;
        for invalid in 0..5 {
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let source = open_node(&dir.path().join("source"), &account);
            let target = open_node(&dir.path().join("target"), &Account::generate());
            let own = target.account_id();
            let mut snapshot =
                AccountAccessSnapshot::new(&account, 1, b"credentials".to_vec()).unwrap();
            match invalid {
                0 => snapshot.payload[0] ^= 1,
                1 => snapshot.payload = vec![0; MAX_PAYLOAD + 1],
                2 => {
                    snapshot =
                        AccountAccessSnapshot::new(&Account::generate(), 1, b"foreign".to_vec())
                            .unwrap()
                }
                _ => {}
            }
            let mut secret = account.secret_bytes();
            if invalid == 3 {
                secret[0] ^= 1;
            }
            let response = PairingAccessResponse {
                profile: None,
                account: PairingResponse {
                    account_secret: secret,
                    account_ed25519_pub: account.public().ed25519_pub,
                    cert: account.certify(&target.identity.public().ed25519_pub),
                },
                access: snapshot,
                created_at_ms: account.created_at_ms(),
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let public = source.identity.public();
            if invalid == 4 {
                let announce = crate::discovery::Announce::new_with_account(
                    &source.identity,
                    &Account::generate(),
                    "selected source",
                    address.port(),
                );
                target
                    .roster
                    .lock()
                    .unwrap()
                    .update(&announce, address.ip(), &target.user_id());
            }
            let task = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(socket, &source.identity)
                    .await
                    .unwrap();
                assert!(PairingRequest::decode_access(&channel.recv().await.unwrap()).is_some());
                channel.send(&response.encode()).await.unwrap();
                let _ = channel.send(&[]).await;
            });
            assert!(
                target
                    .link_to_device_with_account_access_checked_if(
                        address,
                        &public,
                        &super::super::PairingCode::generate().as_hex(),
                        |_| Ok(()),
                        |commit| commit()
                    )
                    .await
                    .is_err()
            );
            assert_eq!(target.account_id(), own);
            assert!(
                target
                    .account_access
                    .lock()
                    .unwrap()
                    .current(&account.account_id())
                    .is_none()
            );
            task.await.unwrap();
        }
    }
    fn open_node(dir: &Path, account: &Account) -> std::sync::Arc<Node> {
        open_node_with_identity(dir, account, DeviceIdentity::generate())
    }
    fn open_node_with_identity(
        dir: &Path,
        account: &Account,
        identity: DeviceIdentity,
    ) -> std::sync::Arc<Node> {
        let (d, _) = tokio::sync::mpsc::unbounded_channel();
        let (c, _) = tokio::sync::mpsc::unbounded_channel();
        let (f, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            Account::from_secret_bytes(account.secret_bytes()),
            std::sync::Arc::new(std::sync::Mutex::new(crate::discovery::Roster::default())),
            d,
            c,
            f,
            &dir.join("events.log"),
            &dir.join("sent.log"),
            "pw",
        )
        .unwrap()
    }
    fn certify_pair(left: &Node, right: &Node, account: &Account) {
        for (node, other) in [(left, right), (right, left)] {
            node.account_devices
                .lock()
                .unwrap()
                .insert(super::super::account_devices::AccountDevice {
                    public: other.identity.public(),
                    certificate: account.certify(&other.identity.public().ed25519_pub),
                })
                .unwrap();
        }
    }
    #[tokio::test]
    async fn essential_exchange_keeps_valid_twelve_second_control_response() {
        use crate::transport::SecureChannel;
        use tokio::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = open_node(&dir.path().join("source"), &account);
        let target = open_node(&dir.path().join("target"), &account);
        certify_pair(&source, &target, &account);
        let expected = source
            .publish_account_access_if(b"source credentials".to_vec(), |commit| commit())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let announce = source.signed_announce("source", address.port());
        target
            .roster
            .lock()
            .unwrap()
            .update(&announce, address.ip(), &target.user_id());
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let serving = source.clone();
        let offered = expected.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut channel = SecureChannel::accept(socket, &serving.identity)
                .await
                .unwrap();
            assert!(AccessRequest::decode(&channel.recv().await.unwrap()).is_some());
            let delay = tokio::time::sleep(std::time::Duration::from_secs(12));
            tokio::pin!(delay);
            ready.send(()).unwrap();
            delay.await;
            let _ = channel
                .send(&super::super::wire::frame(
                    b"MTAS1",
                    &AccessResponse {
                        snapshot: Some(offered),
                    },
                ))
                .await;
        });
        let mut updates = target.subscribe_account_access();
        let worker = tokio::spawn(target.clone().run_account_access_loop());
        tokio::time::timeout(std::time::Duration::from_secs(3), waiting)
            .await
            .unwrap()
            .unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(12)).await;
        let applied =
            tokio::time::timeout(std::time::Duration::from_secs(1), updates.changed()).await;
        worker.abort();
        server.await.unwrap();
        assert!(
            applied.is_ok(),
            "a legitimate 12-second authenticated response must fit the shared adaptive control budget"
        );
        assert_eq!(target.account_access_snapshot(), Some(expected));
    }
    #[tokio::test]
    async fn essential_exchange_retries_transient_close_without_five_minute_stall() {
        use crate::transport::SecureChannel;
        use tokio::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = open_node(&dir.path().join("source"), &account);
        let target = open_node(&dir.path().join("target"), &account);
        certify_pair(&source, &target, &account);
        let expected = source
            .publish_account_access_if(b"source credentials".to_vec(), |commit| commit())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let announce = source.signed_announce("source", address.port());
        target
            .roster
            .lock()
            .unwrap()
            .update(&announce, address.ip(), &target.user_id());
        let serving = source.clone();
        let server = tokio::spawn(async move {
            for first in [true, false] {
                let (socket, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(socket, &serving.identity)
                    .await
                    .unwrap();
                let bytes = channel.recv().await.unwrap();
                let request = AccessRequest::decode(&bytes).unwrap();
                if !first {
                    serving.serve_account_access(&mut channel, request).await;
                }
            }
        });
        let mut updates = target.subscribe_account_access();
        let worker = tokio::spawn(target.clone().run_account_access_loop());
        let applied =
            tokio::time::timeout(std::time::Duration::from_secs(8), updates.changed()).await;
        worker.abort();
        server.abort();
        assert!(
            applied.is_ok(),
            "a transient authenticated close must retry promptly rather than wait five minutes"
        );
        assert_eq!(target.account_access_snapshot(), Some(expected));
    }
    #[tokio::test]
    async fn fresh_verified_route_wakes_unsupported_sibling_before_cooldown() {
        use crate::transport::SecureChannel;
        use tokio::net::TcpListener;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = open_node(&dir.path().join("source"), &account);
        let target = open_node(&dir.path().join("target"), &account);
        certify_pair(&source, &target, &account);
        let expected = source
            .publish_account_access_if(b"private source credentials".to_vec(), |commit| commit())
            .unwrap();
        let old = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let old_address = old.local_addr().unwrap();
        target.roster.lock().unwrap().update(
            &source.signed_announce("source", old_address.port()),
            old_address.ip(),
            &target.user_id(),
        );
        let serving = source.clone();
        let old_server = tokio::spawn(async move {
            let (socket, _) = old.accept().await.unwrap();
            let mut channel = SecureChannel::accept(socket, &serving.identity)
                .await
                .unwrap();
            channel.recv().await.unwrap();
            channel.send(b"old-unsupported-protocol").await.unwrap();
        });
        let mut updates = target.subscribe_account_access();
        let worker = tokio::spawn(target.clone().run_account_access_loop());
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                if target.account_access_sync_status().iter().any(|status| {
                    status.phase == AccountAccessSyncPhase::Unsupported
                        && status.retry_delay_ms == 60_000
                }) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("unsupported status must be observable");
        old_server.await.unwrap();
        let fresh = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = fresh.local_addr().unwrap();
        let serving = source.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = fresh.accept().await.unwrap();
            let mut channel = SecureChannel::accept(socket, &serving.identity)
                .await
                .unwrap();
            let request = AccessRequest::decode(&channel.recv().await.unwrap()).unwrap();
            serving.serve_account_access(&mut channel, request).await;
        });
        target.roster.lock().unwrap().update(
            &source.signed_announce("source", address.port()),
            address.ip(),
            &target.user_id(),
        );
        let applied =
            tokio::time::timeout(std::time::Duration::from_secs(3), updates.changed()).await;
        worker.abort();
        server.abort();
        assert!(
            applied.is_ok(),
            "fresh authenticated route must not inherit an old protocol cooldown"
        );
        assert_eq!(target.account_access_snapshot(), Some(expected));
    }
    #[tokio::test]
    async fn direct_siblings_converge_with_optional_sync_disabled_and_unrelated_cannot_fetch() {
        use crate::transport::SecureChannel;
        use tokio::net::{TcpListener, TcpStream};
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = open_node(&dir.path().join("source"), &account);
        let target = open_node(&dir.path().join("target"), &account);
        let stranger = open_node(&dir.path().join("stranger"), &Account::generate());
        for (node, other) in [(&source, &target), (&target, &source)] {
            node.account_devices
                .lock()
                .unwrap()
                .insert(super::super::account_devices::AccountDevice {
                    public: other.identity.public(),
                    certificate: account.certify(&other.identity.public().ed25519_pub),
                })
                .unwrap();
            for category in [
                super::super::SyncCategory::History,
                super::super::SyncCategory::Appearance,
                super::super::SyncCategory::Contacts,
            ] {
                node.set_device_sync_category(category, false).unwrap();
            }
        }
        let expected = source
            .publish_account_access_if(b"private shared access".to_vec(), |commit| commit())
            .unwrap();
        for (client, allowed) in [(&target, true), (&stranger, false)] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let serving = source.clone();
            let task = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(socket, &serving.identity)
                    .await
                    .unwrap();
                let bytes = channel.recv().await.unwrap();
                let request = AccessRequest::decode(&bytes).unwrap();
                serving.serve_account_access(&mut channel, request).await;
            });
            if allowed {
                let peer = crate::discovery::PeerRecord {
                    public: source.identity.public(),
                    addr: address,
                    routes: Vec::new(),
                    name: "source".into(),
                    post_office: false,
                    account_id: Some(account.account_id()),
                    last_seen: std::time::Instant::now(),
                };
                client.pull_account_access(&peer).await.unwrap();
                assert_eq!(client.account_access_snapshot(), Some(expected.clone()));
            } else {
                let mut channel = SecureChannel::connect(
                    TcpStream::connect(address).await.unwrap(),
                    &client.identity,
                    Some(&source.identity.public()),
                )
                .await
                .unwrap();
                channel
                    .send(&super::super::wire::frame(
                        b"MTAR1",
                        &AccessRequest {
                            account_id: account.account_id(),
                            known_revision: 0,
                        },
                    ))
                    .await
                    .unwrap();
                assert!(
                    tokio::time::timeout(std::time::Duration::from_secs(2), channel.recv())
                        .await
                        .unwrap()
                        .is_err()
                );
            }
            task.await.unwrap();
        }
        let (ed, dh) = target.identity.secret_bytes();
        drop(target);
        let reopened = open_node_with_identity(
            &dir.path().join("target"),
            &account,
            DeviceIdentity::from_secret_bytes(ed, dh),
        );
        assert_eq!(reopened.account_access_snapshot(), Some(expected));
        reopened.runtime_work.close();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reopened.run_account_access_loop(),
        )
        .await
        .unwrap();
    }
}

#[derive(Serialize, Deserialize)]
pub(super) struct AccessRequest {
    account_id: String,
    known_revision: u64,
}
impl AccessRequest {
    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        super::wire::unframe(b"MTAR1", bytes)
    }
}
#[derive(Serialize, Deserialize)]
struct AccessResponse {
    snapshot: Option<AccountAccessSnapshot>,
}
impl Node {
    pub(super) async fn serve_account_access(
        &self,
        channel: &mut crate::transport::SecureChannel<tokio::net::TcpStream>,
        request: AccessRequest,
    ) {
        let response = {
            let _operation = self.privacy.gate.read().await;
            if !channel.io_admitted()
                || request.account_id != self.account_id()
                || !self.certified_sibling(channel.peer_identity())
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
            {
                return;
            }
            let snapshot = self
                .account_access_snapshot()
                .filter(|s| s.revision() > request.known_revision);
            zeroize::Zeroizing::new(super::wire::frame(b"MTAS1", &AccessResponse { snapshot }))
        };
        let peer = channel.peer_identity().clone();
        let started = std::time::Instant::now();
        if matches!(
            self.adaptive_control_timeout(&peer, channel.send(&response))
                .await,
            Some(Ok(()))
        ) {
            self.observe_control_exchange(channel, started);
        }
    }
    async fn pull_account_access(
        &self,
        peer: &crate::discovery::PeerRecord,
    ) -> Result<Option<u64>, AccountAccessSyncFailure> {
        use AccountAccessSyncFailure as Failure;
        if peer.account_id.as_deref() != Some(self.account_id().as_str())
            || !self.certified_sibling(&peer.public)
        {
            return Err(Failure::PermissionDenied);
        }
        let mut channel = self
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(|error| {
                if matches!(error, crate::transport::TransportError::AdmissionDenied) {
                    Failure::PermissionDenied
                } else {
                    Failure::Connection
                }
            })?;
        let started = std::time::Instant::now();
        let request = AccessRequest {
            account_id: self.account_id(),
            known_revision: self.account_access_snapshot().map_or(0, |s| s.revision()),
        };
        channel
            .send(&super::wire::frame(b"MTAR1", &request))
            .await
            .map_err(|_| Failure::Connection)?;
        let bytes =
            zeroize::Zeroizing::new(channel.recv().await.map_err(|_| Failure::ResponseClosed)?);
        let response: AccessResponse =
            super::wire::unframe(b"MTAS1", &bytes).ok_or(Failure::UnsupportedProtocol)?;
        {
            let _operation = self.privacy.gate.read().await;
            if !channel.io_admitted()
                || !self.certified_sibling(channel.peer_identity())
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
            {
                return Err(Failure::PermissionDenied);
            }
            if let Some(snapshot) = response.snapshot {
                if !snapshot.valid_for(&self.account.public().ed25519_pub) {
                    return Err(Failure::InvalidRecord);
                }
                if self
                    .account_access
                    .lock()
                    .expect("account access mutex")
                    .merge(snapshot.clone())
                    .map_err(|_| Failure::Storage)?
                {
                    self.account_access_updates.send_replace(Some(snapshot));
                }
            }
        }
        self.observe_control_exchange(&channel, started);
        Ok(self
            .account_access_snapshot()
            .map(|snapshot| snapshot.revision()))
    }
    /// The sole runtime worker uses the shared transport/control budgets. Route
    /// changes wake it; transient failures never enter protocol-upgrade cooldown.
    pub(super) async fn run_account_access_loop(self: std::sync::Arc<Self>) {
        use AccountAccessSyncFailure as Failure;
        use AccountAccessSyncPhase as Phase;
        let mut attempts: HashMap<String, AccessRetry> = HashMap::new();
        let mut observations = self
            .roster
            .lock()
            .expect("roster mutex")
            .subscribe_changes();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {_=tick.tick()=>{},result=observations.changed()=>{if result.is_err(){return;}}}
            if self.runtime_work.is_closed() {
                return;
            }
            let peers = self.roster.lock().expect("roster mutex").peers();
            attempts.retain(|_, state| {
                peers.iter().any(|peer| peer.public == state.peer.public)
                    && self.certified_sibling(&state.peer.public)
            });
            for peer in peers {
                if self.runtime_work.is_closed() {
                    return;
                }
                if peer.account_id.as_deref() != Some(self.account_id().as_str())
                    || !self.certified_sibling(&peer.public)
                {
                    continue;
                }
                let id = peer.public.user_id();
                let binding = AccessPeerBinding::new(
                    &peer,
                    self.privacy
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst),
                );
                let state = attempts
                    .entry(id.clone())
                    .or_insert_with(|| AccessRetry::new(binding.clone()));
                if state.peer != binding {
                    *state = AccessRetry::new(binding);
                }
                if state.next > tokio::time::Instant::now() {
                    continue;
                }
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let revision = self.account_access_snapshot().map_or(0, |s| s.revision());
                self.set_account_access_sync_status(AccountAccessSyncStatus {
                    device_id: id.clone(),
                    phase: Phase::Synchronizing,
                    revision,
                    failure: state.failure,
                    attempts: state.failures,
                    retry_delay_ms: 0,
                });
                let result = self
                    .adaptive_control_timeout(
                        &peer.public,
                        permit.track(self.pull_account_access(&peer)),
                    )
                    .await
                    .unwrap_or(Err(Failure::Timeout));
                let (phase, revision, delay) = match result {
                    Ok(revision) => {
                        state.failures = 0;
                        state.failure = None;
                        (
                            if revision.is_some() {
                                Phase::UpToDate
                            } else {
                                Phase::WaitingForSource
                            },
                            revision.unwrap_or(0),
                            30,
                        )
                    }
                    Err(reason) => {
                        if state.failure != Some(reason) {
                            log::warn!("Private account synchronization deferred: {reason:?}");
                        }
                        state.failures = state.failures.saturating_add(1);
                        state.failure = Some(reason);
                        let delay = match reason {
                            Failure::UnsupportedProtocol | Failure::InvalidRecord => 60,
                            Failure::PermissionDenied => 30,
                            _ => match state.failures {
                                1 => 2,
                                2 => 5,
                                3 => 15,
                                _ => 30,
                            },
                        };
                        (
                            match reason {
                                Failure::UnsupportedProtocol => Phase::Unsupported,
                                Failure::PermissionDenied => Phase::PermissionDenied,
                                _ => Phase::RetryScheduled,
                            },
                            revision,
                            delay,
                        )
                    }
                };
                state.next = tokio::time::Instant::now() + std::time::Duration::from_secs(delay);
                self.set_account_access_sync_status(AccountAccessSyncStatus {
                    device_id: id,
                    phase,
                    revision,
                    failure: state.failure,
                    attempts: state.failures,
                    retry_delay_ms: delay * 1000,
                });
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct AccessPeerBinding {
    public: crate::identity::device::PublicIdentity,
    account: Option<String>,
    address: std::net::SocketAddr,
    routes: Vec<(std::net::SocketAddr, u8)>,
    generation: u64,
}
impl AccessPeerBinding {
    fn new(peer: &crate::discovery::PeerRecord, generation: u64) -> Self {
        use crate::discovery::roster::RouteSource;
        let mut routes: Vec<_> = peer
            .routes
            .iter()
            .map(|route| {
                (
                    route.addr,
                    match route.source {
                        RouteSource::Lan => 0,
                        RouteSource::Discovery => 1,
                        RouteSource::RoutedQuery => 2,
                        RouteSource::Gateway => 3,
                        RouteSource::Authenticated => 4,
                        RouteSource::PostOffice => 5,
                    },
                )
            })
            .collect();
        routes.sort();
        routes.dedup();
        Self {
            public: peer.public.clone(),
            account: peer.account_id.clone(),
            address: peer.addr,
            routes,
            generation,
        }
    }
}
struct AccessRetry {
    peer: AccessPeerBinding,
    next: tokio::time::Instant,
    failures: u32,
    failure: Option<AccountAccessSyncFailure>,
}
impl AccessRetry {
    fn new(peer: AccessPeerBinding) -> Self {
        Self {
            peer,
            next: tokio::time::Instant::now(),
            failures: 0,
            failure: None,
        }
    }
}

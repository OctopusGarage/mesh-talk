//! Optional MTOW1 bounded invalidation hints on a dedicated authenticated channel.
use super::*;
use crate::postoffice::{IndexedPostOffice, MailboxWatchToken};
use sha2::{Digest, Sha256};
use std::time::Duration;
const MAGIC: &[u8; 5] = b"MTOW1";
fn probe() -> ConversationId {
    ConversationId::new([0; 32])
}
#[derive(Debug, Serialize, Deserialize)]
enum Frame {
    Supported,
    Busy,
    Observe(Vec<ConversationId>),
    Wait(Vec<MailboxWatchToken>),
    Snapshot(Vec<MailboxWatchToken>),
}
fn marker() -> [u8; 32] {
    Sha256::digest(b"mesh-talk-opaque-mailbox-watch-v1-probe\0").into()
}
fn enc(frame: &Frame) -> Result<Vec<u8>, SessionError> {
    let body = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(16384)
        .serialize(frame)
        .map_err(|e| SessionError::Serialization(e.to_string()))?;
    let mut bytes = MAGIC.to_vec();
    bytes.extend(body);
    Ok(bytes)
}
fn dec(bytes: &[u8]) -> Result<Frame, SessionError> {
    if !bytes.starts_with(MAGIC) || bytes.len() > 16384 {
        return Err(SessionError::UnexpectedMessage);
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(16384)
        .reject_trailing_bytes()
        .deserialize(&bytes[5..])
        .map_err(|e| SessionError::Serialization(e.to_string()))
}
pub(crate) fn is_probe(bytes: &[u8]) -> bool {
    matches!(decode(bytes), Ok(SyncWire::FpRequest(r)) if r.conversation == probe() && r.fingerprint == marker())
}
pub(crate) async fn negotiate<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
) -> Result<bool, SessionError> {
    channel
        .send(&encode(&SyncWire::FpRequest(FpRequest {
            conversation: probe(),
            fingerprint: marker(),
        }))?)
        .await?;
    let bytes = channel.recv().await?;
    if bytes.starts_with(MAGIC) {
        return match dec(&bytes)? {
            Frame::Supported => Ok(true),
            Frame::Busy => Err(SessionError::ResourceUnavailable),
            _ => Err(SessionError::UnexpectedMessage),
        };
    }
    match decode(&bytes)? {
        SyncWire::FpResponse(FpResponse { matched: false }) => Ok(false),
        _ => Err(SessionError::UnexpectedMessage),
    }
}
fn validate(scopes: &[ConversationId]) -> Result<(), SessionError> {
    if scopes.is_empty()
        || scopes.len() > 64
        || scopes
            .iter()
            .enumerate()
            .any(|(i, c)| scopes[..i].contains(c))
    {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(())
}
async fn request<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    frame: Frame,
    scopes: &[ConversationId],
) -> Result<Vec<MailboxWatchToken>, SessionError> {
    validate(scopes)?;
    channel.send(&enc(&frame)?).await?;
    match dec(&channel.recv().await?)? {
        Frame::Snapshot(tokens)
            if tokens.len() == scopes.len()
                && tokens
                    .iter()
                    .zip(scopes)
                    .all(|(t, c)| t.conversation == *c && t.epoch >= 0 && t.revision >= 0) =>
        {
            Ok(tokens)
        }
        Frame::Busy => Err(SessionError::ResourceUnavailable),
        _ => Err(SessionError::UnexpectedMessage),
    }
}
pub(crate) async fn observe<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    conversations: &[ConversationId],
) -> Result<Vec<MailboxWatchToken>, SessionError> {
    request(
        channel,
        Frame::Observe(conversations.to_vec()),
        conversations,
    )
    .await
}
pub(crate) async fn wait<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    prior: &[MailboxWatchToken],
) -> Result<Vec<MailboxWatchToken>, SessionError> {
    let scopes: Vec<_> = prior.iter().map(|t| t.conversation).collect();
    request(channel, Frame::Wait(prior.to_vec()), &scopes).await
}
async fn send_server_frame<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    frame: &Frame,
) -> Result<(), SessionError> {
    tokio::time::timeout(Duration::from_secs(30), channel.send(&enc(frame)?))
        .await
        .map_err(|_| SessionError::ResourceUnavailable)??;
    Ok(())
}

pub(crate) async fn serve<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &IndexedPostOffice,
    permits: &std::sync::Arc<tokio::sync::Semaphore>,
) -> Result<(), SessionError> {
    serve_with_wait_timeout(channel, store, permits, Duration::from_secs(300)).await
}

async fn serve_with_wait_timeout<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &IndexedPostOffice,
    permits: &std::sync::Arc<tokio::sync::Semaphore>,
    wait_timeout: Duration,
) -> Result<(), SessionError> {
    let Ok(_permit) = permits.clone().try_acquire_owned() else {
        send_server_frame(channel, &Frame::Busy).await?;
        return Ok(());
    };
    send_server_frame(channel, &Frame::Supported).await?;
    loop {
        let bytes = tokio::time::timeout(Duration::from_secs(30), channel.recv())
            .await
            .map_err(|_| SessionError::UnexpectedMessage)??;
        let (scopes, prior) = match dec(&bytes)? {
            Frame::Observe(scopes) => (scopes, None),
            Frame::Wait(tokens) => (
                tokens.iter().map(|t| t.conversation).collect(),
                Some(tokens),
            ),
            _ => return Err(SessionError::UnexpectedMessage),
        };
        validate(&scopes)?;
        // Subscribe before the first snapshot; changed() consumes only committed
        // publications. No watch Ref or storage guard crosses an await.
        let mut changes = store.subscribe_changes();
        let deadline = tokio::time::Instant::now() + wait_timeout;
        let tokens = loop {
            let _generation = *changes.borrow_and_update();
            let current = store.mailbox_watch_snapshot(&scopes)?;
            if prior.as_ref().is_none_or(|p| *p != current)
                || tokio::time::Instant::now() >= deadline
            {
                break current;
            }
            tokio::select! {
                result=channel.watch_disconnect()=> { result?; return Ok(()); }
                result=changes.changed()=> { if result.is_err() { return Err(SessionError::ResourceUnavailable); } }
                _=tokio::time::sleep_until(deadline)=>break store.mailbox_watch_snapshot(&scopes)?,
            }
        };
        send_server_frame(channel, &Frame::Snapshot(tokens)).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    use crate::storage::profile::ProfileStorageLease;
    fn office(root: &tempfile::TempDir) -> IndexedPostOffice {
        IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap()
    }

    async fn endpoint_channel(
        endpoint: crate::postoffice::federation::IndexedRelayEndpoint,
        tasks: &mut tokio::task::JoinSet<()>,
    ) -> SecureChannel<tokio::io::DuplexStream> {
        let (client, server) = tokio::io::duplex(65536);
        tasks.spawn(async move {
            let channel = SecureChannel::accept(server, &DeviceIdentity::generate())
                .await
                .unwrap();
            let _closed = endpoint.serve_connection_checked(channel, None).await;
        });
        SecureChannel::connect(client, &DeviceIdentity::generate(), None)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn watch_protocol_timeout_returns_equal_snapshot_and_abort_releases_permit() {
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let owned_permits = permits.clone();
        let (client, server) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(server, &DeviceIdentity::generate())
                .await
                .unwrap();
            assert!(is_probe(&channel.recv().await.unwrap()));
            let _closed = serve_with_wait_timeout(
                &mut channel,
                &office,
                &owned_permits,
                Duration::from_millis(30),
            )
            .await;
        });
        let mut channel = SecureChannel::connect(client, &DeviceIdentity::generate(), None)
            .await
            .unwrap();
        assert!(negotiate(&mut channel).await.unwrap());
        let prior = observe(&mut channel, &[probe()]).await.unwrap();
        let after = tokio::time::timeout(Duration::from_secs(2), wait(&mut channel, &prior))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prior, after);
        channel
            .send(&enc(&Frame::Wait(prior)).unwrap())
            .await
            .unwrap();
        assert_eq!(permits.available_permits(), 0);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(permits.available_permits(), 1);
        assert!(channel.recv().await.is_err());
    }

    #[tokio::test]
    async fn watch_protocol_rejects_malformed_oversize_and_duplicate_scopes() {
        let root = tempfile::tempdir().unwrap();
        let endpoint =
            crate::postoffice::federation::IndexedRelayEndpoint::new(office(&root), None, None);
        let mut tasks = tokio::task::JoinSet::new();
        let scopes = (0..65).map(|n| ConversationId::new([n; 32])).collect();
        for bytes in [
            b"MTOW1bad".to_vec(),
            vec![7; 16385],
            enc(&Frame::Observe(vec![probe(), probe()])).unwrap(),
            enc(&Frame::Observe(scopes)).unwrap(),
        ] {
            let mut channel = endpoint_channel(endpoint.clone(), &mut tasks).await;
            assert!(negotiate(&mut channel).await.unwrap());
            channel.send(&bytes).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_secs(2), channel.recv())
                    .await
                    .unwrap()
                    .is_err()
            );
        }
        while let Some(joined) = tasks.join_next().await {
            joined.unwrap();
        }
    }

    #[tokio::test]
    async fn watch_protocol_commit_before_wait_returns_without_notification() {
        let root = tempfile::tempdir().unwrap();
        let office = office(&root);
        let endpoint =
            crate::postoffice::federation::IndexedRelayEndpoint::new(office.clone(), None, None);
        let mut tasks = tokio::task::JoinSet::new();
        let mut channel = endpoint_channel(endpoint, &mut tasks).await;
        let author = DeviceIdentity::generate();
        let mailbox = crate::postoffice::opaque::RelayMailbox::federated(
            &author,
            &DeviceIdentity::generate().public(),
        )
        .unwrap();
        assert!(negotiate(&mut channel).await.unwrap());
        let prior = observe(&mut channel, &[mailbox.id()]).await.unwrap();
        let event = crate::eventlog::Event::new(
            &author,
            probe(),
            1,
            vec![],
            1,
            1,
            crate::eventlog::EventKind::Message,
            vec![1],
        );
        office
            .clone()
            .ingest(mailbox.wrap(&event).unwrap())
            .unwrap();
        let current = tokio::time::timeout(Duration::from_secs(2), wait(&mut channel, &prior))
            .await
            .unwrap()
            .unwrap();
        assert_ne!(prior, current);
        drop(channel);
        tasks.join_next().await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn watch_protocol_sixty_four_waiters_leave_ordinary_mailbox_available() {
        let root = tempfile::tempdir().unwrap();
        let endpoint =
            crate::postoffice::federation::IndexedRelayEndpoint::new(office(&root), None, None);
        let mut tasks = tokio::task::JoinSet::new();
        let mut channels = Vec::new();
        for _ in 0..64 {
            let mut channel = endpoint_channel(endpoint.clone(), &mut tasks).await;
            assert!(negotiate(&mut channel).await.unwrap());
            let prior = observe(&mut channel, &[probe()]).await.unwrap();
            channel
                .send(&enc(&Frame::Wait(prior)).unwrap())
                .await
                .unwrap();
            channels.push(channel);
        }
        let mut excess = endpoint_channel(endpoint.clone(), &mut tasks).await;
        assert!(matches!(
            negotiate(&mut excess).await,
            Err(SessionError::ResourceUnavailable)
        ));
        let mut ordinary = endpoint_channel(endpoint, &mut tasks).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            assert!(
                super::super::mailbox::negotiate(&mut ordinary, probe())
                    .await
                    .unwrap()
            );
            assert!(
                super::super::mailbox::delta(&mut ordinary, probe(), None)
                    .await
                    .unwrap()
                    .complete
            );
        })
        .await
        .unwrap();
        drop(channels);
        drop(excess);
        drop(ordinary);
        while let Some(joined) = tasks.join_next().await {
            joined.unwrap();
        }
    }

    #[tokio::test]
    async fn watch_protocol_negotiates_observes_and_releases_on_eof() {
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let identity = DeviceIdentity::generate();
        let client_identity = DeviceIdentity::generate();
        let permits = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let server_permits = permits.clone();
        let (client, server) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(server, &identity).await.unwrap();
            assert!(is_probe(&channel.recv().await.unwrap()));
            serve(&mut channel, &office, &server_permits).await.unwrap();
        });
        let mut channel = SecureChannel::connect(client, &client_identity, None)
            .await
            .unwrap();
        assert!(negotiate(&mut channel).await.unwrap());
        let tokens = observe(&mut channel, &[probe()]).await.unwrap();
        channel
            .send(&enc(&Frame::Wait(tokens)).unwrap())
            .await
            .unwrap();
        tokio::task::yield_now().await;
        drop(channel);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(permits.available_permits(), 1);
    }
    #[tokio::test]
    async fn watch_protocol_quiet_filters_unrelated_and_returns_committed_change() {
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let endpoint =
            crate::postoffice::federation::IndexedRelayEndpoint::new(office.clone(), None, None);
        let identity = DeviceIdentity::generate();
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let mailbox =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap();
        let (client, server) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let channel = SecureChannel::accept(server, &identity).await.unwrap();
            endpoint.serve_connection_checked(channel, None).await
        });
        let mut channel = SecureChannel::connect(client, &author, None).await.unwrap();
        assert!(negotiate(&mut channel).await.unwrap());
        let tokens = observe(&mut channel, &[mailbox.id()]).await.unwrap();
        channel
            .send(&enc(&Frame::Wait(tokens.clone())).unwrap())
            .await
            .unwrap();
        // Commit unrelated mailbox traffic before checking the quiet predicate.
        let unrelated = crate::postoffice::opaque::RelayMailbox::federated(
            &author,
            &DeviceIdentity::generate().public(),
        )
        .unwrap();
        let other = crate::eventlog::Event::new(
            &author,
            probe(),
            1,
            vec![],
            1,
            1,
            crate::eventlog::EventKind::Message,
            vec![8; 16],
        );
        office
            .clone()
            .ingest(unrelated.wrap(&other).unwrap())
            .unwrap();
        {
            let receive = channel.recv();
            tokio::pin!(receive);
            assert!(
                tokio::time::timeout(Duration::from_millis(30), &mut receive)
                    .await
                    .is_err()
            );
            let inner = crate::eventlog::Event::new(
                &author,
                probe(),
                1,
                vec![],
                1,
                1,
                crate::eventlog::EventKind::Message,
                vec![7; 16],
            );
            office
                .clone()
                .ingest(mailbox.wrap(&inner).unwrap())
                .unwrap();
            let bytes = tokio::time::timeout(Duration::from_secs(2), &mut receive)
                .await
                .unwrap()
                .unwrap();
            match dec(&bytes).unwrap() {
                Frame::Snapshot(current) => assert_ne!(tokens, current),
                _ => panic!("expected snapshot"),
            }
        }
        drop(channel);
        // Idle connection closure is an ordinary transport termination.
        let _closed = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn watch_protocol_busy_is_transient_not_unsupported() {
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let (client, server) = tokio::io::duplex(65536);
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(server, &DeviceIdentity::generate())
                .await
                .unwrap();
            assert!(is_probe(&channel.recv().await.unwrap()));
            serve(
                &mut channel,
                &office,
                &std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            )
            .await
            .unwrap();
        });
        let mut channel = SecureChannel::connect(client, &DeviceIdentity::generate(), None)
            .await
            .unwrap();
        assert!(matches!(
            negotiate(&mut channel).await,
            Err(SessionError::ResourceUnavailable)
        ));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn watch_protocol_legacy_false_is_only_unsupported_response() {
        for matched in [false, true] {
            let (client, server) = tokio::io::duplex(65536);
            let identity = DeviceIdentity::generate();
            let task = tokio::spawn(async move {
                let mut channel = SecureChannel::accept(server, &identity).await.unwrap();
                assert!(is_probe(&channel.recv().await.unwrap()));
                channel
                    .send(&encode(&SyncWire::FpResponse(FpResponse { matched })).unwrap())
                    .await
                    .unwrap();
            });
            let mut channel = SecureChannel::connect(client, &DeviceIdentity::generate(), None)
                .await
                .unwrap();
            if matched {
                assert!(negotiate(&mut channel).await.is_err());
            } else {
                assert!(!negotiate(&mut channel).await.unwrap());
            }
            task.await.unwrap();
        }
    }
}

use super::*;
use crate::eventlog::repository::{EventDomainId, EventRepository};
use crate::eventlog::{Event, EventLog};
use crate::identity::device::DeviceIdentity;
use crate::storage::database::EncryptedDatabase;
use std::sync::Arc;

#[tokio::test]
async fn paged_sql_and_legacy_real_tcp_converge_both_directions() {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        for sql_requests in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let db = Arc::new(
                EncryptedDatabase::open(&dir.path().join("profile.db"), "pw", b"profile").unwrap(),
            );
            let repository = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
            let signer = DeviceIdentity::generate();
            let conversation = ConversationId::new([7; 32]);
            let mut log = EventLog::default();
            let mut parent = None;
            let mut missing_roots = Vec::new();
            // More than one Have chunk. SQL-only roots are inserted opposite
            // canonical clock priority; their child has an inverted signed clock.
            for seq in 1..=1501 {
                let event = Event::new(
                    &signer,
                    conversation,
                    seq,
                    vec![],
                    2000 - seq,
                    0,
                    EventKind::Message,
                    vec![],
                );
                repository.append(&event).unwrap();
                if seq <= 2 {
                    missing_roots.push(event.id);
                } else {
                    log.append(event.clone()).unwrap();
                }
                parent = Some(event.id);
            }
            let sql_extra = Event::new(
                &signer,
                conversation,
                1502,
                missing_roots,
                0,
                0,
                EventKind::Message,
                vec![1],
            );
            let legacy_extra = Event::new(
                &signer,
                conversation,
                1503,
                parent.into_iter().collect(),
                1,
                0,
                EventKind::Message,
                vec![2],
            );
            repository.append(&sql_extra).unwrap();
            log.append(legacy_extra.clone()).unwrap();
            let sql = Arc::new(Mutex::new(TinyPagedStore::new(repository)));
            let legacy = Arc::new(Mutex::new(log));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_sql = sql.clone();
            let server_legacy = legacy.clone();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(stream, &DeviceIdentity::generate())
                    .await
                    .unwrap();
                loop {
                    let served = if sql_requests {
                        serve_one(&mut channel, &server_legacy).await
                    } else {
                        serve_one_paged(&mut channel, &server_sql).await
                    };
                    match served.unwrap() {
                        Served::Closed => break,
                        Served::Handled(_) => {}
                    }
                }
            });
            let mut channel = SecureChannel::connect(
                tokio::net::TcpStream::connect(address).await.unwrap(),
                &DeviceIdentity::generate(),
                None,
            )
            .await
            .unwrap();
            if sql_requests {
                request_round_paged(&mut channel, &sql, conversation)
                    .await
                    .unwrap();
            } else {
                request_round(&mut channel, &legacy, conversation)
                    .await
                    .unwrap();
            }
            if !sql_requests {
                assert!(
                    request_durable_have(&mut channel, conversation, legacy_extra.id)
                        .await
                        .unwrap()
                );
            }
            drop(channel);
            server.await.unwrap();
            assert!(sql.lock().unwrap().repo.has(&legacy_extra.id).unwrap());
            assert!(legacy.lock().unwrap().get(&sql_extra.id).is_some());
        }
    })
    .await
    .expect("mixed paged/legacy TCP exchange timed out");
}

use crate::eventlog::sync::network::{
    ExportPage, ExportPageBudget, IdPageBudget, SyncMetadata, SyncPage,
};
use crate::eventlog::{AppendOutcome, EventId, LogError};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Metadata,
    Have(usize),
    Export(usize),
    IngestSecond,
    Durable,
    RevokeMetadata,
    RevokeHave,
    HideHave,
    Count(u64),
    AppendControlMetadata,
    AppendControlHave,
}
/// Both collecting methods panic. Tiny SQL pages force empty continuation scans
/// through a long known prefix; this store cannot accidentally use SDK fallback.
struct TinyPagedStore {
    repo: EventRepository,
    fault: Fault,
    have_calls: AtomicUsize,
    export_calls: AtomicUsize,
    revoked: AtomicBool,
    ingests: usize,
    preparation_rows: AtomicUsize,
    preparation_calls: AtomicUsize,
}
impl TinyPagedStore {
    fn new(repo: EventRepository) -> Self {
        Self {
            repo,
            fault: Fault::None,
            have_calls: AtomicUsize::new(0),
            export_calls: AtomicUsize::new(0),
            revoked: AtomicBool::new(false),
            ingests: 0,
            preparation_rows: AtomicUsize::new(0),
            preparation_calls: AtomicUsize::new(0),
        }
    }
    fn append_control(&self) -> Result<(), LogError> {
        self.repo.append(&Event::new(
            &DeviceIdentity::generate(),
            ConversationId::new([7; 32]),
            1,
            vec![],
            0,
            0,
            EventKind::ChannelAccountAuthority,
            vec![],
        ))?;
        Ok(())
    }
    fn error() -> LogError {
        std::io::Error::other("injected paged storage failure").into()
    }
}
impl SyncStore for TinyPagedStore {
    fn event_ids(&self, _: &ConversationId) -> Result<Vec<EventId>, LogError> {
        panic!("collecting IDs forbidden")
    }
    fn events_excluding(
        &self,
        _: &ConversationId,
        _: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError> {
        panic!("collecting bodies forbidden")
    }
    fn ingest(&mut self, _: Event) -> Result<AppendOutcome, LogError> {
        panic!("collecting SDK ingest forbidden")
    }
}
impl NetworkSyncStore for TinyPagedStore {
    type View = <EventRepository as NetworkSyncStore>::View;
    type HaveCursor = <EventRepository as NetworkSyncStore>::HaveCursor;
    type ExportCursor = <EventRepository as NetworkSyncStore>::ExportCursor;
    fn open_view(&self, c: &ConversationId) -> Result<Self::View, LogError> {
        self.repo.open_view(c)
    }
    fn prepare_view_step(
        &self,
        view: &Self::View,
        budget: crate::eventlog::sync::network::PreparationBudget,
    ) -> Result<crate::eventlog::sync::network::PreparationStep, LogError> {
        budget.validate()?;
        self.validate_view(view)?;
        self.preparation_calls.fetch_add(1, Ordering::Relaxed);
        let before = self.preparation_rows.load(Ordering::Relaxed);
        let rows = before.min(budget.rows);
        self.preparation_rows
            .store(before - rows, Ordering::Relaxed);
        Ok(crate::eventlog::sync::network::PreparationStep {
            ready: before == rows,
            rows,
            bytes: rows * 32,
            edges: 0,
        })
    }
    fn metadata(&self, v: &Self::View) -> Result<SyncMetadata, LogError> {
        if matches!(self.fault, Fault::Metadata) {
            return Err(Self::error());
        }
        let mut metadata = NetworkSyncStore::metadata(&self.repo, v)?;
        if let Fault::Count(count) = self.fault {
            metadata.count = count;
        }
        if matches!(self.fault, Fault::HideHave) {
            metadata.count = 0;
            metadata.fingerprint = None;
        }
        if matches!(self.fault, Fault::RevokeMetadata) {
            self.revoked.store(true, Ordering::Relaxed);
        }
        if matches!(self.fault, Fault::AppendControlMetadata) {
            self.append_control()?;
        }
        Ok(metadata)
    }
    fn validate_view(&self, v: &Self::View) -> Result<(), LogError> {
        if self.revoked.load(Ordering::Relaxed) {
            return Err(LogError::StaleCursor);
        }
        self.repo.validate_view(v)
    }
    fn have_page(
        &self,
        v: &Self::View,
        a: Option<&Self::HaveCursor>,
        mut b: IdPageBudget,
    ) -> Result<SyncPage<EventId, Self::HaveCursor>, LogError> {
        if matches!(self.fault, Fault::HideHave) {
            return Ok(SyncPage {
                items: vec![],
                next: None,
                examined: 0,
            });
        }
        let call = self.have_calls.fetch_add(1, Ordering::Relaxed) + 1;
        if matches!(self.fault,Fault::Have(n) if n==call) {
            return Err(Self::error());
        }
        self.validate_view(v)?;
        b.ids = b.ids.min(1);
        b.examined = b.examined.min(1);
        let page = self.repo.have_page(v, a, b)?;
        if matches!(self.fault, Fault::RevokeHave) {
            self.revoked.store(true, Ordering::Relaxed);
        }
        if matches!(self.fault, Fault::AppendControlHave) {
            self.append_control()?;
        }
        Ok(page)
    }
    fn export_page(
        &self,
        v: &Self::View,
        h: &HashSet<EventId>,
        a: Option<&Self::ExportCursor>,
        mut b: ExportPageBudget,
    ) -> Result<ExportPage<Self::ExportCursor>, LogError> {
        let call = self.export_calls.fetch_add(1, Ordering::Relaxed) + 1;
        if matches!(self.fault,Fault::Export(n) if n==call) {
            return Err(Self::error());
        }
        self.validate_view(v)?;
        b.examined = 1;
        self.repo.export_page(v, h, a, b)
    }
    fn ingest(&mut self, e: Event) -> Result<AppendOutcome, LogError> {
        self.ingests += 1;
        if matches!(self.fault, Fault::IngestSecond) && self.ingests == 2 {
            return Err(Self::error());
        }
        self.repo.append(&e)
    }
    fn durable_have(&self, c: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        if matches!(self.fault, Fault::Durable) {
            return Err(Self::error());
        }
        // The SQL commit is already durable. Re-read/fully validate the exact
        // retained body now; no checkpoint or write is part of this qualification.
        Ok(self
            .repo
            .get(id)?
            .is_some_and(|event| event.conversation_id == *c))
    }
    fn file_consent(&self, _: &ConversationId, _: &EventId) -> Result<FileConsentStatus, LogError> {
        Ok(FileConsentStatus::Pending)
    }
    fn signed_file_consent(
        &self,
        _: &ConversationId,
        _: &EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    fn admission_denied(&self) -> bool {
        false
    }
}
fn tiny_fixture() -> (
    tempfile::TempDir,
    TinyPagedStore,
    DeviceIdentity,
    ConversationId,
) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(
        EncryptedDatabase::open(&dir.path().join("profile.db"), "pw", b"profile").unwrap(),
    );
    let repo = EventRepository::new(db, EventDomainId([1; 16])).unwrap();
    (
        dir,
        TinyPagedStore::new(repo),
        DeviceIdentity::generate(),
        ConversationId::new([7; 32]),
    )
}
#[tokio::test]
async fn paged_cached_probe_rechecks_authority_before_transport_handoff() {
    let (_dir, mut store, signer, c) = tiny_fixture();
    let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
    store.repo.append(&event).unwrap();
    store.repo.fingerprint_and_len(&c).unwrap();
    store.fault = Fault::RevokeMetadata;
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        request_round_paged(&mut client, &Mutex::new(store), c),
    )
    .await;
    assert!(
        matches!(result, Ok(Err(SessionError::Store(LogError::StaleCursor)))),
        "revocation must fail locally before the peer sees a digest: {result:?}"
    );
    drop(client);
    assert!(server.recv().await.is_err());
}
#[tokio::test]
async fn paged_local_failures_close_without_terminal_have_or_positive_custody() {
    for fault in [
        Fault::Metadata,
        Fault::Have(1),
        Fault::Have(2),
        Fault::Export(1),
        Fault::Export(2),
        Fault::Durable,
        Fault::RevokeHave,
    ] {
        let (_dir, mut store, signer, c) = tiny_fixture();
        let mut events = vec![];
        for seq in 1..=5 {
            let e = Event::new(&signer, c, seq, vec![], seq, 0, EventKind::Message, vec![]);
            store.repo.append(&e).unwrap();
            events.push(e);
        }
        store.fault = fault;
        let (a, b) = tokio::io::duplex(65536);
        let peer = DeviceIdentity::generate();
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &signer, None),
            SecureChannel::accept(b, &peer)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        let task = tokio::spawn(async move {
            let result = serve_one_paged(&mut server, &Mutex::new(store)).await;
            if matches!(fault, Fault::RevokeHave) {
                assert!(matches!(
                    result,
                    Err(SessionError::Store(LogError::StaleCursor))
                ));
            } else {
                let error = result.unwrap_err();
                assert!(matches!(error, SessionError::Store(LogError::Io(_))));
                let source = std::error::Error::source(&error).unwrap();
                assert!(
                    source
                        .source()
                        .unwrap()
                        .downcast_ref::<std::io::Error>()
                        .is_some()
                );
            }
        });
        if matches!(fault, Fault::Durable) {
            client
                .send(
                    &encode(&SyncWire::DurableHaveRequest {
                        conversation: c,
                        event_id: events[0].id,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
        } else {
            client
                .send(
                    &encode(&SyncWire::RequestV4(SyncRequest {
                        conversation: c,
                        have: vec![],
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            send_have(&mut client, c, &[events[0].id], SyncWire::ReqHave)
                .await
                .unwrap();
        }
        while let Ok(bytes) = client.recv().await {
            match decode(&bytes).unwrap() {
                SyncWire::RespHave(chunk) => assert!(
                    chunk.more,
                    "local page failure must not advertise completed Have"
                ),
                SyncWire::DurableHaveResponse { stored: true, .. } => {
                    panic!("failed exchange must not acknowledge custody")
                }
                _ => {}
            }
        }
        task.await.unwrap();
    }
}
#[tokio::test]
async fn paged_second_write_failure_preserves_prefix_and_closes_exchange() {
    let (_dir, mut store, signer, c) = tiny_fixture();
    store.fault = Fault::IngestSecond;
    let root = Event::new(&signer, c, 1, vec![], 10, 0, EventKind::Message, vec![]);
    let child = Event::new(
        &signer,
        c,
        2,
        vec![root.id],
        1,
        0,
        EventKind::Message,
        vec![],
    );
    let store = Arc::new(Mutex::new(store));
    let server_store = store.clone();
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        assert!(matches!(
            serve_one_paged(&mut server, &server_store).await,
            Err(SessionError::Store(LogError::Io(_)))
        ));
    });
    client
        .send(
            &encode(&SyncWire::FollowupV4(SyncFollowup {
                conversation: c,
                events: vec![root.clone(), child.clone()],
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(client.recv().await.is_err());
    task.await.unwrap();
    assert!(store.lock().unwrap().repo.has(&root.id).unwrap());
    assert!(!store.lock().unwrap().repo.has(&child.id).unwrap());
}
#[test]
fn paged_batch_classifies_peer_rejections_without_losing_valid_suffix() {
    let (_dir, store, signer, c) = tiny_fixture();
    let backend_store = Mutex::new(store);
    let backend = PagedNetworkBackend(&backend_store, None);
    let root = Event::new(&signer, c, 1, vec![], 10, 0, EventKind::Message, vec![]);
    let mut invalid = root.clone();
    invalid.sig[0] ^= 1;
    let child = Event::new(
        &signer,
        c,
        2,
        vec![root.id],
        1,
        0,
        EventKind::Message,
        vec![],
    );
    let report = backend.apply(&[invalid, root, child]).unwrap();
    assert_eq!(report.applied, 2);
    assert_eq!(report.rejected.len(), 1);
}
#[tokio::test]
async fn paged_cached_fingerprint_hit_skips_id_and_body_pages() {
    let (_dir, store, signer, c) = tiny_fixture();
    let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
    store.repo.append(&event).unwrap();
    store.repo.fingerprint_and_len(&c).unwrap();
    let mut log = EventLog::default();
    log.append(event).unwrap();
    let store = Arc::new(Mutex::new(store));
    let observed = store.clone();
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        serve_one(&mut server, &Mutex::new(log)).await.unwrap();
    });
    request_round_paged(&mut client, &store, c).await.unwrap();
    task.await.unwrap();
    let store = observed.lock().unwrap();
    assert_eq!(store.have_calls.load(Ordering::Relaxed), 0);
    assert_eq!(store.export_calls.load(Ordering::Relaxed), 0);
}
#[tokio::test]
async fn paged_exact_custody_reads_current_body_and_conversation() {
    let (_dir, store, signer, c) = tiny_fixture();
    let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
    store.repo.append(&event).unwrap();
    let id = event.id;
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        let store = Mutex::new(store);
        for _ in 0..3 {
            serve_one_paged(&mut server, &store).await.unwrap();
        }
    });
    assert!(
        !request_durable_have(&mut client, ConversationId::new([9; 32]), id)
            .await
            .unwrap()
    );
    assert!(
        !request_durable_have(&mut client, c, EventId::new([9; 32]))
            .await
            .unwrap()
    );
    assert!(request_durable_have(&mut client, c, id).await.unwrap());
    task.await.unwrap();
}
#[tokio::test]
async fn streamed_have_preserves_raw_id_and_chunk_protective_limits() {
    for (chunks, per_chunk, terminal, accepted) in [
        (1, 1501, true, false),
        (1001, 0, false, false),
        (134, 1500, true, false),
        (134, 1500, true, true),
    ] {
        let signer = DeviceIdentity::generate();
        let peer = DeviceIdentity::generate();
        let c = ConversationId::new([7; 32]);
        let (a, b) = tokio::io::duplex(65536);
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &signer, None),
            SecureChannel::accept(b, &peer)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        let task = tokio::spawn(async move {
            let received = recv_have(&mut server, true, c).await;
            if accepted {
                assert_eq!(received.unwrap().len(), 200000);
            } else {
                assert!(matches!(received, Err(SessionError::UnexpectedMessage)));
            }
        });
        for index in 0..chunks {
            let size = if chunks == 134 && index + 1 == chunks {
                if accepted { 500 } else { 501 }
            } else {
                per_chunk
            };
            // Duplicate IDs still count against the raw cumulative receive cap.
            let bytes = encode(&SyncWire::ReqHave(HaveChunk {
                conversation: c,
                ids: vec![EventId::new([1; 32]); size],
                more: !terminal || index + 1 < chunks,
            }))
            .unwrap();
            if client.send(&bytes).await.is_err() {
                break;
            }
        }
        drop(client);
        task.await.unwrap();
    }
    assert!(matches!(
        have_count(200001),
        Err(SessionError::UnexpectedMessage)
    ));
    assert_eq!(have_count(200000).unwrap(), 200000);
}
#[tokio::test]
async fn paged_export_uses_actual_frame_framing_and_rejects_one_byte_oversize() {
    for excess in [0, 1] {
        let (_dir, store, signer, c) = tiny_fixture();
        let blank = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
        let payload = MAX_PLAINTEXT
            - encode(&SyncWire::Response(SyncResponse {
                conversation: c,
                events: vec![],
                have: vec![],
            }))
            .unwrap()
            .len()
            - crate::eventlog::sync::encoded_event_size(&blank)
            + excess;
        let event = Event::new(
            &signer,
            c,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![42; payload],
        );
        store.repo.append(&event).unwrap();
        let store = Mutex::new(store);
        let backend = PagedNetworkBackend(&store, None);
        let view = backend.open_view(&c).unwrap();
        let result = select_export_frames(&backend, &view, c, &HashSet::new(), 1).await;
        if excess == 0 {
            let frame = result.unwrap().pop().unwrap();
            let bytes = encode(&SyncWire::Response(SyncResponse {
                conversation: c,
                events: frame.events,
                have: vec![],
            }))
            .unwrap();
            assert_eq!(bytes.len(), MAX_PLAINTEXT);
        } else {
            assert!(matches!(
                result,
                Err(SessionError::Store(LogError::PageBudgetTooSmall { .. }))
            ));
        }
    }
}
#[tokio::test]
async fn paged_metadata_capability_upgrade_converges_without_optional_fingerprint() {
    let (_dir, store, signer, c) = tiny_fixture();
    let event = Event::new(
        &signer,
        c,
        1,
        vec![],
        1,
        0,
        EventKind::ChannelAccountAuthority,
        vec![],
    );
    let id = event.id;
    let mut log = EventLog::default();
    log.append(event).unwrap();
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        let store = Mutex::new(log);
        while !matches!(
            serve_one(&mut server, &store).await.unwrap(),
            Served::Closed
        ) {}
    });
    let store = Mutex::new(store);
    request_round_paged(&mut client, &store, c).await.unwrap();
    drop(client);
    task.await.unwrap();
    assert!(store.lock().unwrap().repo.has(&id).unwrap());
}
#[tokio::test]
async fn paged_file_burst_fills_four_bounded_frames_across_tiny_scan_pages() {
    let (_dir, store, signer, c) = tiny_fixture();
    let mut parent = None;
    for seq in 1..=8 {
        let event = Event::new(
            &signer,
            c,
            seq,
            parent.into_iter().collect(),
            seq,
            0,
            EventKind::Message,
            vec![3; 40000],
        );
        store.repo.append(&event).unwrap();
        parent = Some(event.id);
    }
    let store = Mutex::new(store);
    let backend = PagedNetworkBackend(&store, None);
    let view = backend.open_view(&c).unwrap();
    let frames = select_export_frames(&backend, &view, c, &HashSet::new(), FILE_FOLLOWUP_BURST)
        .await
        .unwrap();
    assert_eq!(frames.len(), 4);
    let mut oracle = EventLog::default();
    for frame in frames {
        assert_eq!(frame.events.len(), 1);
        assert!(encode(&SyncWire::Followup(frame.clone())).unwrap().len() <= MAX_PLAINTEXT);
        for event in frame.events {
            oracle.append(event).unwrap();
        }
    }
}
#[tokio::test]
async fn cancelling_paged_exchange_releases_view_and_profile_lease() {
    use crate::storage::profile::{ProfileStorageLease, context::ProfileStorage};
    let dir = tempfile::tempdir().unwrap();
    let signer = DeviceIdentity::generate();
    let c = ConversationId::new([7; 32]);
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(dir.path()).unwrap(),
        &signer.public(),
        &dir.path().join("events"),
        "pw",
        12,
    )
    .unwrap();
    let repo = profile.events().unwrap();
    repo.append(&Event::new(
        &signer,
        c,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![],
    ))
    .unwrap();
    drop(profile);
    let store = Arc::new(Mutex::new(TinyPagedStore::new(repo)));
    let task_store = store.clone();
    let (a, b) = tokio::io::duplex(65536);
    let task = tokio::spawn(async move {
        let mut channel = SecureChannel::connect(a, &signer, None).await.unwrap();
        request_round_paged(&mut channel, &task_store, c).await
    });
    let mut peer = SecureChannel::accept(b, &DeviceIdentity::generate())
        .await
        .unwrap();
    assert!(matches!(
        decode(&peer.recv().await.unwrap()).unwrap(),
        SyncWire::Request(_)
    ));
    assert!(matches!(
        decode(&peer.recv().await.unwrap()).unwrap(),
        SyncWire::ReqHave(HaveChunk { more: false, .. })
    ));
    assert!(
        store.try_lock().is_ok(),
        "network receive must not retain a store lock"
    );
    assert!(ProfileStorageLease::acquire(dir.path()).is_err());
    drop(store);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert!(ProfileStorageLease::acquire(dir.path()).is_ok());
    assert!(peer.recv().await.is_err());
}
#[tokio::test]
async fn paged_projected_duplicates_stop_without_remote_acceptance() {
    let (_a_dir, a, signer, c) = tiny_fixture();
    let (_b_dir, mut b, _, _) = tiny_fixture();
    let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
    a.repo.append(&event).unwrap();
    b.repo.append(&event).unwrap();
    b.fault = Fault::HideHave;
    let (aio, bio) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(aio, &signer, None),
        SecureChannel::accept(bio, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        let store = Mutex::new(b);
        let mut handled = 0;
        loop {
            match serve_one_paged(&mut server, &store).await.unwrap() {
                Served::Closed => return handled,
                Served::Handled(_) => {
                    handled += 1;
                    assert!(handled < 9);
                }
            }
        }
    });
    assert!(matches!(
        request_round_paged(&mut client, &Mutex::new(a), c).await,
        Err(SessionError::NoProgress)
    ));
    drop(client);
    assert!(task.await.unwrap() < 9);
}

#[tokio::test]
async fn legacy_cached_probe_never_collects_local_ids_or_bodies() {
    struct CachedMetadataOnly;
    impl SyncStore for CachedMetadataOnly {
        fn fingerprint_and_len(&self, _: &ConversationId) -> Result<([u8; 32], usize), LogError> {
            Ok(([9; 32], 200001))
        }
        fn requires_channel_v2(&self, _: &ConversationId) -> Result<bool, LogError> {
            Ok(false)
        }
        fn requires_channel_v3(&self, _: &ConversationId) -> Result<bool, LogError> {
            Ok(false)
        }
        fn requires_channel_v4(&self, _: &ConversationId) -> Result<bool, LogError> {
            Ok(false)
        }
        fn requires_channel_v5(&self, _: &ConversationId) -> Result<bool, LogError> {
            Ok(false)
        }
        fn event_ids(&self, _: &ConversationId) -> Result<Vec<EventId>, LogError> {
            panic!("cached legacy probe collected IDs")
        }
        fn events_excluding(
            &self,
            _: &ConversationId,
            _: &HashSet<EventId>,
        ) -> Result<Vec<Event>, LogError> {
            panic!("cached legacy probe collected bodies")
        }
        fn ingest(&mut self, _: Event) -> Result<AppendOutcome, LogError> {
            panic!("cached probe ingested history")
        }
    }
    let a = DeviceIdentity::generate();
    let b = DeviceIdentity::generate();
    let (aio, bio) = tokio::io::duplex(65536);
    let (client, server) = tokio::join!(
        SecureChannel::connect(aio, &a, None),
        SecureChannel::accept(bio, &b)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        serve_one(&mut server, &Mutex::new(CachedMetadataOnly))
            .await
            .unwrap();
    });
    request_round(
        &mut client,
        &Mutex::new(CachedMetadataOnly),
        ConversationId::new([7; 32]),
    )
    .await
    .unwrap();
    task.await.unwrap();
}
#[tokio::test]
async fn paged_terminal_have_requires_snapshot_count_agreement() {
    let (_dir, mut store, signer, c) = tiny_fixture();
    store
        .repo
        .append(&Event::new(
            &signer,
            c,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![],
        ))
        .unwrap();
    store.fault = Fault::Count(2);
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        assert!(matches!(
            serve_one_paged(&mut server, &Mutex::new(store)).await,
            Err(SessionError::Store(LogError::CorruptFile(_)))
        ));
    });
    client
        .send(
            &encode(&SyncWire::RequestV4(SyncRequest {
                conversation: c,
                have: vec![],
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    send_have(&mut client, c, &[], SyncWire::ReqHave)
        .await
        .unwrap();
    while let Ok(bytes) = client.recv().await {
        assert!(
            !matches!(
                decode(&bytes).unwrap(),
                SyncWire::RespHave(HaveChunk { more: false, .. })
            ),
            "count mismatch must abort before terminal Have"
        );
    }
    task.await.unwrap();
}

#[tokio::test]
async fn paged_cached_overcap_match_succeeds_without_have_collection() {
    let (_a_dir, mut a, signer, c) = tiny_fixture();
    let (_b_dir, mut b, _, _) = tiny_fixture();
    let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
    // Synthetic authoritative counts exercise the protective boundary without
    // creating 200k signed events. Neither endpoint is allowed to collect IDs.
    for store in [&mut a, &mut b] {
        store.repo.append(&event).unwrap();
        store.repo.fingerprint_and_len(&c).unwrap();
        store.fault = Fault::Count(200001);
    }
    let (aio, bio) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(aio, &signer, None),
        SecureChannel::accept(bio, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        let store = Mutex::new(b);
        serve_one_paged(&mut server, &store).await.unwrap();
        assert_eq!(store.lock().unwrap().have_calls.load(Ordering::Relaxed), 0);
    });
    let store = Mutex::new(a);
    request_round_paged(&mut client, &store, c).await.unwrap();
    task.await.unwrap();
    assert_eq!(store.lock().unwrap().have_calls.load(Ordering::Relaxed), 0);
    assert_eq!(
        store.lock().unwrap().export_calls.load(Ordering::Relaxed),
        0
    );
}
#[tokio::test]
async fn paged_overcap_mismatch_and_miss_fail_before_have_exchange() {
    for cached in [false, true] {
        let (_dir, mut store, signer, c) = tiny_fixture();
        let event = Event::new(&signer, c, 1, vec![], 1, 0, EventKind::Message, vec![]);
        store.repo.append(&event).unwrap();
        if cached {
            store.repo.fingerprint_and_len(&c).unwrap();
        }
        store.fault = Fault::Count(200001);
        let (a, b) = tokio::io::duplex(65536);
        let peer = DeviceIdentity::generate();
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &signer, None),
            SecureChannel::accept(b, &peer)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        let task = tokio::spawn(async move {
            if cached {
                assert!(matches!(
                    decode(&server.recv().await.unwrap()).unwrap(),
                    SyncWire::FpRequest(_)
                ));
                server
                    .send(&encode(&SyncWire::FpResponse(FpResponse { matched: false })).unwrap())
                    .await
                    .unwrap();
            }
            assert!(
                server.recv().await.is_err(),
                "over-cap view must not start a Have exchange"
            );
        });
        assert!(matches!(
            request_round_paged(&mut client, &Mutex::new(store), c).await,
            Err(SessionError::UnexpectedMessage)
        ));
        drop(client);
        task.await.unwrap();
    }
}
#[tokio::test]
async fn paged_responder_actual_request_preflights_have_count() {
    let (_dir, mut store, signer, c) = tiny_fixture();
    store.fault = Fault::Count(200001);
    let (a, b) = tokio::io::duplex(65536);
    let peer = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &signer, None),
        SecureChannel::accept(b, &peer)
    );
    let mut client = client.unwrap();
    let mut server = server.unwrap();
    let task = tokio::spawn(async move {
        assert!(matches!(
            serve_one_paged(&mut server, &Mutex::new(store)).await,
            Err(SessionError::UnexpectedMessage)
        ));
    });
    client
        .send(
            &encode(&SyncWire::RequestV4(SyncRequest {
                conversation: c,
                have: vec![],
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    send_have(&mut client, c, &[], SyncWire::ReqHave)
        .await
        .unwrap();
    assert!(client.recv().await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn paged_capability_append_during_metadata_or_have_prevents_handoff() {
    for fault in [Fault::AppendControlMetadata, Fault::AppendControlHave] {
        let (_dir, mut store, signer, c) = tiny_fixture();
        store
            .repo
            .append(&Event::new(
                &signer,
                c,
                1,
                vec![],
                1,
                0,
                EventKind::Message,
                vec![],
            ))
            .unwrap();
        store.repo.fingerprint_and_len(&c).unwrap();
        store.fault = fault;
        let (a, b) = tokio::io::duplex(65536);
        let peer = DeviceIdentity::generate();
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &signer, None),
            SecureChannel::accept(b, &peer)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        if matches!(fault, Fault::AppendControlMetadata) {
            let result = tokio::time::timeout(
                std::time::Duration::from_millis(300),
                request_round_paged(&mut client, &Mutex::new(store), c),
            )
            .await;
            assert!(
                matches!(result, Ok(Err(SessionError::Store(LogError::StaleCursor)))),
                "raced SQL upgrade must abort old fingerprint handoff: {result:?}"
            );
            drop(client);
            assert!(server.recv().await.is_err());
        } else {
            let task = tokio::spawn(async move {
                assert!(matches!(
                    serve_one_paged(&mut server, &Mutex::new(store)).await,
                    Err(SessionError::Store(LogError::StaleCursor))
                ));
            });
            client
                .send(
                    &encode(&SyncWire::Request(SyncRequest {
                        conversation: c,
                        have: vec![],
                    }))
                    .unwrap(),
                )
                .await
                .unwrap();
            send_have(&mut client, c, &[], SyncWire::ReqHave)
                .await
                .unwrap();
            while let Ok(bytes) = client.recv().await {
                assert!(
                    !matches!(decode(&bytes).unwrap(), SyncWire::RespHave(_)),
                    "raced SQL upgrade must abort old Have handoff"
                );
            }
            task.await.unwrap();
        }
    }
}

#[test]
fn legacy_view_open_and_have_read_never_collect_event_bodies() {
    struct IdsOnly;
    impl SyncStore for IdsOnly {
        fn event_ids(&self, _: &ConversationId) -> Result<Vec<EventId>, LogError> {
            Ok(vec![EventId::new([3; 32])])
        }
        fn events_excluding(
            &self,
            _: &ConversationId,
            _: &HashSet<EventId>,
        ) -> Result<Vec<Event>, LogError> {
            panic!("legacy view opener or Have queried collecting capability defaults")
        }
        fn ingest(&mut self, _: Event) -> Result<AppendOutcome, LogError> {
            unreachable!()
        }
    }
    let store = Mutex::new(IdsOnly);
    let backend = LegacyCollectingNetworkAdapter(&store);
    let view = backend.open_view(&ConversationId::new([7; 32])).unwrap();
    let page = backend
        .have_page(
            &view,
            None,
            IdPageBudget {
                ids: 1500,
                examined: 1500,
            },
        )
        .unwrap();
    assert_eq!(page.items, [EventId::new([3; 32])]);
    assert!(page.next.is_none());
}

#[test]
fn remote_have_budget_is_global_fail_fast_and_holds_conversion_overlap() {
    use super::have_budget::{ReceivedHave, RemoteHaveBudget};
    let budget = RemoteHaveBudget::default();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(budget.try_admit().unwrap());
    }
    assert!(
        budget.try_admit().is_err(),
        "fifth endpoint must fail immediately, never await a symmetric endpoint"
    );
    let reservation = held.pop().unwrap();
    let ids = (0..200_000u64)
        .map(|i| {
            let mut b = [0u8; 32];
            b[..8].copy_from_slice(&i.to_le_bytes());
            EventId::new(b)
        })
        .collect::<Vec<_>>();
    let received = ReceivedHave::from_vec(ids, Some(reservation)).unwrap();
    assert_eq!(received.len(), 200_000);
    assert!(received.accounted_bytes() <= 32 * 1024 * 1024);
    assert!(
        budget.try_admit().is_err(),
        "conversion must retain the same reservation"
    );
    drop(received);
    assert!(budget.try_admit().is_ok());
    held.clear();
    for _ in 0..4 {
        held.push(budget.try_admit().unwrap());
    }
}

#[tokio::test]
async fn indexed_relay_and_legacy_real_tcp_converge_both_directions_with_budget() {
    tokio::time::timeout(std::time::Duration::from_secs(60), async {
        for sql_requests in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let owner_identity = DeviceIdentity::generate();
            let profile = crate::storage::profile::context::ProfileStorage::open_relay(
                crate::storage::profile::ProfileStorageLease::acquire(dir.path()).unwrap(),
                &owner_identity.public(),
                &dir.path().join("relay.log"),
                "pw",
                crate::postoffice::network::unix_now(),
            )
            .unwrap();
            let state = Arc::new(
                crate::postoffice::storage::role::IndexedRelayState::from_profile(
                    profile,
                    crate::postoffice::storage::role::RelayAdmission::LegacyCompatible,
                )
                .unwrap(),
            );
            let budget = RemoteHaveBudget::default();
            let server_budget = budget.clone();
            let signer = DeviceIdentity::generate();
            let conversation = ConversationId::new([7; 32]);
            let mut log = EventLog::default();
            let mut parent = None;
            let mut missing_roots = Vec::new();
            // More than one Have chunk. SQL-only roots are inserted opposite
            // canonical clock priority; their child has an inverted signed clock.
            for seq in 1..=1501 {
                let event = Event::new(
                    &signer,
                    conversation,
                    seq,
                    vec![],
                    2000 - seq,
                    0,
                    EventKind::Message,
                    vec![],
                );
                state
                    .record(&event, crate::postoffice::network::unix_now(), None)
                    .unwrap();
                if seq <= 2 {
                    missing_roots.push(event.id);
                } else {
                    log.append(event.clone()).unwrap();
                }
                parent = Some(event.id);
            }
            let sql_extra = Event::new(
                &signer,
                conversation,
                1502,
                missing_roots,
                0,
                0,
                EventKind::Message,
                vec![1],
            );
            let legacy_extra = Event::new(
                &signer,
                conversation,
                1503,
                parent.into_iter().collect(),
                1,
                0,
                EventKind::Message,
                vec![2],
            );
            state
                .record(&sql_extra, crate::postoffice::network::unix_now(), None)
                .unwrap();
            log.append(legacy_extra.clone()).unwrap();
            let sql = Arc::new(Mutex::new(crate::postoffice::IndexedPostOffice {
                state: state.clone(),
            }));
            let legacy = Arc::new(Mutex::new(log));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server_sql = sql.clone();
            let server_legacy = legacy.clone();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = SecureChannel::accept(stream, &DeviceIdentity::generate())
                    .await
                    .unwrap();
                loop {
                    let served = if sql_requests {
                        serve_one(&mut channel, &server_legacy).await
                    } else {
                        serve_one_budgeted(&mut channel, &server_sql, &server_budget).await
                    };
                    match served.unwrap() {
                        Served::Closed => break,
                        Served::Handled(_) => {}
                    }
                }
            });
            let mut channel = SecureChannel::connect(
                tokio::net::TcpStream::connect(address).await.unwrap(),
                &DeviceIdentity::generate(),
                None,
            )
            .await
            .unwrap();
            if sql_requests {
                request_round_budgeted(&mut channel, &sql, conversation, &budget)
                    .await
                    .unwrap();
            } else {
                request_round(&mut channel, &legacy, conversation)
                    .await
                    .unwrap();
            }
            if !sql_requests {
                assert!(
                    request_durable_have(&mut channel, conversation, legacy_extra.id)
                        .await
                        .unwrap()
                );
            }
            drop(channel);
            server.await.unwrap();
            assert!(
                state
                    .durable_have_at(
                        &conversation,
                        &legacy_extra.id,
                        crate::postoffice::network::unix_now()
                    )
                    .unwrap()
            );
            assert!(legacy.lock().unwrap().get(&sql_extra.id).is_some());
        }
    })
    .await
    .expect("mixed paged/legacy TCP exchange timed out");
}

#[tokio::test]
async fn budgeted_round_saturation_cancellation_and_healthy_retry_release_reservations() {
    async fn exchange(budget: RemoteHaveBudget) -> Result<(), SessionError> {
        let (a, b) = tokio::io::duplex(262144);
        let requester = DeviceIdentity::generate();
        let responder = DeviceIdentity::generate();
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &requester, None),
            SecureChannel::accept(b, &responder)
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(
            EncryptedDatabase::open(&dir.path().join("profile.db"), "pw", b"profile").unwrap(),
        );
        let store = Arc::new(Mutex::new(
            EventRepository::new(db, EventDomainId([1; 16])).unwrap(),
        ));
        let server_store = store.clone();
        let server_budget = budget.clone();
        let task = tokio::spawn(async move {
            loop {
                match serve_one_budgeted(&mut server, &server_store, &server_budget).await? {
                    Served::Closed => return Ok::<(), SessionError>(()),
                    Served::Handled(_) => {}
                }
            }
        });
        let result =
            request_round_budgeted(&mut client, &store, ConversationId::new([7; 32]), &budget)
                .await;
        drop(client);
        let served = task.await.unwrap();
        match served {
            Err(error) => Err(error),
            Ok(_) => result.map(|_| ()),
        }
    }
    let budget = RemoteHaveBudget::default();
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(budget.try_admit().unwrap());
    }
    let results = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(exchange(budget.clone()), exchange(budget.clone()))
    })
    .await
    .expect("symmetric endpoint admission must fail immediately without distributed waiting");
    assert!(matches!(results.0, Err(SessionError::ResourceUnavailable)));
    assert!(matches!(results.1, Err(SessionError::ResourceUnavailable)));
    held.clear();
    tokio::time::timeout(std::time::Duration::from_secs(2), exchange(budget.clone()))
        .await
        .unwrap()
        .unwrap();
    // Cancellation during Have receive must drop its full reservation.
    let (a, b) = tokio::io::duplex(262144);
    let requester = DeviceIdentity::generate();
    let responder = DeviceIdentity::generate();
    let (client, server) = tokio::join!(
        SecureChannel::connect(a, &requester, None),
        SecureChannel::accept(b, &responder)
    );
    let _client = client.unwrap();
    let mut server = server.unwrap();
    let receive = recv_have_owned(
        &mut server,
        true,
        ConversationId::new([7; 32]),
        Some(&budget),
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), receive)
            .await
            .is_err()
    );
    for _ in 0..4 {
        held.push(budget.try_admit().unwrap());
    }
}

#[tokio::test]
async fn paged_preparation_yields_unlocked_and_observes_concurrent_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let database = Arc::new(
        EncryptedDatabase::open(&directory.path().join("profile.db"), "pw", b"profile").unwrap(),
    );
    let store = Arc::new(Mutex::new(TinyPagedStore::new(
        EventRepository::new(database, EventDomainId([1; 16])).unwrap(),
    )));
    store
        .lock()
        .unwrap()
        .preparation_rows
        .store(4096, Ordering::Relaxed);
    let observer = store.clone();
    let revoke = tokio::spawn(async move {
        loop {
            let revoked = {
                let guard = observer
                    .try_lock()
                    .expect("preparation must release the store mutex before yielding");
                let ready = guard.preparation_calls.load(Ordering::Relaxed) > 0;
                if ready {
                    guard.revoked.store(true, Ordering::Relaxed);
                }
                ready
            };
            if revoked {
                break;
            }
            tokio::task::yield_now().await;
        }
    });
    let result = backend::open_prepared_view(
        &PagedNetworkBackend(&store, None),
        &ConversationId::new([7; 32]),
    )
    .await;
    assert!(matches!(
        result,
        Err(SessionError::Store(LogError::StaleCursor))
    ));
    revoke.await.unwrap();
    let guard = store.lock().unwrap();
    assert!(
        guard.preparation_calls.load(Ordering::Relaxed) < 64,
        "a competing task must run before preparation drains the source"
    );
    assert!(guard.preparation_rows.load(Ordering::Relaxed) > 0);
}

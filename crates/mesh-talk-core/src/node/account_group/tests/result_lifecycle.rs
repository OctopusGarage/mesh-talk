//! Retired terminal File receipts retain status, never usable manifest/key copies.
use super::*;
use crate::node::account_group::operations::wire::OperationFrame;
use crate::{file::FileKind, node::MessageProtection};
use base64::Engine;

#[derive(Clone, Copy)]
enum Retirement {
    Expiry,
    Recall,
    Erase,
}

async fn retired_file_result_is_status_only(retirement: Retirement) {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let mut source = node(&dir.path().join("source"), &account);
    let owner = node(&dir.path().join("owner"), &Account::generate());
    let genesis = legacy_group(&owner, &source);
    let group = genesis.conversation_id;
    install(&source, &genesis);
    let path = dir.path().join("protected.png");
    let bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    std::fs::write(&path, bytes).unwrap();
    let deadline = matches!(retirement, Retirement::Expiry).then(|| source.expiry_now() + 60_000);
    let id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    source
        .submit_account_group_file_if(
            &account.account_id(),
            id,
            group,
            &path,
            FileKind::Media,
            deadline,
            Some(MessageProtection::Click),
            |_| {},
            |commit| commit(),
        )
        .await
        .unwrap();
    source.maintain_account_group_operation_effects();
    let original = source
        .account_group_operation_status(&account.account_id(), id)
        .unwrap()
        .unwrap();
    assert_eq!(original.phase, AccountGroupOperationPhase::Committed);
    let event_id = original.primary_event.unwrap();
    let live = source
        .operation_projection(group)
        .results
        .into_iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    assert!(
        live.file_descriptor.is_some() && live.message_event.is_some(),
        "live explicit File completion must first expose a real usable result"
    );
    // Exercise the real preflight path used while submit already holds delivery.
    // Release the guard before joining even on failure, so regressions report a
    // bounded timeout rather than leaving a test thread permanently blocked.
    let operation = AccountGroupOperation::React {
        target: event_id,
        emoji: "👍".into(),
        remove: false,
    };
    let next_id = source
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let executor = source
        .account_group_access(group)
        .unwrap()
        .sources
        .into_iter()
        .find(|executor| executor.device == source.identity.public())
        .unwrap();
    let request = operations::wire::RequestProjection {
        reference: operations::journal::OperationReference::new(
            &source, next_id, group, &operation, executor,
        ),
        body: Some(operation),
        cancel: false,
        nonce: [0; 16],
    };
    let (sent, received) = std::sync::mpsc::channel();
    let worker_source = source.clone();
    let held = source.delivery.lock().unwrap();
    let worker = std::thread::spawn(move || {
        sent.send(worker_source.preflight_group_operation_request(request))
            .unwrap()
    });
    let preflight = received.recv_timeout(std::time::Duration::from_secs(2));
    drop(held);
    worker.join().unwrap();
    assert!(
        preflight
            .expect("live File result preflight must not reacquire the already-held delivery lock")
    );
    let malformed_reference = node(&dir.path().join("malformed-reference"), &account);
    link(&malformed_reference, &source);
    for missing_descriptor in [false, true] {
        let mut partial = live.clone();
        if missing_descriptor {
            partial.file_descriptor = None;
        } else {
            partial.message_event = None;
        }
        assert!(
            malformed_reference
                .import_group_operation_result_for_test(partial, &source.own_group_proof())
                .is_err(),
            "retired status-only support must not accept a partially stripped File proof"
        );
    }
    drop(malformed_reference);
    let file =
        crate::node::files::validated_manifest(&live.file_descriptor.as_ref().unwrap().manifest)
            .unwrap()
            .file_conv();
    match retirement {
        Retirement::Expiry => {
            Arc::get_mut(&mut source).unwrap().expiry_clock_start =
                (std::time::Instant::now(), deadline.unwrap() + 1);
        }
        Retirement::Recall => {
            let recall_id = source
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            source
                .submit_account_group_operation(
                    &account.account_id(),
                    recall_id,
                    group,
                    AccountGroupOperation::Recall { target: event_id },
                )
                .unwrap();
            source.maintain_account_group_operation_effects();
            assert_eq!(
                source
                    .account_group_operation_status(&account.account_id(), recall_id)
                    .unwrap()
                    .unwrap()
                    .phase,
                AccountGroupOperationPhase::Committed
            );
            assert!(
                source
                    .recalls
                    .lock()
                    .unwrap()
                    .targets(&group)
                    .contains(&event_id)
            );
        }
        Retirement::Erase => {
            assert!(source.delete_message(group, event_id, false).unwrap() > 0);
            assert!(
                source
                    .received_files
                    .lock()
                    .unwrap()
                    .entry(event_id)
                    .is_none()
            );
        }
    }
    let projection = source.operation_projection(group);
    let retired = projection
        .results
        .iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    assert_eq!(retired.status, original);
    assert!(
        retired.file_descriptor.is_none() && retired.message_event.is_none(),
        "expiry, recall or local erasure must not republish a usable retired File manifest/key"
    );
    let wire = OperationFrame::new(&source, group, 1, projection).encode();
    let frame = OperationFrame::decode(&wire, &account.public().ed25519_pub)
        .expect("status-only terminal receipts must retain their original signed reference");
    let result = frame
        .projection
        .results
        .into_iter()
        .find(|result| result.reference.id == id)
        .unwrap();
    let reference = node(&dir.path().join("fresh-reference"), &account);
    link(&reference, &source);
    assert!(
        !reference
            .import_group_operation_result_for_test(result, &source.own_group_proof())
            .unwrap(),
        "a body-free retired receipt must adopt terminal status without keeping a pending File copy"
    );
    assert_eq!(
        reference
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(
        reference
            .received_files
            .lock()
            .unwrap()
            .entry(event_id)
            .is_none()
    );
    assert!(
        reference
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&event_id)
            .is_none()
    );
    assert!(reference.account_group_data_work(None).work.is_empty());
    assert!(reference.channel_members(group).is_empty());
    assert!(reference.history(group, 20).is_empty());
    assert!(reference.log.lock().unwrap().events(&file).is_empty());
    let identity = reference.identity.secret_bytes();
    drop(reference);
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    let restarted = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &dir.path().join("fresh-reference/events.log"),
        &dir.path().join("fresh-reference/sent.log"),
        "pw",
    )
    .unwrap();
    assert_eq!(
        restarted
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap(),
        original
    );
    assert!(restarted.account_group_data_work(None).work.is_empty());
    assert!(
        restarted
            .received_files
            .lock()
            .unwrap()
            .entry(event_id)
            .is_none()
    );
    assert!(
        restarted
            .account_file_proofs
            .lock()
            .unwrap()
            .get(&event_id)
            .is_none()
    );
}

#[tokio::test]
async fn expired_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Expiry).await;
}
#[tokio::test]
async fn recalled_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Recall).await;
}
#[tokio::test]
async fn erased_file_result_publishes_only_terminal_status_to_fresh_sibling() {
    retired_file_result_is_status_only(Retirement::Erase).await;
}

/// A terminal account receipt remains readable after its physical source leaves.
/// A fresh authenticated wake must not wait behind an older failed Noise dial.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn modern_leave_terminal_wake_recovers_a_real_noise_timeout_within_operation_budget() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};
    let f = crate::node::channels::account_self_control::tests::fixture(false);
    let dir = tempfile::tempdir().unwrap();
    let group = f.group;
    let account = Account::from_secret_bytes(f.account.secret_bytes());
    let source_path = dir.path().join("source");
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (channel, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let source = Node::open_with_account(
        f.member,
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        files,
        &source_path.join("events.log"),
        &source_path.join("sent.log"),
        "pw",
    )
    .unwrap();
    let reference = node(&dir.path().join("reference"), &account);
    for event in &f.events {
        install(&source, event);
    }
    link(&source, &reference);
    link(&reference, &source);
    let source_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reference_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_address = source_listener.local_addr().unwrap();
    let reference_address = reference_listener.local_addr().unwrap();
    for (label, current, other, address, own_address) in [
        (
            "source",
            &source,
            &reference,
            reference_address,
            source_address,
        ),
        (
            "reference",
            &reference,
            &source,
            source_address,
            reference_address,
        ),
    ] {
        for category in [
            SyncCategory::History,
            SyncCategory::Profile,
            SyncCategory::Attachments,
        ] {
            current.set_device_sync_category(category, false).unwrap();
        }
        configure(current, &dir.path().join(label), own_address.port());
        current.roster.lock().unwrap().update(
            &other.signed_announce("Timeout recovery", address.port()),
            address.ip(),
            &current.user_id(),
        );
    }
    let stalled = Arc::new(AtomicBool::new(false));
    let release = Arc::new(tokio::sync::Notify::new());
    // Existing body-free client traces identify the actual Control TCP port;
    // other lanes retain their real endpoint admission and serving behavior.
    let connection_roles = Arc::new(Mutex::new((
        std::collections::HashMap::<tokio::task::Id, bool>::new(),
        std::collections::HashMap::<u16, bool>::new(),
    )));
    let server_roles = connection_roles.clone();
    let serving = source.clone();
    let server_stalled = stalled.clone();
    let server_release = release.clone();
    let source_server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = source_listener.accept() => {
                    let (stream, _) = accepted.unwrap();
                    let serving = serving.clone();
                    let armed = server_stalled.load(Ordering::Acquire);
                    let roles = server_roles.clone();
                    let release = server_release.clone();
                    connections.spawn(Box::pin(async move {
                        let held = if armed {
                            let port = stream.peer_addr().unwrap().port();
                            tokio::time::timeout(Duration::from_secs(1), async {
                                loop {
                                    if let Some(control) = roles.lock().unwrap().1.get(&port).copied() {
                                        break control;
                                    }
                                    tokio::time::sleep(Duration::from_millis(1)).await;
                                }
                            }).await.expect("the real dial must publish its socket classification")
                        } else { false };
                        if held {
                            // A real accepted TCP socket receives no Noise reply.
                            // Releasing it closes this failed attempt; it cannot
                            // manufacture a successful protocol exchange.
                            release.notified().await;
                            drop(stream);
                        } else if let Ok(channel) = serving.privacy_accept(stream).await {
                            serving.serve_connection(channel).await;
                        }
                    }));
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    let reference_server = inbound_server(reference.clone(), reference_listener);
    source.publish_account_group_access_for(group);
    pull_group_control(&reference, &source, source_address, group).await;
    source
        .pull_account_documents_page(
            &source.routing_peer(&reference.user_id()).unwrap(),
            &mut crate::node::account_documents::sync::DocumentSyncCursor::control(),
        )
        .await
        .unwrap();
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Member
    );
    assert!(reference.channel_members(group).is_empty());
    let trace = Arc::new(Mutex::new(Vec::new()));
    let timeout_seen = Arc::new(AtomicBool::new(false));
    let expected = source.identity.public();
    let client_trace = trace.clone();
    let client_timeout = timeout_seen.clone();
    *reference.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |peer, event| {
        if peer == expected {
            let mut roles = connection_roles.lock().unwrap();
            if event.starts_with("negotiation_begin") {
                roles.0.insert(
                    tokio::task::id(),
                    event.contains("domain=Some(GroupControl)"),
                );
            }
            if let Some(port) = event
                .strip_prefix("dial_socket_ports=(Some(")
                .and_then(|ports| ports.split(')').next())
                .and_then(|port| port.parse::<u16>().ok())
            {
                let control = roles.0.get(&tokio::task::id()).copied().unwrap_or(false);
                roles.1.insert(port, control);
            }
            drop(roles);
            if event.starts_with("page_end")
                && event.contains("outcome=noise_timeout")
                && event.contains("domain=GroupControl")
            {
                client_timeout.store(true, Ordering::Release);
            }
            if event.starts_with("page_end")
                || event.starts_with("wake_received")
                || event.starts_with("control_dispatch")
                || event.starts_with("control_timeout_wake_refresh")
                || event.starts_with("dial_noise_end")
            {
                let mut rows = client_trace.lock().unwrap();
                if rows.len() < 128 {
                    rows.push(event);
                }
            }
        }
    }));
    source.sessions.clear();
    reference.sessions.clear();
    stalled.store(true, Ordering::Release);
    let id = reference
        .new_account_group_operation_id(&account.account_id())
        .unwrap();
    let started = Instant::now();
    reference
        .submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::LeaveAccount,
        )
        .unwrap();
    let reference_worker = tokio::spawn(reference.clone().run_account_history_loop());
    let source_worker = tokio::spawn(source.clone().run_account_history_loop());
    let outcome = tokio::time::timeout(Duration::from_secs(8), async {
        while !timeout_seen.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        stalled.store(false, Ordering::Release);
        release.notify_waiters();
        let peer = source.routing_peer(&reference.user_id()).unwrap();
        // This fresh wake traverses Noise and the authenticated Wake handler;
        // status polling itself must not clear a failed cursor's cooldown.
        use crate::node::account_sync_wire::{AccountHistoryFrame, AccountSyncVersion};
        let mut wake = source.begin_account_exchange(&peer, AccountSyncVersion::V2,
            AccountHistoryFrame::Wake { account_id: account.account_id(), epoch: [17; 16], revision: 1 })
            .await.unwrap();
        let ack = wake.recv().await.unwrap();
        assert!(matches!(AccountHistoryFrame::decode_versioned(&ack),
            Some((AccountSyncVersion::V2, AccountHistoryFrame::WakeAck { epoch, revision: 1 })) if epoch == [17; 16]));
        wake.recycle(&source);
        loop {
            let status = reference.list_account_group_operations(&account.account_id(),
                AccountGroupOperationQuery { group: Some(group), pending_only: false, after: None, limit: 64 })
                .unwrap().into_iter().find(|status| status.id == id).unwrap();
            if status.phase == AccountGroupOperationPhase::Committed {
                assert_eq!(status.committed_events.len(), 1);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await;
    let source_phase = source
        .list_account_group_operations(
            &account.account_id(),
            AccountGroupOperationQuery {
                group: Some(group),
                pending_only: false,
                after: None,
                limit: 64,
            },
        )
        .unwrap()
        .into_iter()
        .find(|status| status.id == id)
        .map(|status| status.phase);
    let elapsed = started.elapsed();
    for task in [
        source_server,
        reference_server,
        reference_worker,
        source_worker,
    ] {
        task.abort();
        let _ = task.await;
    }
    release.notify_waiters();
    source.runtime_work.drain().await;
    reference.runtime_work.drain().await;
    assert!(
        timeout_seen.load(Ordering::Acquire),
        "a real Control Noise timeout must precede the recovery wake: {:?}",
        trace.lock().unwrap()
    );
    assert!(
        trace
            .lock()
            .unwrap()
            .iter()
            .any(|row| row.starts_with("wake_received")),
        "a real newer authenticated wake must arrive"
    );
    assert_eq!(source_phase, Some(AccountGroupOperationPhase::Committed));
    assert!(
        outcome.is_ok(),
        "original Leave receipt must recover within 8s after real Noise timeout; elapsed={elapsed:?}, trace={:?}",
        trace.lock().unwrap()
    );
    assert_eq!(
        reference.account_group_access(group).unwrap().role,
        AccountGroupRole::Left
    );
}

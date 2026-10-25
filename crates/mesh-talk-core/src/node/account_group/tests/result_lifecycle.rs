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

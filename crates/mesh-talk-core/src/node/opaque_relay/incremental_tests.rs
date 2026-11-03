use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;

async fn exchange(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
    conversation: ConversationId,
) {
    let keys = relay.secret_bytes();
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        endpoint
            .serve_connection_checked(channel, None)
            .await
            .unwrap();
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    let mailbox = RelayMailbox::federated(&node.identity, peer).unwrap();
    let confirm = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .authored_header(
            &conversation,
            crate::eventlog::Author::from_ed25519(node.identity.public().ed25519_pub),
            1,
        )
        .unwrap()
        .map(|header| header.id);
    let uploaded = node
        .opaque_mailbox_round_confirmed(&mut channel, peer, Some(conversation), mailbox, confirm)
        .await
        .unwrap();
    if let Some(first) = confirm {
        assert!(
            uploaded.iter().any(|(inner, _)| *inner == first),
            "exact custody target must survive bounded recent results, including old targets beyond 64 events"
        );
    }
    drop(channel);
    task.await.unwrap();
}

#[tokio::test]
async fn incremental_mailbox_warm_history_does_not_fetch_old_bodies() {
    for count in [32, 128] {
        let directory = tempfile::tempdir().unwrap();
        let relay_directory = tempfile::tempdir().unwrap();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        let relay = DeviceIdentity::generate();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
            &relay_directory.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let conversation = super::super::conversation::dm_conversation_id(
            &node.identity.public(),
            &remote.public(),
        );
        let mut previous = None;
        for seq in 1..=count {
            let event = Event::new(
                &node.identity,
                conversation,
                seq,
                previous.into_iter().collect(),
                seq,
                0,
                EventKind::Message,
                vec![7; 8192],
            );
            previous = Some(event.id);
            node.log.lock().unwrap().append_durable(event).unwrap();
        }
        loop {
            if node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .prepare_kind_metadata_step(
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )
                .unwrap()
                .ready
            {
                break;
            }
        }
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        assert!(
            !office
                .mailbox_delta_page(&mailbox.id(), None, 64)
                .unwrap()
                .ids
                .is_empty(),
            "fixture must actually upload history"
        );
        let view =
            crate::eventlog::sync::network::NetworkSyncStore::open_view(&office, &mailbox.id())
                .unwrap();
        assert_eq!(
            crate::eventlog::sync::network::NetworkSyncStore::metadata(&office, &view)
                .unwrap()
                .count,
            count,
            "initial exchange must place every historical wrapper in custody"
        );
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            before,
            "unchanged sync must not read or decode historical event bodies; history={count}"
        );
        let tail = Event::new(
            &node.identity,
            conversation,
            count + 1,
            previous.into_iter().collect(),
            count + 1,
            0,
            EventKind::Message,
            vec![8; 8192],
        );
        let tail_outer = mailbox.wrap(&tail).unwrap().id;
        node.log.lock().unwrap().append_durable(tail).unwrap();
        exchange(
            &node,
            office.clone(),
            &relay,
            &remote.public(),
            conversation,
        )
        .await;
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(
            after.0 - before.0 <= 8 && after.1 - before.1 <= 8,
            "one append must fetch only bounded new bodies: before={before:?}, after={after:?}, history={count}"
        );
        assert!(
            crate::eventlog::sync::network::NetworkSyncStore::durable_have(
                &office,
                &mailbox.id(),
                &tail_outer
            )
            .unwrap(),
            "tail must reach durable custody"
        );
        exchange(&node, office, &relay, &remote.public(), conversation).await;
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            after,
            "the next unchanged round must again be bodyless"
        );
    }
}

#[tokio::test]
async fn incremental_mailbox_invalidated_partial_upload_rebuilds_after_cancellation() {
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let remote = DeviceIdentity::generate();
    let proof = crate::discovery::Announce::new_with_account(
        &remote,
        &Account::generate(),
        "Remote",
        47475,
    );
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let relay = DeviceIdentity::generate();
    let office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    for seq in 1..=128 {
        node.log
            .lock()
            .unwrap()
            .append_durable(Event::new(
                &node.identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![7; 32],
            ))
            .unwrap();
    }
    let keys = relay.secret_bytes();
    let endpoint = IndexedRelayEndpoint::new(office.clone(), None, None);
    let (client, server) = tokio::io::duplex(256 * 1024);
    let task = tokio::spawn(async move {
        let identity = DeviceIdentity::from_secret_bytes(keys.0, keys.1);
        let channel = crate::transport::SecureChannel::accept(server, &identity)
            .await
            .unwrap();
        endpoint
            .serve_connection_checked(channel, None)
            .await
            .unwrap();
    });
    let mut channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut progress = OpaqueMailboxProgress::new(
        &relay.public(),
        &remote.public(),
        Some(conversation),
        mailbox,
    );
    // Negotiate, then admit one bounded preparation turn of a larger history.
    for _ in 0..2 {
        assert!(
            !node
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
        );
    }
    assert!(
        office
            .mailbox_delta_page(&progress.mailbox.id(), None, 64)
            .unwrap()
            .ids
            .is_empty()
    );
    node.privacy
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    drop(progress);
    drop(channel);
    task.await.unwrap();
    exchange(
        &node,
        office.clone(),
        &relay,
        &remote.public(),
        conversation,
    )
    .await;
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let page = office.mailbox_delta_page(&mailbox.id(), None, 256).unwrap();
    assert_eq!(
        page.ids.len(),
        128,
        "a stale partial cache must not pin future rounds"
    );
    assert!(page.complete);
}

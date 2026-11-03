use super::*;
use crate::identity::{account::Account, device::DeviceIdentity};
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;

#[tokio::test]
async fn incremental_receive_revocation_preserves_unhandled_page_for_restore() {
    use crate::eventlog::sync::network::NetworkSyncStore;
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
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let mut inner_ids = Vec::new();
    for sequence in 1..=2 {
        let event = Event::new(
            &remote,
            conversation,
            sequence,
            vec![],
            sequence,
            0,
            EventKind::Message,
            vec![sequence as u8; 128],
        );
        inner_ids.push(event.id);
        office.ingest(mailbox.wrap(&event).unwrap()).unwrap();
    }
    assert_eq!(
        office
            .mailbox_delta_page(&mailbox.id(), None, 64)
            .unwrap()
            .ids
            .len(),
        2
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let endpoint = IndexedRelayEndpoint::new(office, None, None);
    let keys = relay.secret_bytes();
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
    let mut progress = OpaqueMailboxProgress::new(&relay.public(), &remote.public(), None, mailbox);
    assert!(
        !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
    );
    assert!(
        !node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
    );
    let received = || {
        inner_ids
            .iter()
            .filter(|id| node.log.lock().unwrap().has(id).unwrap())
            .count()
    };
    assert_eq!(received(), 1, "fixture must pause inside a two-ID page");
    let before = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap();
    assert!(
        before.is_none(),
        "partial pages must not checkpoint their cursor"
    );
    node.set_post_office_permissions(false, false);
    let interrupted = node
        .advance_opaque_mailbox(&mut channel, &mut progress)
        .await;
    assert!(
        matches!(
            interrupted,
            Err(super::super::session::SessionError::Store(
                LogError::AdmissionRejected(_)
            ))
        ),
        "disabled receive must interrupt the page: {interrupted:?}"
    );
    assert_eq!(received(), 1);
    assert_eq!(
        node.opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap(),
        before,
        "an unhandled ID cannot advance the saved cursor"
    );
    node.set_post_office_permissions(false, true);
    let mut done = false;
    for _ in 0..8 {
        if node
            .advance_opaque_mailbox(&mut channel, &mut progress)
            .await
            .unwrap()
        {
            done = true;
            break;
        }
    }
    assert!(done, "restored receive must finish its retained page");
    assert_eq!(
        received(),
        2,
        "the ID interrupted by revocation must be retried"
    );
    drop(channel);
    task.await.unwrap();
}

// Receive-only fixtures intentionally omit history upload: their signed DM
// bodies exercise storage/DAG admission rather than a live ratchet session.
async fn exchange_receive_only(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
    peer: &PublicIdentity,
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
    node.opaque_mailbox_round(&mut channel, peer, None, mailbox)
        .await
        .unwrap();
    drop(channel);
    task.await.unwrap();
}

#[tokio::test]
async fn incremental_receive_restart_preserves_child_pending_and_repairs_missing_inner() {
    use crate::eventlog::sync::network::NetworkSyncStore;
    let directory = tempfile::tempdir().unwrap();
    let relay_directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let remote = DeviceIdentity::generate();
    let proof = crate::discovery::Announce::new_with_account(
        &remote,
        &Account::generate(),
        "Remote",
        47475,
    );
    let (node, _, _) =
        super::super::test_support::node_with_files(directory.path(), identity, account, &proof);
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    let relay = DeviceIdentity::generate();
    let mut office = IndexedPostOffice::open_with_lease(
        ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
        &relay_directory.path().join("relay.log"),
        "pw",
        DeviceIdentity::generate(),
    )
    .unwrap();
    let conversation =
        super::super::conversation::dm_conversation_id(&node.identity.public(), &remote.public());
    let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
    let parent = Event::new(
        &remote,
        conversation,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1; 128],
    );
    let child = Event::new(
        &remote,
        conversation,
        2,
        vec![parent.id],
        2,
        0,
        EventKind::Message,
        vec![2; 128],
    );
    let child_outer = mailbox.wrap(&child).unwrap();
    office.ingest(child_outer.clone()).unwrap();
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(
        !node.log.lock().unwrap().has(&child.id).unwrap(),
        "child must remain pending while its parent is absent"
    );
    let key = bincode::serialize(&(
        relay.public(),
        remote.public(),
        mailbox.id(),
        Option::<ConversationId>::None,
    ))
    .unwrap();
    let receipt = node
        .opaque_receive_cache
        .lock()
        .unwrap()
        .get_checkpoint(&key)
        .unwrap()
        .expect("completed receive page must persist its pending IDs");
    let sealed: (Vec<u8>, Vec<u8>) = bincode::deserialize(&receipt).unwrap();
    let saved: (
        Option<crate::postoffice::MailboxCursor>,
        Vec<EventId>,
        i64,
        i64,
        [u8; 16],
        bool,
    ) = bincode::deserialize(&sealed.0).unwrap();
    assert!(saved.0.is_some());
    assert_eq!(
        saved.1,
        vec![child_outer.id],
        "restart recovery requires the exact deferred wrapper ID"
    );
    assert!(
        !saved.5,
        "a missing-parent deferral is durable rather than session-only progress"
    );
    drop(node);
    office.ingest(mailbox.wrap(&parent).unwrap()).unwrap();
    let (node, _, _) = super::super::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &proof,
    );
    node.cached_peer_snapshot_async().await.unwrap();
    node.set_post_office_permissions(false, true);
    exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
    assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
    assert!(
        node.log.lock().unwrap().has(&child.id).unwrap(),
        "the saved cursor excludes the old child; its durable pending ID must retry it after the new parent arrives"
    );
    node.log
        .lock()
        .unwrap()
        .drop_conversation(&conversation)
        .unwrap();
    assert!(!node.log.lock().unwrap().has(&child.id).unwrap());
    exchange_receive_only(&node, office, &relay, &remote.public()).await;
    assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
    assert!(
        node.log.lock().unwrap().has(&child.id).unwrap(),
        "retained wrapper hints cannot suppress recovery of genuinely missing inner events"
    );
}

#[tokio::test]
async fn incremental_receive_rejects_corrupt_or_unsigned_modified_checkpoint_after_restart() {
    use crate::eventlog::sync::network::NetworkSyncStore;
    for tampered_body in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let relay_directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let account = Account::generate();
        let account_key = account.secret_bytes();
        let remote = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new_with_account(
            &remote,
            &Account::generate(),
            "Remote",
            47475,
        );
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            identity,
            account,
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(false, true);
        let relay = DeviceIdentity::generate();
        let mut office = IndexedPostOffice::open_with_lease(
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
        let mailbox = RelayMailbox::federated(&node.identity, &remote.public()).unwrap();
        let parent = Event::new(
            &remote,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1; 128],
        );
        let child = Event::new(
            &remote,
            conversation,
            2,
            vec![parent.id],
            2,
            0,
            EventKind::Message,
            vec![2; 128],
        );
        let child_outer = mailbox.wrap(&child).unwrap();
        office.ingest(child_outer.clone()).unwrap();
        exchange_receive_only(&node, office.clone(), &relay, &remote.public()).await;
        assert!(!node.log.lock().unwrap().has(&child.id).unwrap());
        let key = bincode::serialize(&(
            relay.public(),
            remote.public(),
            mailbox.id(),
            Option::<ConversationId>::None,
        ))
        .unwrap();
        let original = node
            .opaque_receive_cache
            .lock()
            .unwrap()
            .get_checkpoint(&key)
            .unwrap()
            .expect("fixture must persist a signed pending checkpoint");
        let invalid = if tampered_body {
            // Preserve a structurally valid completed cursor and all local owner
            // bindings, but erase its pending child without signing the edit.
            // Accepting this blob would fetch only the new parent and lose child.
            let mut sealed: (Vec<u8>, Vec<u8>) = bincode::deserialize(&original).unwrap();
            let mut receipt: (
                Option<crate::postoffice::MailboxCursor>,
                Vec<EventId>,
                i64,
                i64,
                [u8; 16],
                bool,
            ) = bincode::deserialize(&sealed.0).unwrap();
            assert!(receipt.0.is_some());
            assert_eq!(receipt.1, vec![child_outer.id]);
            assert!(!receipt.5);
            receipt.1.clear();
            sealed.0 = bincode::serialize(&receipt).unwrap();
            bincode::serialize(&sealed).unwrap()
        } else {
            b"corrupt optional checkpoint".to_vec()
        };
        assert_ne!(invalid, original);
        assert!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .put_checkpoint(&key, &invalid)
                .unwrap()
        );
        drop(node);
        office.ingest(mailbox.wrap(&parent).unwrap()).unwrap();
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(account_key),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        node.set_post_office_permissions(false, true);
        exchange_receive_only(&node, office, &relay, &remote.public()).await;
        assert!(node.log.lock().unwrap().has(&parent.id).unwrap());
        assert!(
            node.log.lock().unwrap().has(&child.id).unwrap(),
            "invalid optional checkpoint must fall back to the retained mailbox: tampered_body={tampered_body}"
        );
        assert_ne!(
            node.opaque_receive_cache
                .lock()
                .unwrap()
                .get_checkpoint(&key)
                .unwrap()
                .unwrap(),
            invalid,
            "successful reconciliation must replace the rejected optional hint"
        );
    }
}

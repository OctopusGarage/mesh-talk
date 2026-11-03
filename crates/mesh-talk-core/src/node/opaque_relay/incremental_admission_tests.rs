use super::*;
use crate::discovery::{Announce, DiscoveryVisibility};
use crate::eventlog::sync::network::NetworkSyncStore;
use crate::identity::device::DeviceIdentity;
use crate::postoffice::{IndexedPostOffice, federation::IndexedRelayEndpoint};
use crate::storage::profile::ProfileStorageLease;
use std::sync::Arc;

async fn admission_connection(
    node: &Node,
    office: IndexedPostOffice,
    relay: &DeviceIdentity,
) -> (
    crate::transport::SecureChannel<tokio::io::DuplexStream>,
    tokio::task::JoinHandle<()>,
) {
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
    let channel =
        crate::transport::SecureChannel::connect(client, &node.identity, Some(&relay.public()))
            .await
            .unwrap();
    (channel, task)
}

#[tokio::test]
async fn incremental_late_invitation_keeps_uninvited_ancestry_bounded_to_its_operation() {
    for cancel_before_invitation in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let alice = privacy_node(&directory.path().join("alice"));
        let relay = privacy_node(&directory.path().join("relay"));
        let carol = privacy_node(&directory.path().join("carol"));
        carol
            .configure_privacy(
                &directory.path().join("carol"),
                "pw",
                &carol.signed_announce("Carol", 1234),
                Arc::new(DiscoveryVisibility::new(true)),
            )
            .unwrap();
        let relay_proof =
            Announce::new_post_office_with_account(&relay.identity, &relay.account, "Relay", 1234);
        for (peer, proof) in [
            (&alice, alice.signed_announce("Alice", 1234)),
            (&relay, relay_proof),
        ] {
            carol.roster.lock().unwrap().update(
                &proof,
                "127.0.0.1".parse().unwrap(),
                &carol.user_id(),
            );
            carol.set_allowed(&peer.account_id(), true).await.unwrap();
        }
        carol.set_invisible(true).await.unwrap();
        carol.set_post_office_permissions(false, true);
        let group = alice.create_channel_invited("Team", vec![]).await.unwrap();
        let uninvited = alice.log.lock().unwrap().events(&group).unwrap();
        assert!(!uninvited.is_empty());
        alice
            .add_channel_member(group, carol.identity.public())
            .await
            .unwrap();
        let complete = alice.log.lock().unwrap().events(&group).unwrap();
        let earlier: HashSet<_> = uninvited.iter().map(|event| event.id).collect();
        let invitation: Vec<_> = complete
            .into_iter()
            .filter(|event| !earlier.contains(&event.id))
            .collect();
        assert!(!invitation.is_empty());
        let relay_directory = tempfile::tempdir().unwrap();
        let mut office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(relay_directory.path()).unwrap(),
            &relay_directory.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let mailbox = RelayMailbox::federated(&carol.identity, &alice.identity.public()).unwrap();
        let mut staged_outer = Vec::new();
        // The real relay's append sequence fixes ancestry before the grant.
        for event in &uninvited {
            let outer = mailbox.wrap(event).unwrap();
            staged_outer.push(outer.id);
            office.ingest(outer).unwrap();
        }
        for event in &invitation {
            office.ingest(mailbox.wrap(event).unwrap()).unwrap();
        }
        let (mut channel, mut task) =
            admission_connection(&carol, office.clone(), &relay.identity).await;
        let mut progress = OpaqueMailboxProgress::new(
            &relay.identity.public(),
            &alice.identity.public(),
            None,
            mailbox,
        );
        assert!(
            !carol
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
        );
        for _ in &uninvited {
            assert!(
                !carol
                    .advance_opaque_mailbox(&mut channel, &mut progress)
                    .await
                    .unwrap()
            );
        }
        assert!(
            carol.log.lock().unwrap().events(&group).unwrap().is_empty(),
            "uninvited ancestry must remain off disk"
        );
        assert!(
            staged_outer
                .iter()
                .all(|id| progress.deferred_events.contains(id)),
            "in-memory staging must retain retry evidence rather than processed evidence"
        );
        assert!(
            carol
                .opaque_receive_cache
                .lock()
                .unwrap()
                .seen_selected(
                    progress.mailbox.id(),
                    &staged_outer,
                    &carol.log.lock().unwrap()
                )
                .unwrap()
                .is_empty(),
            "canceling staged ancestry must not leave node-wide session hints that suppress replay"
        );
        if !cancel_before_invitation {
            // Force a real admission Result error after the staging transfer.
            // Retrying the already-staged genesis needs another deferred slot.
            let deferred = std::mem::take(&mut progress.deferred_events);
            for number in 0..MAX_DEFERRED_EVENTS as u64 {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&number.to_be_bytes());
                progress.deferred_events.insert(EventId::new(bytes));
            }
            let outcome = carol.accept_opaque_wrappers(
                &mut progress,
                vec![
                    RelayMailbox::federated(&carol.identity, &alice.identity.public())
                        .unwrap()
                        .wrap(&uninvited[0])
                        .unwrap(),
                ],
            );
            assert!(matches!(
                outcome,
                Err(super::super::session::SessionError::NoProgress)
            ));
            progress.deferred_events = deferred;
        }
        if cancel_before_invitation {
            drop(progress);
            drop(channel);
            task.await.unwrap();
            (channel, task) = admission_connection(&carol, office.clone(), &relay.identity).await;
            progress = OpaqueMailboxProgress::new(
                &relay.identity.public(),
                &alice.identity.public(),
                None,
                RelayMailbox::federated(&carol.identity, &alice.identity.public()).unwrap(),
            );
        }
        let mut done = false;
        for _ in 0..(uninvited.len() + invitation.len()) * 3 + 16 {
            if carol
                .advance_opaque_mailbox(&mut channel, &mut progress)
                .await
                .unwrap()
            {
                done = true;
                break;
            }
        }
        assert!(
            done,
            "late invitation must finish: canceled={cancel_before_invitation}"
        );
        drop(progress);
        drop(channel);
        task.await.unwrap();
        crate::node::test_support::settle_group_metadata(&carol, group);
        assert!(
            carol
                .list_channels()
                .unwrap()
                .iter()
                .any(|summary| summary.id == group && summary.pending_invite),
            "a late grant must admit its staged ancestors: canceled={cancel_before_invitation}"
        );
    }
}

fn privacy_node(path: &std::path::Path) -> Arc<Node> {
    std::fs::create_dir_all(path).unwrap();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (groups, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        dm,
        groups,
        files,
        &path.join("messages.log"),
        &path.join("sent.log"),
        "pw",
    )
    .unwrap()
}

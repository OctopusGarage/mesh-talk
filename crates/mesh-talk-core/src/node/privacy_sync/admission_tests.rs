use super::*;
use crate::discovery::Announce;
use crate::identity::{account::Account, device::DeviceIdentity};

#[tokio::test]
async fn message_scope_admission_does_not_read_historical_bodies() {
    for history in [32, 128] {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let (node, _, _) = super::super::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        let direct =
            super::super::conversation::dm_conversation_id(&node.identity.public(), &peer.public());
        let channel = ConversationId::new([121; 32]);
        let unknown = ConversationId::new([122; 32]);
        let genesis = Event::new(
            &peer,
            channel,
            1,
            vec![],
            1,
            0,
            EventKind::MembershipChange,
            crate::channel::ChannelMeta {
                name: "Retained channel".into(),
                members: vec![peer.public(), node.identity.public()],
                epoch: 0,
                owner: peer.user_id(),
            }
            .encode(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(genesis.clone())
            .unwrap();
        assert!(
            node.channels.lock().unwrap().state(&channel).is_none(),
            "exercise indexed retained-kind fallback rather than the installed channel book"
        );
        for conversation in [direct, channel, unknown] {
            let mut parent = (conversation == channel).then_some(genesis.id);
            for sequence in 2..=history + 1 {
                let event = Event::new(
                    &peer,
                    conversation,
                    sequence,
                    parent.into_iter().collect(),
                    sequence,
                    0,
                    EventKind::Message,
                    vec![7; 8192],
                );
                parent = Some(event.id);
                node.log.lock().unwrap().append_durable(event).unwrap();
            }
            let tail = Event::new(
                &peer,
                conversation,
                history + 2,
                parent.into_iter().collect(),
                history + 2,
                0,
                EventKind::Message,
                vec![8; 128],
            );
            let tail_id = tail.id;
            let before = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts();
            let outcome = node.sync_store(&peer.public()).lock().unwrap().ingest(tail);
            if conversation == unknown {
                assert!(
                    matches!(outcome, Err(LogError::AdmissionRejected(_))),
                    "history alone must not authorize an unknown attachment scope"
                );
                assert!(!node.log.lock().unwrap().has(&tail_id).unwrap());
            } else {
                assert_eq!(outcome.unwrap(), AppendOutcome::Appended);
                assert!(node.log.lock().unwrap().has(&tail_id).unwrap());
            }
            let after = node
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts();
            let scope = if conversation == direct {
                "direct"
            } else if conversation == channel {
                "channel"
            } else {
                "unknown"
            };
            assert_eq!(
                after, before,
                "message scope classification must use identity and retained metadata, without decrypting old bodies: history={history}, scope={scope}"
            );
        }
    }
}

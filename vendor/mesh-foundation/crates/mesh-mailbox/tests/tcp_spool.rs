use mesh_connect::PeerSession;
use mesh_identity::device::DeviceIdentity;
use mesh_mailbox::{
    RelayMailbox,
    capacity::CapacityLimits,
    rpc::{
        Frame, PAGE_IDS, encode_frame, make_get_request, read_delta_response, read_get_response,
    },
    service::dispatch,
    spool::{Deposit, Spool},
};
use mesh_replication::legacy_event::{ConversationId, Event, EventKind};
use mesh_storage::EncryptionKey;
use std::{num::NonZeroU64, time::Duration};
use tokio::net::{TcpListener, TcpStream};

#[tokio::test]
async fn mailbox_reuses_an_authenticated_stream_and_resumes_after_a_lost_reply() {
    let directory = tempfile::tempdir().unwrap();
    let sender = DeviceIdentity::generate();
    let recipient = DeviceIdentity::generate();
    let relay = DeviceIdentity::generate();
    let outgoing = RelayMailbox::federated(&sender, &recipient.public()).unwrap();
    let incoming = RelayMailbox::federated(&recipient, &sender.public()).unwrap();
    let scope = outgoing.id();
    let mut spool = Spool::open_with_key_provider(
        &directory.path().join("spool"),
        scope,
        CapacityLimits::new(100, 0, 128 * 1024, 0).unwrap(),
        NonZeroU64::new(60).unwrap(),
        |_| Ok(EncryptionKey::from_derived_key([7; 32])),
    )
    .unwrap();
    let count = PAGE_IDS + 6;
    for seq in 1..=count as u64 {
        let inner = Event::new(
            &sender,
            ConversationId::new([3; 32]),
            seq,
            vec![],
            seq,
            0,
            EventKind::Message,
            vec![seq as u8],
        );
        assert_eq!(
            spool.deposit(outgoing.wrap(&inner).unwrap(), 1).unwrap(),
            Deposit::Stored
        );
    }
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let recipient_public = recipient.public().x25519_pub;
    let recipient_secret = recipient.secret_bytes().1;
    let relay_public = relay.public().x25519_pub;
    let relay_secret = relay.secret_bytes().1;

    tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(
            async {
                let mut prior = None;
                let mut delivered = Vec::new();
                for attempt in 0..2 {
                    let stream = TcpStream::connect(address).await.unwrap();
                    let mut session =
                        PeerSession::connect(stream, &recipient_secret, &relay_public)
                            .await
                            .unwrap();
                    let delta = |prior| {
                        encode_frame(&Frame::Delta {
                            conversation: scope,
                            prior,
                        })
                        .unwrap()
                    };
                    session.send(&delta(prior.clone())).await.unwrap();
                    let page = read_delta_response(&session.recv().await.unwrap(), scope).unwrap();
                    assert_eq!(page.complete, attempt == 1);
                    assert!(!page.reset);
                    assert_eq!(page.ids.len(), if attempt == 0 { PAGE_IDS } else { 6 });

                    session
                        .send(&make_get_request(scope, &page.cursor, &page.ids).unwrap())
                        .await
                        .unwrap();
                    let events = read_get_response(
                        &session.recv().await.unwrap(),
                        scope,
                        &page.ids,
                        |event| bincode::serialized_size(event).unwrap() as usize,
                    )
                    .unwrap();
                    delivered.extend(events.iter().map(|event| incoming.open(event).unwrap().seq));
                    prior = Some(page.cursor);

                    session.send(&delta(prior.clone())).await.unwrap();
                    if attempt == 0 {
                        assert!(session.recv().await.is_err());
                    } else {
                        let idle =
                            read_delta_response(&session.recv().await.unwrap(), scope).unwrap();
                        assert!(idle.complete && !idle.reset && idle.ids.is_empty());
                        assert_eq!(Some(idle.cursor), prior);
                    }
                }
                assert_eq!(delivered, (1..=count as u64).collect::<Vec<_>>());
            },
            async {
                for attempt in 0..2 {
                    let (stream, _) = listener.accept().await.unwrap();
                    let mut session =
                        PeerSession::accept(stream, &relay_secret, |key| *key == recipient_public)
                            .await
                            .unwrap();
                    for request_index in 0..3 {
                        let request = session.recv().await.unwrap();
                        if attempt == 0 && request_index == 2 {
                            break;
                        }
                        let reply = dispatch(&spool.view(1), Some(scope), &request).unwrap();
                        session.send(&reply.bytes).await.unwrap();
                    }
                }
            }
        );
    })
    .await
    .unwrap();
}

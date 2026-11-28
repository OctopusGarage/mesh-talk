use mesh_talk_protocol::mailbox::{
    client::{MailboxTurnLimits, sync_turn},
    rpc::{self, Frame, MailboxCursor, MailboxDeltaPage},
};
use mesh_talk_protocol::{legacy_event::ConversationId, session::ReconciliationSession};
use std::collections::VecDeque;

struct Session {
    received: VecDeque<Vec<u8>>,
    sends: usize,
}
impl ReconciliationSession for Session {
    type Peer = ();
    type Error = &'static str;
    fn peer_identity(&self) -> &() {
        &()
    }
    async fn send(&mut self, _: &[u8]) -> Result<(), Self::Error> {
        self.sends += 1;
        Ok(())
    }
    async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.received.pop_front().ok_or("closed")
    }
}
fn cursor(scope: ConversationId) -> MailboxCursor {
    MailboxCursor {
        store_uuid: [1; 16],
        conversation: scope,
        epoch: 0,
        first_seen: None,
        revision: 1,
        after: 0,
        window: None,
    }
}
#[tokio::test]
async fn empty_page_checkpoint_is_committed_before_returning_complete() {
    let scope = ConversationId::new([1; 32]);
    let mut c = cursor(scope);
    c.after = 5;
    let frame = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
        cursor: c.clone(),
        ids: vec![],
        complete: true,
        reset: false,
    }))
    .unwrap();
    let mut session = Session {
        received: VecDeque::from([frame]),
        sends: 0,
    };
    let mut saved = None;
    let progress = sync_turn(
        &mut session,
        scope,
        None,
        MailboxTurnLimits::default(),
        |events, checkpoint| {
            assert!(events.is_empty());
            saved = Some(checkpoint.clone());
            Ok::<_, &'static str>(())
        },
    )
    .await
    .unwrap();
    assert!(progress.work.complete);
    assert_eq!(saved, Some(c));
    assert_eq!(session.sends, 1);
}
#[tokio::test]
async fn failed_commit_retains_prior_checkpoint() {
    let scope = ConversationId::new([1; 32]);
    let prior = cursor(scope);
    let mut next = prior.clone();
    next.after = 5;
    let frame = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
        cursor: next,
        ids: vec![],
        complete: true,
        reset: false,
    }))
    .unwrap();
    let mut session = Session {
        received: VecDeque::from([frame]),
        sends: 0,
    };
    let error = sync_turn(
        &mut session,
        scope,
        Some(prior.clone()),
        MailboxTurnLimits::default(),
        |_, _| Err::<(), _>("disk full"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.progress.cursor, Some(prior));
    assert!(!error.progress.work.complete);
}

fn messages(scope: ConversationId, count: usize) -> Vec<mesh_talk_protocol::legacy_event::Event> {
    use mesh_talk_protocol::device::DeviceIdentity;
    use mesh_talk_protocol::legacy_event::{Event, EventKind};
    let identity = DeviceIdentity::generate();
    (0..count)
        .map(|i| {
            Event::new(
                &identity,
                scope,
                i as u64,
                vec![],
                i as u64,
                0,
                EventKind::Message,
                vec![1],
            )
        })
        .collect()
}
fn pages(
    scope: ConversationId,
    events: &[mesh_talk_protocol::legacy_event::Event],
    offset: usize,
) -> Session {
    let mut received = VecDeque::new();
    for (n, batch) in events.chunks(64).enumerate() {
        let after = offset + (n * 64) + batch.len();
        let mut c = cursor(scope);
        c.after = after as i64;
        let complete = after == 201;
        if !complete {
            c.window = Some((201, 1));
        }
        received.push_back(
            rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
                cursor: c,
                ids: batch.iter().map(|e| e.id).collect(),
                complete,
                reset: false,
            }))
            .unwrap(),
        );
        received.push_back(
            rpc::encode_frame(&Frame::Events {
                conversation: scope,
                events: batch.to_vec(),
            })
            .unwrap(),
        );
    }
    Session { received, sends: 0 }
}
#[tokio::test]
async fn full_pages_never_turn_a_200_record_budget_into_256_or_skip_unacknowledged_ids() {
    let scope = ConversationId::new([2; 32]);
    let events = messages(scope, 201);
    let mut first = pages(scope, &events, 0);
    let mut saved = Vec::new();
    let p = sync_turn(
        &mut first,
        scope,
        None,
        MailboxTurnLimits::default(),
        |batch, _| {
            saved.extend(batch.iter().map(|e| e.id));
            Ok::<_, &'static str>(())
        },
    )
    .await
    .unwrap();
    assert_eq!(p.work.committed, 192);
    assert!(!p.work.complete);
    assert_eq!(first.sends, 7);
    assert_eq!(p.cursor.as_ref().unwrap().after, 192);
    let mut second = pages(scope, &events[192..], 192);
    let p = sync_turn(
        &mut second,
        scope,
        p.cursor,
        MailboxTurnLimits::default(),
        |batch, _| {
            saved.extend(batch.iter().map(|e| e.id));
            Ok::<_, &'static str>(())
        },
    )
    .await
    .unwrap();
    assert!(p.work.complete);
    assert_eq!(saved, events.iter().map(|e| e.id).collect::<Vec<_>>());
}
#[tokio::test]
async fn nonadvancing_partial_page_fails_without_acknowledgement() {
    let scope = ConversationId::new([3; 32]);
    let mut c = cursor(scope);
    c.window = Some((100, 1));
    let bytes = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
        cursor: c.clone(),
        ids: vec![],
        complete: false,
        reset: false,
    }))
    .unwrap();
    let mut session = Session {
        received: VecDeque::from([bytes]),
        sends: 0,
    };
    let result = sync_turn(
        &mut session,
        scope,
        Some(c),
        MailboxTurnLimits::default(),
        |_, _| Ok::<_, &'static str>(()),
    )
    .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn reset_is_explicit_and_does_not_acknowledge_a_new_checkpoint() {
    let scope = ConversationId::new([4; 32]);
    let prior = cursor(scope);
    let mut next = prior.clone();
    next.epoch = 1;
    let bytes = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
        cursor: next,
        ids: vec![],
        complete: true,
        reset: true,
    }))
    .unwrap();
    let mut session = Session {
        received: VecDeque::from([bytes]),
        sends: 0,
    };
    let result = sync_turn(
        &mut session,
        scope,
        Some(prior.clone()),
        MailboxTurnLimits::default(),
        |_, _| panic!("reset must not commit"),
    )
    .await;
    let error: mesh_talk_protocol::mailbox::client::MailboxTurnError<&str, ()> =
        result.unwrap_err();
    assert!(matches!(
        error.kind,
        mesh_talk_protocol::mailbox::client::MailboxTurnErrorKind::CheckpointInvalidated
    ));
    assert_eq!(error.progress.cursor, Some(prior));
}

#[tokio::test]
async fn partial_snapshot_cannot_move_its_upper_watermark_or_finish_early() {
    let scope = ConversationId::new([5; 32]);
    let mut prior = cursor(scope);
    prior.after = 64;
    prior.window = Some((201, 1));
    for (after, window, complete) in [(128, Some((300, 2)), false), (128, None, true)] {
        let mut next = prior.clone();
        next.after = after;
        next.window = window;
        let bytes = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
            cursor: next,
            ids: vec![],
            complete,
            reset: false,
        }))
        .unwrap();
        let mut session = Session {
            received: VecDeque::from([bytes]),
            sends: 0,
        };
        let error = sync_turn(
            &mut session,
            scope,
            Some(prior.clone()),
            MailboxTurnLimits::default(),
            |_, _| Ok::<_, ()>(()),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error.kind,
            mesh_talk_protocol::mailbox::client::MailboxTurnErrorKind::Wire(_)
        ));
        assert_eq!(error.progress.cursor, Some(prior.clone()));
    }
}

#[tokio::test]
async fn first_empty_nonadvancing_page_never_commits() {
    let scope = ConversationId::new([1; 32]);
    let mut checkpoint = cursor(scope);
    checkpoint.window = Some((10, 1));
    let frame = rpc::encode_frame(&Frame::Page(MailboxDeltaPage {
        cursor: checkpoint,
        ids: vec![],
        complete: false,
        reset: false,
    }))
    .unwrap();
    let mut session = Session {
        received: VecDeque::from([frame]),
        sends: 0,
    };
    let mut commits = 0;
    let result = sync_turn(
        &mut session,
        scope,
        None,
        MailboxTurnLimits::default(),
        |_, _| {
            commits += 1;
            Ok::<_, ()>(())
        },
    )
    .await;
    assert!(result.is_err());
    assert_eq!(commits, 0);
}

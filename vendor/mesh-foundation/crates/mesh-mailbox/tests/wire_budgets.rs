#![cfg(all(feature = "wire", feature = "spool"))]
use mesh_mailbox::{
    Record, ScopeId,
    capacity::CapacityLimits,
    client::MailboxTurnLimits,
    model::{MailboxCursor, MailboxDeltaPage},
    ports::MailboxStore,
    rpc::{self, Frame},
    service,
    spool::{Retention, Spool, SpoolOptions},
    wire,
};
use mesh_replication::{pull::PullTurnLimits, session::ReconciliationSession};
use mesh_storage::EncryptionKey;
use std::collections::VecDeque;
struct Session<'a> {
    spool: &'a Spool,
    scope: ScopeId,
    replies: VecDeque<Vec<u8>>,
    requests: usize,
}
impl ReconciliationSession for Session<'_> {
    type Peer = ();
    type Error = &'static str;
    fn peer_identity(&self) -> &() {
        &()
    }
    async fn send(&mut self, bytes: &[u8]) -> Result<(), Self::Error> {
        match rpc::decode_frame(bytes).unwrap() {
            Frame::Delta {
                limit, max_bytes, ..
            } => {
                assert_eq!(limit, 1);
                assert_eq!(max_bytes, 1024);
            }
            Frame::Get { max_bytes, .. } => assert_eq!(max_bytes, 1024),
            _ => panic!("request"),
        }
        let reply = service::dispatch(&self.spool.view(1), Some(self.scope), bytes)
            .map_err(|_| "dispatch")?;
        self.requests += 1;
        self.replies.push_back(reply.bytes);
        Ok(())
    }
    async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.replies.pop_front().ok_or("closed")
    }
}
#[tokio::test]
async fn wire_profile_carries_small_record_and_byte_allowances() {
    let dir = tempfile::tempdir().unwrap();
    let scope = ScopeId::new([1; 32]);
    let mut spool = Spool::open_configured(
        &dir.path().join("s"),
        scope,
        SpoolOptions {
            capacity: CapacityLimits::new(10, 0, 4096, 0).unwrap(),
            retention: Retention::Manual,
            max_record_bytes: 1024,
            max_query_records: 10,
        },
        |_| Ok(EncryptionKey::from_derived_key([1; 32])),
    )
    .unwrap();
    let record = Record::new(scope, b"small".to_vec());
    spool.deposit(record.clone(), 1).unwrap();
    let mut session = Session {
        spool: &spool,
        scope,
        replies: VecDeque::new(),
        requests: 0,
    };
    let mut saved = vec![];
    let limits = MailboxTurnLimits {
        work: PullTurnLimits {
            records: 1,
            bytes: 1024,
            requests: 2,
        },
        pages: 1,
        page_records: 1,
    };
    let result = wire::sync_turn(&mut session, scope, None, limits, |records, _| {
        saved.extend_from_slice(records);
        Ok::<_, ()>(())
    })
    .await
    .unwrap();
    assert!(result.work.complete);
    assert_eq!(result.work.bytes, 5);
    assert_eq!(saved, [record]);
    assert_eq!(session.requests, 2);
    assert!(rpc::decode_frame(b"MFOM1").is_err());
}
struct Forbidden;
impl MailboxStore for Forbidden {
    type Error = ();
    fn delta_bounded(
        &self,
        _: &ScopeId,
        _: Option<&MailboxCursor>,
        _: usize,
        _: usize,
    ) -> Result<MailboxDeltaPage, ()> {
        panic!("invalid scope/budgets must precede store")
    }
    fn bodies(
        &self,
        _: &ScopeId,
        _: &MailboxCursor,
        _: &[mesh_mailbox::RecordId],
        _: usize,
    ) -> Result<Vec<Record>, ()> {
        panic!("invalid request")
    }
}
#[test]
fn negotiated_scope_and_wire_budgets_precede_store_access() {
    let scope = ScopeId::new([1; 32]);
    for (auth, limit, bytes) in [
        (None, 1, 1024),
        (Some(scope), 0, 1024),
        (Some(scope), 65, 1024),
        (Some(scope), 1, 0),
        (Some(scope), 1, u32::MAX),
    ] {
        let request = rpc::encode_frame(&Frame::Delta {
            scope,
            prior: None,
            limit,
            max_bytes: bytes,
        })
        .unwrap();
        assert!(service::dispatch(&Forbidden, auth, &request).is_err());
    }
}

struct OvershootingStore;
impl MailboxStore for OvershootingStore {
    type Error = ();
    fn delta_bounded(
        &self,
        scope: &ScopeId,
        _: Option<&MailboxCursor>,
        _: usize,
        _: usize,
    ) -> Result<MailboxDeltaPage, ()> {
        Ok(MailboxDeltaPage {
            cursor: MailboxCursor {
                store_uuid: [1; 16],
                scope: *scope,
                epoch: 0,
                revision: 2,
                after: 2,
                window: None,
            },
            ids: vec![
                Record::new(*scope, vec![1]).id,
                Record::new(*scope, vec![2]).id,
            ],
            complete: true,
            reset: false,
        })
    }
    fn bodies(
        &self,
        scope: &ScopeId,
        _: &MailboxCursor,
        _: &[mesh_mailbox::RecordId],
        _: usize,
    ) -> Result<Vec<Record>, ()> {
        Ok(vec![Record::new(*scope, vec![1; 1024])])
    }
}
#[test]
fn backend_results_are_rechecked_before_any_response_is_emitted() {
    let scope = ScopeId::new([1; 32]);
    let delta = rpc::encode_frame(&Frame::Delta {
        scope,
        prior: None,
        limit: 1,
        max_bytes: 1024,
    })
    .unwrap();
    assert!(service::dispatch(&OvershootingStore, Some(scope), &delta).is_err());
    let cursor = MailboxCursor {
        store_uuid: [1; 16],
        scope,
        epoch: 0,
        revision: 1,
        after: 1,
        window: None,
    };
    let record = Record::new(scope, vec![1; 1024]);
    let get = rpc::make_get_request_bounded(scope, &cursor, &[record.id], 1).unwrap();
    assert!(service::dispatch(&OvershootingStore, Some(scope), &get).is_err());
}

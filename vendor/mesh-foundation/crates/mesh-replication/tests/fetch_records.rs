use mesh_replication::{
    pull::{PullLimits, RecordExchangeError, fetch_records},
    session::ReconciliationSession,
};

#[derive(Default)]
struct Session {
    response: Vec<u8>,
    sent: Vec<Vec<u8>>,
}
impl ReconciliationSession for Session {
    type Peer = ();
    type Error = ();
    fn peer_identity(&self) -> &() {
        &()
    }
    async fn send(&mut self, bytes: &[u8]) -> Result<(), ()> {
        self.sent.push(bytes.to_vec());
        Ok(())
    }
    async fn recv(&mut self) -> Result<Vec<u8>, ()> {
        Ok(self.response.clone())
    }
}
fn response(ids: &[u8]) -> Vec<u8> {
    let mut bytes = b"MRS1\0".to_vec();
    bytes.extend_from_slice(&[7; 32]);
    for id in ids {
        bytes.extend_from_slice(&[*id; 32]);
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(*id);
    }
    bytes
}
fn limits() -> PullLimits {
    PullLimits {
        records_per_batch: 2,
        bytes_per_batch: 2,
    }
}
#[tokio::test]
async fn fetches_selected_bodies_with_exact_request() {
    let mut session = Session {
        response: response(&[2, 1]),
        ..Default::default()
    };
    let records = fetch_records(&mut session, [7; 32], &[[1; 32], [2; 32]], limits())
        .await
        .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].body, vec![2]);
    let mut expected = b"MRQ1\0".to_vec();
    expected.extend_from_slice(&[7; 32]);
    expected.extend_from_slice(&2u32.to_be_bytes());
    expected.extend_from_slice(&[1; 32]);
    expected.extend_from_slice(&[2; 32]);
    assert_eq!(session.sent, vec![expected]);
}
#[tokio::test]
async fn rejects_mismatched_and_oversized_responses() {
    for ids in [&[1, 1][..], &[1, 3], &[1], &[1, 2, 3]] {
        let mut session = Session {
            response: response(ids),
            ..Default::default()
        };
        assert_eq!(
            fetch_records(&mut session, [7; 32], &[[1; 32], [2; 32]], limits()).await,
            Err(RecordExchangeError::InvalidFrame)
        );
    }
}
#[tokio::test]
async fn validates_selection_and_limits_without_io() {
    let mut session = Session::default();
    for (ids, limits) in [
        (vec![[1; 32], [1; 32]], limits()),
        (vec![[1; 32]; 3], limits()),
        (
            vec![[1; 32]],
            PullLimits {
                records_per_batch: usize::MAX,
                bytes_per_batch: 1,
            },
        ),
        (
            vec![[1; 32]],
            PullLimits {
                records_per_batch: 1,
                bytes_per_batch: usize::MAX,
            },
        ),
    ] {
        assert_eq!(
            fetch_records(&mut session, [7; 32], &ids, limits).await,
            Err(RecordExchangeError::InvalidFrame)
        );
    }
    assert!(
        fetch_records(&mut session, [7; 32], &[], limits())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(session.sent.is_empty());
}

#[tokio::test]
async fn existing_session_pull_preserves_typed_record_rejection() {
    use mesh_replication::{
        CompleteInventory,
        pull::{PullErrorKind, pull_over_session},
    };
    let remote = CompleteInventory::from_ids(&[[1; 32], [2; 32]], 2).unwrap();
    let local = CompleteInventory::from_ids(&[], 2).unwrap();
    let mut session = Session {
        response: response(&[1, 3]),
        ..Default::default()
    };
    let error = pull_over_session(&mut session, [7; 32], &remote, &local, limits(), |_| {
        Ok::<_, ()>(())
    })
    .await
    .unwrap_err();
    assert_eq!(error.committed, 0);
    assert_eq!(error.kind, PullErrorKind::UnexpectedRecords);
}

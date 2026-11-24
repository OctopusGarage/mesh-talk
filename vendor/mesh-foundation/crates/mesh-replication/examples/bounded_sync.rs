//! One caller-scheduled synchronization turn. Replace callbacks with an
//! authenticated range session and an authorized transactional record store.
use mesh_replication::{
    driver::{DriverLimits, SyncDriver},
    pull::Record,
    range::RangeIndex,
};
use sha2::{Digest, Sha256};
use std::convert::Infallible;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let body = b"a verified application record";
    let id: [u8; 32] = Sha256::digest(body).into();
    let mut source = RangeIndex::default();
    source.insert(id);
    let mut local = RangeIndex::default();
    let (scope, authority, view) = ([1; 32], [2; 32], [3; 32]);
    let mut sync = SyncDriver::new(scope, authority, &local, source.revision(), view);
    let result = sync
        .turn(
            &mut local,
            DriverLimits::default(),
            || true, // Recheck the real scope/authority/snapshot on each call.
            async |prefix| Ok::<_, Infallible>(source.frame(scope, view, prefix)),
            async |ids, _| {
                assert_eq!(ids, &[id]);
                Ok::<_, Infallible>(vec![Record {
                    id,
                    body: body.to_vec(),
                }])
            },
            |records| {
                for record in records {
                    assert_eq!(<[u8; 32]>::from(Sha256::digest(&record.body)), record.id);
                }
                // Real hosts durably commit these records atomically here.
                Ok::<_, Infallible>(())
            },
        )
        .await
        .unwrap();
    assert!(result.complete);
    assert_eq!(result.committed, 1);
    println!(
        "{} committed record; {} bounded requests",
        result.committed, result.requests
    );
}

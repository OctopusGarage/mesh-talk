use mesh_replication::{
    driver::{DriverError, DriverProgress, FetchError},
    range::RemoteError,
    reader::PrepareError,
};
use std::{error::Error, io};
fn cause() -> io::Error {
    io::Error::other("disk offline")
}
#[test]
fn errors_compose_without_losing_causes_or_progress() {
    let prepare: PrepareError<io::Error, io::Error> = PrepareError::Read(cause());
    assert!(prepare.to_string().contains("read"));
    assert_eq!(prepare.source().unwrap().to_string(), "disk offline");
    let allowance: PrepareError<io::Error, io::Error> = PrepareError::RecordAllowance {
        required: 12,
        allowance: 3,
    };
    assert!(allowance.to_string().contains("12"));
    assert!(allowance.to_string().contains("3"));
    assert!(allowance.source().is_none());
    let driver: DriverError<io::Error, io::Error, io::Error> = DriverError::Query {
        progress: DriverProgress {
            committed: 7,
            ..Default::default()
        },
        error: RemoteError::Exchange(cause()),
    };
    assert!(driver.to_string().contains("query"));
    assert_eq!(
        driver.source().unwrap().source().unwrap().to_string(),
        "disk offline"
    );
    assert!(matches!(
        driver,
        DriverError::Query {
            progress: DriverProgress { committed: 7, .. },
            ..
        }
    ));
    let fetch = FetchError::Exchange(cause());
    assert_eq!(fetch.source().unwrap().to_string(), "disk offline");
    let invalid: RemoteError<io::Error> = RemoteError::InvalidFrame;
    assert!(invalid.to_string().contains("frame"));
    assert!(invalid.source().is_none());
}

use mesh_blob::{BlobVerifier, file_checksum};

#[test]
fn rejects_missing_out_of_order_and_oversized_chunks() {
    let mut receive = BlobVerifier::new(2, 2, file_checksum(b"ab")).unwrap();
    assert!(receive.push(1, b"b").is_err());
    assert!(receive.push(0, b"abc").is_err());
    receive.push(0, b"a").unwrap();
    assert!(receive.push(0, b"a").is_err());
    receive.push(1, b"b").unwrap();
    assert!(receive.push(2, b"").is_err());
    receive.finish().unwrap();

    let mut truncated = BlobVerifier::new(2, 2, file_checksum(b"ab")).unwrap();
    truncated.push(0, b"a").unwrap();
    assert!(truncated.finish().is_err());
}

#[test]
fn requires_the_empty_chunk_and_checks_the_final_digest() {
    let mut empty = BlobVerifier::new(1, 0, file_checksum(b"")).unwrap();
    empty.push(0, b"").unwrap();
    empty.finish().unwrap();

    let mut corrupt = BlobVerifier::new(1, 2, file_checksum(b"ab")).unwrap();
    corrupt.push(0, b"ac").unwrap();
    assert!(matches!(
        corrupt.finish(),
        Err(mesh_blob::FileError::ChecksumMismatch)
    ));
}

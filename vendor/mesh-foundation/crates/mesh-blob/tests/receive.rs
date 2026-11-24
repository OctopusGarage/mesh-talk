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

#[test]
fn validated_layout_rejects_overflow_and_enforces_exact_chunk_sizes() {
    use mesh_blob::BlobLayout;

    assert!(BlobLayout::new(u64::MAX, 1, u64::MAX).is_err());
    assert!(BlobLayout::new(1, 0, 1).is_err());
    assert!(BlobLayout::new(5, 2, 4).is_err());
    let layout = BlobLayout::new(5, 2, 5).unwrap();
    assert_eq!(layout.chunk_count(), 3);
    assert_eq!(layout.chunk_len(2).unwrap(), 1);
    assert!(layout.chunk_len(3).is_err());
    let mut verifier = BlobVerifier::for_layout(layout, file_checksum(b"abcde"));
    assert!(verifier.push(0, b"a").is_err());
    assert_eq!(verifier.next_index(), 0);
    verifier.push(0, b"ab").unwrap();
    verifier.push(1, b"cd").unwrap();
    verifier.push(2, b"e").unwrap();
    verifier.finish().unwrap();
}

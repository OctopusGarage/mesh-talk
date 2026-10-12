use mesh_talk_core::recovery::archive::{ArchiveError, ArchiveWriter, read_archive, write_archive};
use mesh_talk_core::recovery::{BackupError, snapshot::SnapshotError};
use std::io::{Cursor, Write};

const KEY: [u8; 32] = [0x5a; 32];

fn encrypted(bytes: &[u8]) -> Vec<u8> {
    let mut archive = Vec::new();
    write_archive(Cursor::new(bytes), &mut archive, &KEY).unwrap();
    archive
}

#[test]
fn round_trip_streams_across_multiple_chunks() {
    let plaintext = vec![0x42; 150_000];
    let archive = encrypted(&plaintext);
    let mut restored = Vec::new();
    let info = read_archive(Cursor::new(archive), &mut restored, &KEY).unwrap();
    assert_eq!(restored, plaintext);
    assert_eq!(info.plaintext_bytes, plaintext.len() as u64);
    assert!(info.chunks > 1);
}

#[test]
fn wrong_key_cannot_complete_restore() {
    let archive = encrypted(b"private history");
    let error = read_archive(Cursor::new(archive), &mut Vec::new(), &[0x11; 32]).unwrap_err();
    assert_eq!(error, ArchiveError::InvalidArchive);
}

#[test]
fn changed_version_or_frame_metadata_is_rejected_before_publication() {
    let archive = encrypted(b"private history");
    let mut future = archive.clone();
    future[8] = 2;
    assert_eq!(
        read_archive(Cursor::new(future), &mut Vec::new(), &KEY).unwrap_err(),
        ArchiveError::UnsupportedVersion
    );

    for offset in [25usize, 33, 37] {
        let mut changed = archive.clone();
        changed[offset] ^= 0x7f;
        assert_eq!(
            read_archive(Cursor::new(changed), &mut Vec::new(), &KEY).unwrap_err(),
            ArchiveError::InvalidArchive
        );
    }
}

#[test]
fn failed_private_staging_write_aborts_archive_read() {
    struct RejectWrites;
    impl Write for RejectWrites {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("staging disk full"))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let archive = encrypted(b"private history");
    assert_eq!(
        read_archive(Cursor::new(archive), RejectWrites, &KEY).unwrap_err(),
        ArchiveError::Io
    );
}

#[test]
fn truncated_final_record_cannot_complete_restore() {
    let mut archive = encrypted(b"private history");
    archive.truncate(archive.len() - 1);
    let error = read_archive(Cursor::new(archive), &mut Vec::new(), &KEY).unwrap_err();
    assert_eq!(error, ArchiveError::InvalidArchive);
}

#[test]
fn modified_ciphertext_cannot_complete_restore() {
    let mut archive = encrypted(b"private history");
    archive[50] ^= 1;
    let error = read_archive(Cursor::new(archive), &mut Vec::new(), &KEY).unwrap_err();
    assert_eq!(error, ArchiveError::InvalidArchive);
}

#[test]
fn appended_bytes_cannot_complete_restore() {
    let mut archive = encrypted(b"private history");
    archive.extend_from_slice(b"extra");
    let error = read_archive(Cursor::new(archive), &mut Vec::new(), &KEY).unwrap_err();
    assert_eq!(error, ArchiveError::InvalidArchive);
}

#[test]
fn streaming_writer_seals_only_after_finish() {
    let mut archive = Vec::new();
    let info = {
        let mut writer = ArchiveWriter::new(&mut archive, &KEY).unwrap();
        writer.write_all(b"hello ").unwrap();
        writer.write_all(b"world").unwrap();
        writer.finish().unwrap()
    };
    let mut restored = Vec::new();
    read_archive(Cursor::new(archive), &mut restored, &KEY).unwrap();
    assert_eq!(restored, b"hello world");
    assert_eq!(info.plaintext_bytes, 11);
}

#[test]
fn abandoned_streaming_writer_leaves_invalid_archive() {
    let mut archive = Vec::new();
    {
        let mut writer = ArchiveWriter::new(&mut archive, &KEY).unwrap();
        writer.write_all(b"secret").unwrap();
    }
    assert_eq!(
        read_archive(Cursor::new(archive), &mut Vec::new(), &KEY).unwrap_err(),
        ArchiveError::InvalidArchive
    );
}

#[test]
fn public_backup_errors_keep_authentication_failures_ambiguous() {
    use std::collections::HashSet;
    let errors = [
        ArchiveError::InvalidArchive,
        ArchiveError::UnsupportedVersion,
        ArchiveError::SizeLimit,
        ArchiveError::Io,
    ];
    let messages: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert_eq!(
        messages.iter().collect::<HashSet<_>>().len(),
        messages.len()
    );
    assert!(
        ArchiveError::InvalidArchive
            .to_string()
            .contains("archive or recovery key")
    );

    let wrapped: BackupError = ArchiveError::InvalidArchive.into();
    assert_eq!(
        wrapped.to_string(),
        ArchiveError::InvalidArchive.to_string()
    );
    let wrapped: BackupError = SnapshotError::UnsupportedVersion.into();
    assert_eq!(
        wrapped.to_string(),
        SnapshotError::UnsupportedVersion.to_string()
    );
    let wrapped: BackupError = std::io::Error::other("disk full").into();
    assert_eq!(wrapped.to_string(), "disk full");
}

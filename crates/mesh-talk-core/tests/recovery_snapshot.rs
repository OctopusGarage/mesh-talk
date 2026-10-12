use mesh_talk_core::identity::account::Account;
use mesh_talk_core::recovery::snapshot::{
    SnapshotError, SnapshotHeaderV1, SnapshotRecordV1, SnapshotValidator, SnapshotWriter,
    read_snapshot, write_snapshot,
};
use std::io::Cursor;

fn header() -> SnapshotHeaderV1 {
    let secret = [0x7f; 32];
    SnapshotHeaderV1 {
        schema_version: 1,
        account_secret: secret,
        account_id: Account::from_secret_bytes(secret).account_id(),
        source_device_ed25519_pubs: vec![[1; 32]],
        created_at_ms: 100,
        record_count: 1,
    }
}

fn received() -> SnapshotRecordV1 {
    SnapshotRecordV1::ReceivedDm {
        conversation: [1; 32],
        event_id: [2; 32],
        from: "peer".into(),
        wall_clock: 90,
        plaintext: b"hello".to_vec(),
    }
}

#[test]
fn valid_history_snapshot_completes() {
    let mut validator = SnapshotValidator::new(header()).unwrap();
    validator.accept(&received()).unwrap();
    validator.finish().unwrap();
}

#[test]
fn account_fingerprint_must_match_secret() {
    let mut header = header();
    header.account_id = "wrong".into();
    assert_eq!(
        SnapshotValidator::new(header).unwrap_err(),
        SnapshotError::InvalidAccount
    );
}

#[test]
fn duplicate_message_id_is_rejected() {
    let mut header = header();
    header.record_count = 2;
    let mut validator = SnapshotValidator::new(header).unwrap();
    validator.accept(&received()).unwrap();
    assert_eq!(
        validator.accept(&received()),
        Err(SnapshotError::DuplicateRecord)
    );
}

#[test]
fn one_signed_received_event_cannot_claim_two_conversations() {
    let mut header = header();
    header.record_count = 2;
    let mut validator = SnapshotValidator::new(header).unwrap();
    let first = received();
    let mut second = received();
    if let SnapshotRecordV1::ReceivedDm { conversation, .. } = &mut second {
        *conversation = [9; 32];
    }
    validator.accept(&first).unwrap();
    assert_eq!(
        validator.accept(&second),
        Err(SnapshotError::DuplicateRecord)
    );
}

#[test]
fn missing_record_is_rejected_at_finish() {
    let validator = SnapshotValidator::new(header()).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::RecordCount));
}

#[test]
fn oversized_plaintext_is_rejected() {
    let mut validator = SnapshotValidator::new(header()).unwrap();
    let mut record = received();
    if let SnapshotRecordV1::ReceivedDm { plaintext, .. } = &mut record {
        plaintext.resize(1024 * 1024 + 1, 0);
    }
    assert_eq!(validator.accept(&record), Err(SnapshotError::SizeLimit));
}

#[test]
fn snapshot_records_round_trip_without_a_whole_history_buffer() {
    let mut encoded = Vec::new();
    write_snapshot(&mut encoded, &header(), [received()]).unwrap();
    let mut decoded = Vec::new();
    let restored_header = read_snapshot(Cursor::new(encoded), |record| {
        decoded.push(record);
        Ok(())
    })
    .unwrap();
    assert_eq!(restored_header, header());
    assert_eq!(decoded, vec![received()]);
}

#[test]
fn snapshot_trailing_bytes_are_rejected() {
    let mut encoded = Vec::new();
    write_snapshot(&mut encoded, &header(), [received()]).unwrap();
    encoded.push(1);
    assert_eq!(
        read_snapshot(Cursor::new(encoded), |_| Ok(())).unwrap_err(),
        SnapshotError::InvalidEncoding
    );
}

#[test]
fn account_secret_is_redacted_from_debug_output() {
    let debug = format!("{:?}", header());
    assert!(!debug.contains("127, 127, 127"));
}

#[test]
fn incremental_writer_produces_a_valid_snapshot() {
    let mut encoded = Vec::new();
    {
        let mut writer = SnapshotWriter::new(&mut encoded, &header()).unwrap();
        writer.push(&received()).unwrap();
        writer.finish().unwrap();
    }
    let mut count = 0;
    read_snapshot(Cursor::new(encoded), |_| {
        count += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn media_chunks_must_match_declared_digest_and_length() {
    use sha2::{Digest, Sha256};
    let payload = b"image bytes";
    let digest: [u8; 32] = Sha256::digest(payload).into();
    let mut header = header();
    header.record_count = 2;
    let mut validator = SnapshotValidator::new(header).unwrap();
    validator
        .accept(&SnapshotRecordV1::MediaDescriptor {
            file_conv: [8; 32],
            size: payload.len() as u64,
            sha256: digest,
        })
        .unwrap();
    validator
        .accept(&SnapshotRecordV1::MediaChunk {
            file_conv: [8; 32],
            index: 0,
            bytes: payload.to_vec(),
        })
        .unwrap();
    validator.finish().unwrap();
}

#[test]
fn missing_media_chunk_fails_at_finish() {
    let mut validator = SnapshotValidator::new(header()).unwrap();
    validator
        .accept(&SnapshotRecordV1::MediaDescriptor {
            file_conv: [8; 32],
            size: 10,
            sha256: [0; 32],
        })
        .unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia));
}

#[test]
fn signed_event_is_accepted_for_channel_archive_replay() {
    use mesh_talk_core::eventlog::event::{ConversationId, Event, EventKind};
    use mesh_talk_core::identity::device::DeviceIdentity;
    let device = DeviceIdentity::generate();
    let event = Event::new(
        &device,
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        90,
        EventKind::Message,
        b"ciphertext".to_vec(),
    );
    let mut validator = SnapshotValidator::new(header()).unwrap();
    validator
        .accept(&SnapshotRecordV1::SignedEvent { event })
        .unwrap();
    validator.finish().unwrap();
}

#[test]
fn changed_signed_event_is_rejected() {
    use mesh_talk_core::eventlog::event::{ConversationId, Event, EventKind};
    use mesh_talk_core::identity::device::DeviceIdentity;
    let device = DeviceIdentity::generate();
    let mut event = Event::new(
        &device,
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        90,
        EventKind::Message,
        b"ciphertext".to_vec(),
    );
    event.ciphertext[0] ^= 1;
    let mut validator = SnapshotValidator::new(header()).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::SignedEvent { event }),
        Err(SnapshotError::InvalidEvent)
    );
}

#[test]
fn malformed_file_card_manifest_is_rejected() {
    let mut validator = SnapshotValidator::new(header()).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::FileCard {
            conversation: [1; 32],
            event_id: [2; 32],
            from: "peer".into(),
            wall_clock: 42,
            manifest: b"not a manifest".to_vec(),
        }),
        Err(SnapshotError::InvalidFileCard)
    );
}

#[test]
fn file_card_must_match_its_signed_event_author_and_time() {
    use mesh_talk_core::eventlog::event::{ConversationId, Event, EventKind};
    use mesh_talk_core::file::FileManifest;
    use mesh_talk_core::identity::device::DeviceIdentity;
    let device = DeviceIdentity::generate();
    let event = Event::new(
        &device,
        ConversationId::new([1; 32]),
        1,
        vec![],
        1,
        90,
        EventKind::FileManifest,
        b"sealed manifest".to_vec(),
    );
    let manifest = FileManifest {
        name: "photo.png".into(),
        size: 1,
        mime: "image/png".into(),
        checksum: mesh_talk_core::file::file_checksum(b"a"),
        file_key: [3; 32],
        file_conv: ConversationId::new([9; 32]),
        chunk_count: 1,
    };
    let mut header = header();
    header.record_count = 2;
    let mut validator = SnapshotValidator::new(header).unwrap();
    validator
        .accept(&SnapshotRecordV1::SignedEvent {
            event: event.clone(),
        })
        .unwrap();
    validator
        .accept(&SnapshotRecordV1::FileCard {
            conversation: [1; 32],
            event_id: *event.id.as_bytes(),
            from: "forged-author".into(),
            wall_clock: event.wall_clock,
            manifest: manifest.encode(),
        })
        .unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidFileCard));
}

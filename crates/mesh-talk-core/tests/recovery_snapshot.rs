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
fn incompatible_or_ambiguous_snapshot_header_is_rejected() {
    let mut future = header();
    future.schema_version += 1;
    assert_eq!(
        SnapshotValidator::new(future).unwrap_err(),
        SnapshotError::UnsupportedVersion
    );

    let mut no_author = header();
    no_author.source_device_ed25519_pubs.clear();
    assert_eq!(
        SnapshotValidator::new(no_author).unwrap_err(),
        SnapshotError::InvalidAccount
    );

    let mut duplicate_author = header();
    duplicate_author.source_device_ed25519_pubs.push([1; 32]);
    assert_eq!(
        SnapshotValidator::new(duplicate_author).unwrap_err(),
        SnapshotError::InvalidAccount
    );

    let mut excessive = header();
    excessive.record_count = u64::MAX;
    assert_eq!(
        SnapshotValidator::new(excessive).unwrap_err(),
        SnapshotError::SizeLimit
    );
}

#[test]
fn file_only_contact_requires_valid_identity_and_a_matching_file_card() {
    let contact = SnapshotRecordV1::AccountContact {
        account_id: "a".repeat(32),
    };
    let mut validator = SnapshotValidator::new(header()).unwrap();
    validator.accept(&contact).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidFileCard));

    let mut validator = SnapshotValidator::new(header()).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::AccountContact {
            account_id: "bad".into()
        }),
        Err(SnapshotError::InvalidAccount)
    );

    let mut duplicate_header = header();
    duplicate_header.record_count = 2;
    let mut validator = SnapshotValidator::new(duplicate_header).unwrap();
    validator.accept(&contact).unwrap();
    assert_eq!(
        validator.accept(&contact),
        Err(SnapshotError::DuplicateRecord)
    );
}

#[test]
fn duplicate_profile_and_media_descriptors_cannot_replace_restored_data() {
    use sha2::Digest;
    let mut two_records = header();
    two_records.record_count = 2;
    let profile = SnapshotRecordV1::Profile {
        account_id: "peer".into(),
        updated_at: 1,
        avatar: None,
    };
    let mut validator = SnapshotValidator::new(two_records.clone()).unwrap();
    validator.accept(&profile).unwrap();
    assert_eq!(
        validator.accept(&profile),
        Err(SnapshotError::DuplicateRecord)
    );

    let media = SnapshotRecordV1::MediaDescriptor {
        file_conv: [8; 32],
        size: 0,
        sha256: sha2::Sha256::digest([]).into(),
    };
    let mut validator = SnapshotValidator::new(two_records).unwrap();
    validator.accept(&media).unwrap();
    assert_eq!(
        validator.accept(&media),
        Err(SnapshotError::DuplicateRecord)
    );
}

#[test]
fn out_of_order_or_oversized_media_chunks_are_rejected() {
    let mut two_records = header();
    two_records.record_count = 2;
    let descriptor = SnapshotRecordV1::MediaDescriptor {
        file_conv: [8; 32],
        size: 2,
        sha256: [0; 32],
    };
    let mut validator = SnapshotValidator::new(two_records.clone()).unwrap();
    validator.accept(&descriptor).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::MediaChunk {
            file_conv: [8; 32],
            index: 1,
            bytes: b"a".to_vec()
        }),
        Err(SnapshotError::InvalidMedia)
    );

    let mut validator = SnapshotValidator::new(two_records).unwrap();
    validator.accept(&descriptor).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::MediaChunk {
            file_conv: [8; 32],
            index: 0,
            bytes: b"too long".to_vec()
        }),
        Err(SnapshotError::InvalidMedia)
    );
}

#[test]
fn duplicate_message_id_is_rejected() {
    let mut first_header = header();
    first_header.record_count = 2;
    let mut validator = SnapshotValidator::new(first_header).unwrap();
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
fn malformed_snapshot_lengths_are_rejected_before_allocation() {
    let mut encoded = Vec::new();
    write_snapshot(&mut encoded, &header(), [received()]).unwrap();

    let mut oversized_header = encoded.clone();
    oversized_header[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        read_snapshot(Cursor::new(oversized_header), |_| Ok(())).unwrap_err(),
        SnapshotError::SizeLimit
    );

    let header_len = u32::from_be_bytes(encoded[8..12].try_into().unwrap()) as usize;
    let record_length_offset = 12 + header_len;
    let mut oversized_record = encoded;
    oversized_record[record_length_offset..record_length_offset + 4]
        .copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(
        read_snapshot(Cursor::new(oversized_record), |_| Ok(())).unwrap_err(),
        SnapshotError::SizeLimit
    );
}

#[test]
fn recovery_errors_have_distinct_public_messages() {
    use std::collections::HashSet;
    let errors = [
        SnapshotError::UnsupportedVersion,
        SnapshotError::InvalidAccount,
        SnapshotError::InvalidEncoding,
        SnapshotError::SizeLimit,
        SnapshotError::DuplicateRecord,
        SnapshotError::RecordCount,
        SnapshotError::InvalidMedia,
        SnapshotError::InvalidEvent,
        SnapshotError::InvalidFileCard,
        SnapshotError::Conflict,
        SnapshotError::Io,
    ];
    let messages: Vec<_> = errors.iter().map(ToString::to_string).collect();
    assert!(messages.iter().all(|message| !message.is_empty()));
    assert_eq!(
        messages.iter().collect::<HashSet<_>>().len(),
        messages.len()
    );
    assert!(messages.iter().all(|message| !message.contains("secret")));
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
    let mut first_header = header();
    first_header.record_count = 2;
    let mut validator = SnapshotValidator::new(first_header).unwrap();
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

    let mut matched_header = header();
    matched_header.record_count = 2;
    let mut validator = SnapshotValidator::new(matched_header).unwrap();
    validator
        .accept(&SnapshotRecordV1::SignedEvent {
            event: event.clone(),
        })
        .unwrap();
    validator
        .accept(&SnapshotRecordV1::FileCard {
            conversation: [2; 32],
            event_id: *event.id.as_bytes(),
            from: event.author.user_id(),
            wall_clock: event.wall_clock,
            manifest: manifest.encode(),
        })
        .unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidFileCard));
}

use mesh_talk_core::identity::account::Account;
use mesh_talk_core::recovery::snapshot::{
    SNAPSHOT_SCHEMA_VERSION, SnapshotError, SnapshotHeaderV1, SnapshotRecordV1, SnapshotValidator,
    SnapshotWriter, read_snapshot, write_snapshot,
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
    for schema_version in [1, SNAPSHOT_SCHEMA_VERSION] {
        let mut header = header();
        header.schema_version = schema_version;
        if schema_version >= 4 {
            header.record_count += 1;
        }
        let mut validator = SnapshotValidator::new(header).unwrap();
        validator.accept(&received()).unwrap();
        if schema_version >= 4 {
            validator
                .accept(&SnapshotRecordV1::AccountCreation {
                    created_at_ms: None,
                })
                .unwrap();
        }
        validator.finish().unwrap();
    }
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
    future.schema_version = SNAPSHOT_SCHEMA_VERSION + 1;
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

#[test]
fn trust_card_snapshot_checks_signature_version_and_duplicates() {
    let account = Account::generate();
    let card = mesh_talk_core::node::TrustCard::new(
        &account,
        1,
        "Alice".into(),
        "bio".into(),
        Some(42),
        None,
    );
    let record = SnapshotRecordV1::PeerTrustCard { card: card.clone() };
    let mut current = header();
    current.schema_version = SNAPSHOT_SCHEMA_VERSION;
    current.record_count = 2;
    let mut validator = SnapshotValidator::new(current.clone()).unwrap();
    validator.accept(&record).unwrap();
    assert_eq!(
        validator.accept(&record),
        Err(SnapshotError::DuplicateRecord)
    );
    let mut bad = card;
    bad.bio = "forged".into();
    let mut validator = SnapshotValidator::new(current).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::PeerTrustCard { card: bad }),
        Err(SnapshotError::InvalidEncoding)
    );
    let mut validator = SnapshotValidator::new(header()).unwrap();
    assert_eq!(
        validator.accept(&record),
        Err(SnapshotError::UnsupportedVersion)
    );
}

// Keep the private journal representation behind the owner adapter in production.
// This wire fixture pins the portable payload and canonical ID recipe independently.
#[derive(Clone, serde::Serialize)]
struct FavoriteFixture {
    generation: u64,
    favorite: mesh_talk_core::node::MessageFavorite,
    removed: bool,
}
fn favorite_fixture() -> FavoriteFixture {
    use mesh_talk_core::node::{
        FavoriteFile, FavoriteMessage, FavoriteMetadata, FavoriteSource, MessageFavorite,
    };
    use sha2::{Digest, Sha256};
    let source = FavoriteSource {
        kind: "account".into(),
        target: "ab".repeat(16),
        message_ids: vec![hex::encode([19; 32])],
    };
    let mut digest = Sha256::new();
    digest.update(b"mesh-talk-favorite-v1");
    digest.update(&header().account_id);
    digest.update(bincode::serialize(&source).unwrap());
    FavoriteFixture {
        generation: 1,
        removed: false,
        favorite: MessageFavorite {
            id: hex::encode(digest.finalize()),
            source,
            source_label: "Peer".into(),
            metadata: FavoriteMetadata::default(),
            messages: vec![FavoriteMessage {
                id: hex::encode([19; 32]),
                who: "ab".repeat(16),
                sender_name: "Peer".into(),
                text: "你好 🌍".into(),
                wall_clock: 1,
                expires_at_ms: None,
                sticker: None,
                file: Some(FavoriteFile {
                    file_conv: hex::encode([27; 32]),
                    name: "retained.bin".into(),
                    size: 3,
                    mime: "application/octet-stream".into(),
                    media: false,
                }),
            }],
            saved_at_ms: 1,
            revision: 1,
            device_id: "cd".repeat(16),
        },
    }
}
fn sanitize_tombstone(fixture: &mut FavoriteFixture) {
    fixture.removed = true;
    fixture.favorite.metadata = Default::default();
    fixture.favorite.source_label.clear();
    for message in &mut fixture.favorite.messages {
        message.text.clear();
        message.file = None;
        message.sticker = None;
        message.who.clear();
        message.sender_name.clear();
    }
}
fn favorite_record(fixture: &FavoriteFixture) -> SnapshotRecordV1 {
    SnapshotRecordV1::Favorite {
        payload: bincode::serialize(fixture).unwrap(),
    }
}
fn favorite_header(count: u64) -> SnapshotHeaderV1 {
    let mut value = header();
    value.schema_version = 4;
    value.record_count = count;
    value
}
fn favorite_media_records() -> [SnapshotRecordV1; 2] {
    use sha2::{Digest, Sha256};
    [
        SnapshotRecordV1::FavoriteMediaDescriptor {
            file_conv: [27; 32],
            size: 3,
            sha256: Sha256::digest(b"abc").into(),
        },
        SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 0,
            bytes: b"abc".to_vec(),
        },
    ]
}
#[test]
fn schema_four_favorites_roundtrip_and_preserve_all_older_record_tags() {
    let records = [favorite_record(&favorite_fixture())]
        .into_iter()
        .chain(favorite_media_records());
    let mut encoded = Vec::new();
    write_snapshot(&mut encoded, &favorite_header(3), records).unwrap();
    let restored = read_snapshot(Cursor::new(encoded), |_| Ok(())).unwrap();
    assert_eq!(restored.schema_version, 4);
    for version in [1, 2, 3] {
        let mut value = header();
        value.schema_version = version;
        let mut encoded = Vec::new();
        write_snapshot(&mut encoded, &value, [received()]).unwrap();
        read_snapshot(Cursor::new(encoded), |_| Ok(())).unwrap();
        let mut validator = SnapshotValidator::new(value).unwrap();
        assert_eq!(
            validator.accept(&favorite_record(&favorite_fixture())),
            Err(SnapshotError::UnsupportedVersion)
        );
    }
    assert_eq!(
        &bincode::serialize(&received()).unwrap()[..4],
        &1u32.to_le_bytes()
    );
    assert_eq!(
        &bincode::serialize(&favorite_record(&favorite_fixture())).unwrap()[..4],
        &12u32.to_le_bytes()
    );
}
#[test]
fn favorite_payloads_reject_duplicates_wrong_owner_malformed_trailing_and_excessive_data() {
    let fixture = favorite_fixture();
    let record = favorite_record(&fixture);
    let mut validator = SnapshotValidator::new(favorite_header(2)).unwrap();
    validator.accept(&record).unwrap();
    assert_eq!(
        validator.accept(&record),
        Err(SnapshotError::DuplicateRecord)
    );
    let mut wrong_owner = fixture.clone();
    wrong_owner.favorite.id = hex::encode([99; 32]);
    for record in [
        favorite_record(&wrong_owner),
        {
            let mut removed_with_content = fixture.clone();
            removed_with_content.removed = true;
            favorite_record(&removed_with_content)
        },
        {
            let mut expired_with_content = fixture.clone();
            expired_with_content.favorite.messages[0].expires_at_ms = Some(1);
            favorite_record(&expired_with_content)
        },
        SnapshotRecordV1::Favorite {
            payload: vec![0; 32],
        },
        {
            let mut payload = bincode::serialize(&fixture).unwrap();
            payload.push(0);
            SnapshotRecordV1::Favorite { payload }
        },
    ] {
        let mut validator = SnapshotValidator::new(favorite_header(1)).unwrap();
        assert_eq!(
            validator.accept(&record),
            Err(SnapshotError::InvalidEncoding)
        );
    }
    let mut validator = SnapshotValidator::new(favorite_header(1)).unwrap();
    assert_eq!(
        validator.accept(&SnapshotRecordV1::Favorite {
            payload: vec![0; 256 * 1024 + 1]
        }),
        Err(SnapshotError::SizeLimit)
    );
    let mut invalid = fixture;
    invalid
        .favorite
        .metadata
        .notes
        .insert(hex::encode([98; 32]), "not selected".into());
    let mut validator = SnapshotValidator::new(favorite_header(1)).unwrap();
    assert_eq!(
        validator.accept(&favorite_record(&invalid)),
        Err(SnapshotError::InvalidEncoding)
    );
}
#[test]
fn favorite_media_validation_covers_namespace_refs_size_order_hash_and_completeness() {
    let fixture = favorite_fixture();
    let [descriptor, chunk] = favorite_media_records();
    let mut validator = SnapshotValidator::new(favorite_header(1)).unwrap();
    validator.accept(&favorite_record(&fixture)).unwrap();
    validator.finish().unwrap(); // card-only
    let mut validator = SnapshotValidator::new(favorite_header(2)).unwrap();
    validator.accept(&descriptor).unwrap();
    validator.accept(&chunk).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia)); // orphan
    for bad in [
        SnapshotRecordV1::FavoriteMediaDescriptor {
            file_conv: [27; 32],
            size: 64 * 1024 * 1024 + 1,
            sha256: [0; 32],
        },
        SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 0,
            bytes: b"abc".to_vec(),
        },
    ] {
        let mut validator = SnapshotValidator::new(favorite_header(1)).unwrap();
        assert!(validator.accept(&bad).is_err());
    }
    let mut validator = SnapshotValidator::new(favorite_header(2)).unwrap();
    validator.accept(&descriptor).unwrap();
    assert_eq!(
        validator.accept(&descriptor),
        Err(SnapshotError::DuplicateRecord)
    );
    for bad_chunk in [
        SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 1,
            bytes: b"abc".to_vec(),
        },
        SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 0,
            bytes: vec![0; 64 * 1024 + 1],
        },
        SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 0,
            bytes: vec![],
        },
    ] {
        let mut validator = SnapshotValidator::new(favorite_header(2)).unwrap();
        validator.accept(&descriptor).unwrap();
        assert_eq!(
            validator.accept(&bad_chunk),
            Err(SnapshotError::InvalidMedia)
        );
    }
    let mut validator = SnapshotValidator::new(favorite_header(2)).unwrap();
    validator.accept(&favorite_record(&fixture)).unwrap();
    validator.accept(&descriptor).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia));
    let mut bad_size = fixture.clone();
    bad_size.favorite.messages[0].file.as_mut().unwrap().size = 4;
    let mut validator = SnapshotValidator::new(favorite_header(3)).unwrap();
    validator.accept(&favorite_record(&bad_size)).unwrap();
    validator.accept(&descriptor).unwrap();
    validator.accept(&chunk).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia));
    let mut validator = SnapshotValidator::new(favorite_header(3)).unwrap();
    validator.accept(&favorite_record(&fixture)).unwrap();
    validator.accept(&descriptor).unwrap();
    validator
        .accept(&SnapshotRecordV1::FavoriteMediaChunk {
            file_conv: [27; 32],
            index: 0,
            bytes: b"bad".to_vec(),
        })
        .unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia));
    let mut validator = SnapshotValidator::new(favorite_header(5)).unwrap();
    validator.accept(&favorite_record(&fixture)).unwrap();
    validator.accept(&descriptor).unwrap();
    validator.accept(&chunk).unwrap();
    validator
        .accept(&SnapshotRecordV1::MediaDescriptor {
            file_conv: [27; 32],
            size: 0,
            sha256: {
                use sha2::Digest;
                sha2::Sha256::digest([]).into()
            },
        })
        .unwrap();
    validator.accept(&received()).unwrap();
    validator.finish().unwrap();
}
#[test]
fn favorite_and_regular_media_share_the_aggregate_budget() {
    let mut validator = SnapshotValidator::new(favorite_header(6)).unwrap();
    for n in 0..4 {
        validator
            .accept(&SnapshotRecordV1::MediaDescriptor {
                file_conv: [n; 32],
                size: 2 * 1024 * 1024 * 1024,
                sha256: [0; 32],
            })
            .unwrap();
    }
    assert_eq!(
        validator.accept(&favorite_media_records()[0]),
        Err(SnapshotError::SizeLimit)
    );
}
#[test]
fn favorite_count_limit_includes_removed_tombstones() {
    use sha2::{Digest, Sha256};
    let mut validator = SnapshotValidator::new(favorite_header(4097)).unwrap();
    let mut fixture = favorite_fixture();
    sanitize_tombstone(&mut fixture);
    for n in 0..=4096u32 {
        let mut message_id = [0; 32];
        message_id[..4].copy_from_slice(&(n + 1).to_be_bytes());
        fixture.favorite.source.message_ids = vec![hex::encode(message_id)];
        fixture.favorite.messages[0].id = hex::encode(message_id);
        let mut digest = Sha256::new();
        digest.update(b"mesh-talk-favorite-v1");
        digest.update(&header().account_id);
        digest.update(bincode::serialize(&fixture.favorite.source).unwrap());
        fixture.favorite.id = hex::encode(digest.finalize());
        let result = validator.accept(&favorite_record(&fixture));
        if n < 4096 {
            result.unwrap();
        } else {
            assert_eq!(result, Err(SnapshotError::SizeLimit));
        }
    }
}
#[test]
fn removed_favorite_cannot_authorize_retained_attachment_bytes() {
    let mut fixture = favorite_fixture();
    sanitize_tombstone(&mut fixture);
    let [descriptor, chunk] = favorite_media_records();
    let mut validator = SnapshotValidator::new(favorite_header(3)).unwrap();
    validator.accept(&favorite_record(&fixture)).unwrap();
    validator.accept(&descriptor).unwrap();
    validator.accept(&chunk).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidMedia));
}

#[test]
fn immutable_creation_metadata_requires_v4_and_rejects_invalid_or_duplicate_dates() {
    let record = SnapshotRecordV1::AccountCreation {
        created_at_ms: Some(1_700_000_000_000),
    };
    let mut old = header();
    old.schema_version = 3;
    assert_eq!(
        SnapshotValidator::new(old).unwrap().accept(&record),
        Err(SnapshotError::UnsupportedVersion)
    );
    let mut current = header();
    current.record_count = 2;
    current.schema_version = SNAPSHOT_SCHEMA_VERSION;
    let mut validator = SnapshotValidator::new(current.clone()).unwrap();
    validator.accept(&record).unwrap();
    assert_eq!(
        validator.accept(&record),
        Err(SnapshotError::DuplicateRecord)
    );
    for time in [0, 253_402_300_800_000, u64::MAX] {
        let mut validator = SnapshotValidator::new(current.clone()).unwrap();
        assert_eq!(
            validator.accept(&SnapshotRecordV1::AccountCreation {
                created_at_ms: Some(time)
            }),
            Err(SnapshotError::InvalidAccount)
        );
    }
}

#[test]
fn snapshot_requires_own_profile_to_match_authoritative_creation_metadata() {
    let account = Account::from_secret_bytes([0x7f; 32]);
    let mut profile = mesh_talk_core::node::AccountProfile {
        account_ed25519_pub: account.public().ed25519_pub,
        revision: 1,
        display_name: "Alice".into(),
        bio: String::new(),
        created_at_ms: Some(1_700_000_000_000),
        avatar: None,
        signature: [0; 64],
    };
    let mut input = b"mesh-talk-account-profile-v1".to_vec();
    input.extend(
        bincode::serialize(&(
            profile.account_ed25519_pub,
            profile.revision,
            &profile.display_name,
            &profile.bio,
            profile.created_at_ms,
            &profile.avatar,
        ))
        .unwrap(),
    );
    profile.signature = account.sign(&input);
    assert!(profile.verify());
    for metadata_first in [true, false] {
        for creation in [None, Some(1_700_000_000_000), Some(1_700_000_000_001)] {
            let mut current = header();
            current.schema_version = SNAPSHOT_SCHEMA_VERSION;
            current.record_count = 2;
            let mut validator = SnapshotValidator::new(current).unwrap();
            let metadata = SnapshotRecordV1::AccountCreation {
                created_at_ms: creation,
            };
            let record = SnapshotRecordV1::AccountProfile {
                profile: profile.clone(),
            };
            let records = if metadata_first {
                [metadata, record]
            } else {
                [record, metadata]
            };
            for record in records {
                validator.accept(&record).unwrap();
            }
            assert_eq!(
                validator.finish(),
                if creation == profile.created_at_ms {
                    Ok(())
                } else {
                    Err(SnapshotError::InvalidAccount)
                }
            );
        }
    }
}

#[test]
fn schema_five_cannot_silently_omit_authoritative_creation_metadata() {
    let mut current = header();
    current.schema_version = SNAPSHOT_SCHEMA_VERSION;
    let mut validator = SnapshotValidator::new(current).unwrap();
    validator.accept(&received()).unwrap();
    assert_eq!(validator.finish(), Err(SnapshotError::InvalidAccount));
}

// Frozen original development schema-4 wire bytes; independent of current enum serialization.
const LEGACY_FOUR_DRIVE: &str = concat!(
    "4d54534e415031000000008204007f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f2000",
    "000000000000393762353461656265396638633837646136636265366133303131376564623301000000000000000101",
    "010101010101010101010101010101010101010101010101010101010101640000000000000003000000000000000000",
    "02670c0000005b0200000000000001000000000000004000000000000000346164656535636333626365666339646536",
    "346663386661386365623464623162653230306432353661343164643234366536356337363165623466316261650700",
    "0000000000006163636f756e742000000000000000616261626162616261626162616261626162616261626162616261",
    "626162616201000000000000004000000000000000313331333133313331333133313331333133313331333133313331",
    "333133313331333133313331333133313331333133313331333133313331333133313331330400000000000000506565",
    "720000000000000000000000000000000000000000000000000100000000000000400000000000000031333133313331",
    "333133313331333133313331333133313331333133313331333133313331333133313331333133313331333133313331",
    "333133313331333133200000000000000061626162616261626162616261626162616261626162616261626162616261",
    "620400000000000000506565720b00000000000000e4bda0e5a5bd20f09f8c8d01000000000000000000014000000000",
    "000000316231623162316231623162316231623162316231623162316231623162316231623162316231623162316231",
    "623162316231623162316231623162316231620c0000000000000072657461696e65642e62696e030000000000000018",
    "000000000000006170706c69636174696f6e2f6f637465742d73747265616d0001000000000000000100000000000000",
    "20000000000000006364636463646364636463646364636463646364636463646364636463646364000000004c0d0000",
    "001b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b0300000000000000ba7816bf8f01cf",
    "ea414140de5dae2223b00361a396177a9cb410ff61f20015ad000000330e0000001b1b1b1b1b1b1b1b1b1b1b1b1b1b1b",
    "1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b000000000300000000000000616263",
);

// Frozen original development schema-4 wire bytes; independent of current enum serialization.
const LEGACY_FOUR_PROFILES: &str = concat!(
    "4d54534e415031000000008204007f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f7f2000",
    "000000000000393762353461656265396638633837646136636265366133303131376564623301000000000000000101",
    "010101010101010101010101010101010101010101010101010101010101640000000000000002000000000000000000",
    "000d0c000000010068e5cf8b0100000000008b0d000000b2a942ff4c98718bed76e255987f6d59b1a72d3b2cd2510003",
    "e6170ac63a9ffb01000000000000000500000000000000416c6963650000000000000000010068e5cf8b010000003e82",
    "6114d04c0c98cfd58584ca41f05284a4711beb7b1b26c41f18b69602a5b887e600f6d36cfc59214d55c76e0fc76c31a4",
    "42ec075c134dc15483772743d007",
);

#[test]
fn schema_six_appends_suppression_without_changing_existing_discriminants() {
    use mesh_talk_core::eventlog::event::{ConversationId, Event, EventKind};
    use mesh_talk_core::identity::device::DeviceIdentity;
    assert_eq!(SNAPSHOT_SCHEMA_VERSION, 6);
    let account = Account::from_secret_bytes([0x7f; 32]);
    let event = Event::new(
        &DeviceIdentity::generate(),
        ConversationId::new([3; 32]),
        1,
        vec![],
        1,
        90,
        EventKind::Message,
        vec![],
    );
    let mut profile = None;
    read_snapshot(
        Cursor::new(hex::decode(LEGACY_FOUR_PROFILES).unwrap()),
        |record| {
            if matches!(record, SnapshotRecordV1::AccountProfile { .. }) {
                profile = Some(record);
            }
            Ok(())
        },
    )
    .unwrap();
    let records = [
        SnapshotRecordV1::SentDm {
            conversation: [0; 32],
            message_id: [0; 32],
            seq: 0,
            wall_clock: 0,
            plaintext: vec![],
        },
        received(),
        SnapshotRecordV1::Recall {
            conversation: [0; 32],
            target: [0; 32],
            wall_clock: 0,
        },
        SnapshotRecordV1::Profile {
            account_id: account.account_id(),
            updated_at: 0,
            avatar: None,
        },
        SnapshotRecordV1::MediaDescriptor {
            file_conv: [0; 32],
            size: 0,
            sha256: [0; 32],
        },
        SnapshotRecordV1::MediaChunk {
            file_conv: [0; 32],
            index: 0,
            bytes: vec![],
        },
        SnapshotRecordV1::SignedEvent { event },
        SnapshotRecordV1::FileCard {
            conversation: [0; 32],
            event_id: [0; 32],
            from: String::new(),
            wall_clock: 0,
            manifest: vec![],
        },
        SnapshotRecordV1::AccountContact {
            account_id: account.account_id(),
        },
        SnapshotRecordV1::DmLanguageProfile { payload: vec![] },
        SnapshotRecordV1::DmLanguageDecision {
            id: [0; 32],
            hidden: false,
        },
        SnapshotRecordV1::PeerTrustCard {
            card: mesh_talk_core::node::TrustCard::new(
                &account,
                1,
                String::new(),
                String::new(),
                None,
                None,
            ),
        },
        favorite_record(&favorite_fixture()),
        favorite_media_records()[0].clone(),
        favorite_media_records()[1].clone(),
        SnapshotRecordV1::AccountCreation {
            created_at_ms: None,
        },
        profile.unwrap(),
        SnapshotRecordV1::LocalSuppression {
            conversation: [3; 32],
            message_id: [4; 32],
        },
    ];
    for (tag, record) in records.into_iter().enumerate() {
        assert_eq!(
            &bincode::serialize(&record).unwrap()[..4],
            &(tag as u32).to_le_bytes()
        );
    }
}

#[test]
fn suppression_records_require_schema_six_and_duplicate_facts_are_idempotent() {
    let marker = SnapshotRecordV1::LocalSuppression {
        conversation: [3; 32],
        message_id: [4; 32],
    };
    for version in 1..=5 {
        let mut header = header();
        header.schema_version = version;
        let mut writer = SnapshotWriter::new(Vec::new(), &header).unwrap();
        assert_eq!(writer.push(&marker), Err(SnapshotError::UnsupportedVersion));
    }
    let mut header = header();
    header.schema_version = 6;
    header.record_count = 3;
    let mut bytes = Vec::new();
    write_snapshot(
        &mut bytes,
        &header,
        [
            marker.clone(),
            marker.clone(),
            SnapshotRecordV1::AccountCreation {
                created_at_ms: None,
            },
        ],
    )
    .unwrap();
    let mut count = 0;
    read_snapshot(Cursor::new(bytes), |record| {
        if record == marker {
            count += 1;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(count, 2);
}

#[test]
fn legacy_four_frozen_fixtures_decode_both_original_families() {
    for (fixture, expected) in [(LEGACY_FOUR_DRIVE, 3), (LEGACY_FOUR_PROFILES, 2)] {
        let bytes = hex::decode(fixture).unwrap();
        let mut records = Vec::new();
        let restored = read_snapshot(Cursor::new(bytes), |record| {
            records.push(record);
            Ok(())
        })
        .unwrap();
        assert_eq!(restored.schema_version, 4);
        assert_eq!(records.len(), expected);
        if expected == 2 {
            assert!(matches!(
                records[0],
                SnapshotRecordV1::AccountCreation {
                    created_at_ms: Some(1_700_000_000_000)
                }
            ));
            let SnapshotRecordV1::AccountProfile { profile } = &records[1] else {
                panic!("legacy profile tag changed")
            };
            assert_eq!(profile.display_name, "Alice");
            assert!(profile.verify());
            assert_eq!(
                &bincode::serialize(&records[1]).unwrap()[..4],
                &16u32.to_le_bytes()
            );
        } else {
            assert!(matches!(records[0], SnapshotRecordV1::Favorite { .. }));
        }
    }
}

fn frozen_records(fixture: &str) -> Vec<Vec<u8>> {
    let bytes = hex::decode(fixture).unwrap();
    let header_len = u32::from_be_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut offset = 12 + header_len;
    let mut records = Vec::new();
    while offset < bytes.len() {
        let len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        records.push(bytes[offset..offset + len].to_vec());
        offset += len;
    }
    records
}

fn raw_record_snapshot(version: u16, records: &[Vec<u8>]) -> Vec<u8> {
    let mut header = header();
    header.schema_version = version;
    header.record_count = records.len() as u64;
    let header = bincode::serialize(&header).unwrap();
    let mut bytes = b"MTSNAP1\0".to_vec();
    bytes.extend((header.len() as u32).to_be_bytes());
    bytes.extend(header);
    for record in records {
        bytes.extend((record.len() as u32).to_be_bytes());
        bytes.extend(record);
    }
    bytes
}

#[test]
fn legacy_four_rejects_mixed_families_in_both_orders_and_missing_creation() {
    let favorites = frozen_records(LEGACY_FOUR_DRIVE);
    let profiles = frozen_records(LEGACY_FOUR_PROFILES);
    for records in [
        vec![favorites[0].clone(), profiles[0].clone()],
        vec![profiles[0].clone(), favorites[0].clone()],
        vec![profiles[1].clone(), favorites[0].clone()],
        vec![favorites[0].clone(), profiles[1].clone()],
    ] {
        assert_eq!(
            read_snapshot(Cursor::new(raw_record_snapshot(4, &records)), |_| Ok(())).unwrap_err(),
            SnapshotError::InvalidEncoding
        );
    }
    assert_eq!(
        read_snapshot(Cursor::new(raw_record_snapshot(4, &profiles[1..])), |_| Ok(
            ()
        ))
        .unwrap_err(),
        SnapshotError::InvalidAccount
    );
}

#[test]
fn legacy_four_rejects_trailing_corrupt_and_unknown_extension_encodings() {
    for fixture in [LEGACY_FOUR_DRIVE, LEGACY_FOUR_PROFILES] {
        let mut bytes = hex::decode(fixture).unwrap();
        bytes.push(0);
        assert_eq!(
            read_snapshot(Cursor::new(bytes), |_| Ok(())).unwrap_err(),
            SnapshotError::InvalidEncoding
        );
        let mut records = frozen_records(fixture);
        records[0].push(0);
        assert!(read_snapshot(Cursor::new(raw_record_snapshot(4, &records)), |_| Ok(())).is_err());
        let mut records = frozen_records(fixture);
        records[0].truncate(4);
        assert!(read_snapshot(Cursor::new(raw_record_snapshot(4, &records)), |_| Ok(())).is_err());
    }
    let mut invalid_creation = 12u32.to_le_bytes().to_vec();
    invalid_creation.push(1);
    invalid_creation.extend(0u64.to_le_bytes());
    assert_eq!(
        read_snapshot(
            Cursor::new(raw_record_snapshot(4, &[invalid_creation])),
            |_| Ok(())
        )
        .unwrap_err(),
        SnapshotError::InvalidAccount
    );
    let modern_creation = bincode::serialize(&SnapshotRecordV1::AccountCreation {
        created_at_ms: None,
    })
    .unwrap();
    assert_eq!(
        read_snapshot(
            Cursor::new(raw_record_snapshot(4, &[modern_creation])),
            |_| Ok(())
        )
        .unwrap_err(),
        SnapshotError::UnsupportedVersion
    );
    let mut profiles = frozen_records(LEGACY_FOUR_PROFILES);
    *profiles[1].last_mut().unwrap() ^= 1;
    assert_eq!(
        read_snapshot(Cursor::new(raw_record_snapshot(4, &profiles)), |_| Ok(())).unwrap_err(),
        SnapshotError::InvalidEncoding
    );
}

#[test]
fn unified_schema_five_combines_both_collections_without_legacy_tag_guessing() {
    let mut records = Vec::new();
    for fixture in [LEGACY_FOUR_DRIVE, LEGACY_FOUR_PROFILES] {
        read_snapshot(Cursor::new(hex::decode(fixture).unwrap()), |record| {
            records.push(record);
            Ok(())
        })
        .unwrap();
    }
    let mut header = header();
    header.schema_version = SNAPSHOT_SCHEMA_VERSION;
    header.record_count = records.len() as u64;
    let mut bytes = Vec::new();
    write_snapshot(&mut bytes, &header, records.clone()).unwrap();
    let mut restored = Vec::new();
    read_snapshot(Cursor::new(bytes), |record| {
        restored.push(record);
        Ok(())
    })
    .unwrap();
    assert_eq!(restored, records);
    assert!(
        read_snapshot(
            Cursor::new(raw_record_snapshot(
                5,
                &frozen_records(LEGACY_FOUR_PROFILES)
            )),
            |_| Ok(())
        )
        .is_err()
    );
    for version in 1..=3 {
        let bytes = raw_record_snapshot(version, &[bincode::serialize(&received()).unwrap()]);
        assert!(read_snapshot(Cursor::new(bytes), |_| Ok(())).is_ok());
    }
    let bytes = raw_record_snapshot(4, &[bincode::serialize(&received()).unwrap()]);
    assert!(read_snapshot(Cursor::new(bytes), |_| Ok(())).is_ok());
}

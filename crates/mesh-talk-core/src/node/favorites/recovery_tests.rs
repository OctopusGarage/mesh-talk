use super::*;
use crate::recovery::{
    archive,
    key::RecoveryKey,
    snapshot::{SnapshotRecordV1, read_snapshot},
};
use std::io::Cursor;

#[test]
fn recovery_preserves_tombstones_and_projects_expiry_with_card_only_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let account = crate::identity::account::Account::generate();
    let node = super::capture_tests::open(&dir.path().join("source"), &account);
    let selected = super::capture_tests::source(&node);
    let mut favorite = node
        .save_message_favorite_guarded(selected.clone(), Default::default(), |c| c())
        .unwrap();
    let mut single = selected;
    single.message_ids.truncate(1);
    let removed = node
        .save_message_favorite_guarded(single, Default::default(), |c| c())
        .unwrap();
    let stale = node.favorites.lock().unwrap().current[&removed.id].clone();
    node.remove_message_favorite_guarded(&removed.id, removed.revision, &removed.device_id, |c| {
        c()
    })
    .unwrap();
    for (index, message) in favorite.messages.iter_mut().enumerate() {
        message.file = Some(FavoriteFile {
            file_conv: hex::encode([80 + index as u8; 32]),
            name: "retained.bin".into(),
            size: 5,
            mime: "application/octet-stream".into(),
            media: false,
        });
    }
    favorite.messages[0].expires_at_ms = Some(1);
    favorite.messages[0].sticker = Some("secret-sticker".into());
    favorite.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: favorite.clone(),
            removed: false,
        })
        .unwrap();
    node.favorite_media
        .store_bytes(ConversationId::new([80; 32]), "expired.bin", b"bytes")
        .unwrap();
    node.favorite_media
        .store_bytes(ConversationId::new([82; 32]), "orphan.bin", b"bytes")
        .unwrap();
    node.clear_account_conversation(&favorite.source.target)
        .unwrap();
    let key = RecoveryKey::generate();
    let mut encrypted = Vec::new();
    node.write_encrypted_recovery_archive(&mut encrypted, &key, now_millis())
        .unwrap();
    let mut plaintext = Vec::new();
    archive::read_archive(Cursor::new(&encrypted), &mut plaintext, key.as_bytes()).unwrap();
    let mut payloads = Vec::new();
    read_snapshot(Cursor::new(&plaintext), |record| {
        match record {
            SnapshotRecordV1::Favorite { payload } => payloads.push(payload),
            SnapshotRecordV1::FavoriteMediaDescriptor { .. } => {
                panic!("expired or orphan bytes exported")
            }
            _ => {}
        }
        Ok(())
    })
    .unwrap();
    let tombstone: FavoriteRecord = payloads
        .iter()
        .map(|p| bincode::deserialize::<FavoriteRecord>(p).unwrap())
        .find(|r| r.removed)
        .unwrap();
    assert!(tombstone.favorite.metadata.title.is_empty());
    assert!(
        tombstone
            .favorite
            .messages
            .iter()
            .all(|m| m.text.is_empty() && m.file.is_none() && m.sticker.is_none())
    );
    let portable: FavoriteRecord = bincode::deserialize(
        &payloads
            .into_iter()
            .find(|p| {
                bincode::deserialize::<FavoriteRecord>(p)
                    .unwrap()
                    .favorite
                    .id
                    == favorite.id
            })
            .unwrap(),
    )
    .unwrap();
    assert!(portable.favorite.messages[0].text.is_empty());
    assert!(portable.favorite.messages[0].file.is_none());
    assert!(portable.favorite.messages[0].sticker.is_none());
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(&encrypted), &key, dir.path())
            .unwrap();
    assert_eq!(
        (
            preview.favorites,
            preview.favorite_files,
            preview.missing_favorite_files
        ),
        (1, 0, 1)
    );
    let destination = dir.path().join("target");
    let (restored, _) = Node::install_encrypted_recovery_archive(
        Cursor::new(&encrypted),
        &key,
        dir.path(),
        &destination,
        "new pw",
    )
    .unwrap();
    let restored_items = restored.list_message_favorites("", 0, 10).unwrap().items;
    assert_eq!(restored_items, vec![project(favorite, now_millis())]);
    assert!(!restored.favorite_file_available(ConversationId::new([81; 32])));
    let mut stale_edit = stale;
    stale_edit.favorite.revision = MAX_SAFE_REVISION;
    assert!(
        !restored
            .favorites
            .lock()
            .unwrap()
            .merge(stale_edit)
            .unwrap()
    );
    drop(restored);
    let reopened = Node::open_recovery_profile(&destination, "new pw").unwrap();
    assert!(reopened.favorites.lock().unwrap().current[&removed.id].removed);
    assert_eq!(
        reopened.list_message_favorites("", 0, 10).unwrap().items,
        restored_items
    );
    assert!(
        !reopened
            .favorite_media
            .contains(ConversationId::new([80; 32]))
    );
}

#[test]
fn recovery_many_retained_files_uses_bounded_file_handles() {
    const CHILD: &str = "MESH_TEST_RECOVERY_FD_CHILD";
    if std::env::var_os(CHILD).as_deref() != Some(std::ffi::OsStr::new("1")) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "node::favorites::recovery_tests::recovery_many_retained_files_uses_bounded_file_handles",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("RUST_TEST_THREADS", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated many-file recovery failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let account = crate::identity::account::Account::generate();
    let node = super::capture_tests::open(&dir.path().join("source"), &account);
    let mut expected = Vec::new();
    for group in 0..2u32 {
        let mut messages = Vec::new();
        for member in 0..100u32 {
            let number = group * 100 + member + 1;
            let mut message_id = [47; 32];
            message_id[..4].copy_from_slice(&number.to_be_bytes());
            let mut file_id = [48; 32];
            file_id[..4].copy_from_slice(&number.to_be_bytes());
            let file_conv = ConversationId::new(file_id);
            let bytes = if member % 2 == 0 {
                b"".as_slice()
            } else {
                b"retained bytes".as_slice()
            };
            node.favorite_media
                .store_bytes(file_conv, "retained.bin", bytes)
                .unwrap();
            expected.push((file_conv, bytes));
            messages.push(FavoriteMessage {
                id: hex::encode(message_id),
                who: "you".into(),
                sender_name: "You".into(),
                text: "Standalone retained card".into(),
                wall_clock: number.into(),
                expires_at_ms: None,
                sticker: None,
                file: Some(FavoriteFile {
                    file_conv: hex::encode(file_id),
                    name: "retained.bin".into(),
                    size: bytes.len() as u64,
                    mime: "application/octet-stream".into(),
                    media: false,
                }),
            });
        }
        let source = FavoriteSource {
            kind: "account".into(),
            target: "ab".repeat(16),
            message_ids: messages.iter().map(|m| m.id.clone()).collect(),
        };
        let favorite = MessageFavorite {
            id: super::recovery::canonical_id(&account.account_id(), &source).unwrap(),
            source,
            source_label: "Peer".into(),
            metadata: FavoriteMetadata::default(),
            messages,
            saved_at_ms: now_millis(),
            revision: 1,
            device_id: node.user_id(),
        };
        node.favorites
            .lock()
            .unwrap()
            .merge(FavoriteRecord {
                generation: 1,
                favorite,
                removed: false,
            })
            .unwrap();
    }
    let key = RecoveryKey::generate();
    let mut encrypted = Vec::new();
    node.write_encrypted_recovery_archive(&mut encrypted, &key, now_millis())
        .unwrap();
    drop(node);

    // This exact test runs alone in a fresh subprocess on every platform. Limit
    // only that child's descriptors on Unix; concurrent tests and cargo keep theirs.
    #[cfg(unix)]
    {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: `limit` is writable for getrlimit, and valid for setrlimit.
        // No other tests run in this dedicated child process.
        unsafe {
            assert_eq!(libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit), 0);
            limit.rlim_cur = limit.rlim_cur.min(128);
            assert_eq!(libc::setrlimit(libc::RLIMIT_NOFILE, &limit), 0);
        }
    }
    let destination = dir.path().join("restored");
    let (restored, preview) = Node::install_encrypted_recovery_archive(
        Cursor::new(&encrypted),
        &key,
        dir.path(),
        &destination,
        "new password",
    )
    .unwrap_or_else(|error| {
        assert!(
            !destination.exists(),
            "failed recovery left its unpublished staging directory"
        );
        panic!("many-file encrypted recovery failed with a bounded descriptor budget: {error}");
    });
    assert_eq!((preview.favorites, preview.favorite_files), (2, 200));
    assert_eq!(restored.account_id(), account.account_id());
    assert_eq!(
        restored
            .list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .len(),
        2
    );
    drop(restored);
    let reopened = Node::open_recovery_profile(&destination, "new password").unwrap();
    for (file_conv, bytes) in expected {
        assert_eq!(reopened.read_favorite_file(file_conv).unwrap(), bytes);
    }
}

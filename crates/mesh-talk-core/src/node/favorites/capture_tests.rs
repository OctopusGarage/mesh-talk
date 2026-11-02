use super::*;
use crate::{
    discovery::Roster,
    identity::{account::Account, device::DeviceIdentity},
};
use std::sync::{Arc, Mutex};
pub(super) fn open(dir: &Path, account: &Account) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (ch, _) = tokio::sync::mpsc::unbounded_channel();
    let (file, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open_with_account(
        DeviceIdentity::from_secret_bytes([42; 32], [43; 32]),
        Account::from_secret_bytes(account.secret_bytes()),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        ch,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}
pub(super) fn source(node: &Node) -> FavoriteSource {
    let peer = "ab".repeat(16);
    let conv = super::super::conversation::account_conversation_id(&node.account_id(), &peer);
    for (seq, id, text) in [(1, [1; 32], "first"), (2, [2; 32], "second")] {
        let envelope = super::super::DmEnvelope::new(
            node.account_id(),
            peer.clone(),
            id,
            super::super::MessageBody::new(text.as_bytes().to_vec(), None).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, seq, seq, &envelope.encode())
            .unwrap();
    }
    FavoriteSource {
        kind: "account".into(),
        target: peer,
        message_ids: vec![hex::encode([2; 32]), hex::encode([1; 32])],
    }
}
#[test]
fn canonical_group_is_atomic_idempotent_and_independent_of_history() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let source = source(&node);
    let f = node
        .save_message_favorite_guarded(source.clone(), FavoriteMetadata::default(), |commit| {
            commit()
        })
        .unwrap();
    assert_eq!(
        f.messages
            .iter()
            .map(|m| m.text.as_str())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    let mut overlap = source.clone();
    overlap.message_ids.pop();
    assert_ne!(
        node.save_message_favorite_guarded(overlap, FavoriteMetadata::default(), |c| c())
            .unwrap()
            .id,
        f.id
    );
    assert_eq!(
        node.save_message_favorite_guarded(source.clone(), FavoriteMetadata::default(), |c| c())
            .unwrap()
            .id,
        f.id
    );
    let mut invalid = source;
    invalid.message_ids.push(hex::encode([3; 32]));
    assert!(
        node.save_message_favorite_guarded(invalid, FavoriteMetadata::default(), |c| c())
            .is_err()
    );
    node.clear_account_conversation(&f.source.target).unwrap();
    assert_eq!(
        node.save_message_favorite_guarded(f.source.clone(), Default::default(), |c| c())
            .unwrap()
            .id,
        f.id
    );
    assert_eq!(
        node.list_message_favorites("first", 0, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    let edited = node
        .edit_message_favorite_guarded(
            &f.id,
            f.revision,
            &f.device_id,
            FavoriteMetadata {
                description: "find deployment fix".into(),
                ..Default::default()
            },
            |c| c(),
        )
        .unwrap();
    assert!(
        node.edit_message_favorite_guarded(
            &f.id,
            f.revision,
            &f.device_id,
            FavoriteMetadata::default(),
            |c| c()
        )
        .is_err()
    );
    assert_eq!(
        node.list_message_favorites("deployment", 0, 10)
            .unwrap()
            .items
            .len(),
        1
    );
    node.remove_message_favorite_guarded(&f.id, edited.revision, &edited.device_id, |c| c())
        .unwrap();
    assert!(
        node.list_message_favorites("first", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    drop(node);
    let reopened = open(dir.path(), &account);
    assert!(
        reopened
            .list_message_favorites("first", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
}
#[test]
fn denied_authority_and_other_accounts_cannot_write_or_read_favorites() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let source = source(&node);
    assert!(
        node.save_message_favorite_guarded(source.clone(), FavoriteMetadata::default(), |_| {
            Err(NodeError::Authorization("denied".into()))
        })
        .is_err()
    );
    assert!(
        node.list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    node.save_message_favorite_guarded(source, FavoriteMetadata::default(), |c| c())
        .unwrap();
    drop(node);
    let other = open(&dir.path().join("other"), &Account::generate());
    assert!(
        other
            .list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
}
#[test]
fn inspection_uses_exact_sets_and_excludes_expired_removed_members() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &Account::generate());
    let mut selected = source(&node);
    let mut f = node
        .save_message_favorite_guarded(
            selected.clone(),
            FavoriteMetadata {
                title: "Retained notes".into(),
                ..Default::default()
            },
            |c| c(),
        )
        .unwrap();
    node.clear_account_conversation(&f.source.target).unwrap();
    selected.message_ids.reverse();
    selected.message_ids.push(selected.message_ids[0].clone());
    let inspected = node.inspect_message_favorites(&selected).unwrap();
    assert_eq!(
        inspected.existing.as_ref().unwrap().metadata.title,
        "Retained notes"
    );
    assert_eq!(inspected.saved_message_ids.len(), 2);
    f.messages[0].expires_at_ms = Some(1);
    f.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: f.clone(),
            removed: false,
        })
        .unwrap();
    let inspected = node.inspect_message_favorites(&selected).unwrap();
    assert_eq!(inspected.saved_message_ids, vec![f.messages[1].id.clone()]);
    assert!(inspected.existing.unwrap().messages[0].text.is_empty());
    node.remove_message_favorite_guarded(&f.id, f.revision, &f.device_id, |c| c())
        .unwrap();
    let inspected = node.inspect_message_favorites(&selected).unwrap();
    assert!(inspected.existing.is_none());
    assert!(inspected.saved_message_ids.is_empty());
    selected.message_ids.clear();
    assert!(
        node.inspect_message_favorites(&selected)
            .unwrap()
            .existing
            .is_none()
    );
}
#[test]
fn retained_attachment_export_requires_live_favorite_and_safe_destination() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &Account::generate());
    let selected = source(&node);
    let mut f = node
        .save_message_favorite_guarded(selected, Default::default(), |c| c())
        .unwrap();
    let conv = ConversationId::new([9; 32]);
    f.messages[0].file = Some(FavoriteFile {
        file_conv: hex::encode(conv.as_bytes()),
        name: "note.txt".into(),
        size: 5,
        mime: "text/plain".into(),
        media: false,
    });
    f.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: f.clone(),
            removed: false,
        })
        .unwrap();
    node.favorite_media
        .store_bytes(conv, "note.txt", b"bytes")
        .unwrap();
    assert!(node.favorite_file_available(conv));
    let download = dir.path().join("downloads");
    std::fs::create_dir(&download).unwrap();
    let saved = node.save_favorite_file_into_dir(conv, &download).unwrap();
    assert_eq!(std::fs::read(saved).unwrap(), b"bytes");
    node.remove_message_favorite_guarded(&f.id, f.revision, &f.device_id, |c| c())
        .unwrap();
    assert!(!node.favorite_file_available(conv));
    assert!(!node.favorite_media.contains(conv));
    assert!(node.read_favorite_file(conv).is_none());
    assert!(
        node.save_favorite_file(conv, &download.join("blocked.txt"))
            .is_err()
    );
}

#[test]
fn shared_attachment_references_survive_until_last_expiry_and_startup_prunes_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    let mut f = node
        .save_message_favorite_guarded(selected, Default::default(), |c| c())
        .unwrap();
    let conv = ConversationId::new([12; 32]);
    f.messages[0].file = Some(FavoriteFile {
        file_conv: hex::encode(conv.as_bytes()),
        name: "shared.txt".into(),
        size: 5,
        mime: "text/plain".into(),
        media: false,
    });
    f.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: f.clone(),
            removed: false,
        })
        .unwrap();
    let mut shared = f.clone();
    shared.id = hex::encode([13; 32]);
    shared.source.message_ids = vec![shared.messages[0].id.clone()];
    shared.messages.truncate(1);
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: shared.clone(),
            removed: false,
        })
        .unwrap();
    node.favorite_media
        .store_bytes(conv, "shared.txt", b"bytes")
        .unwrap();
    node.remove_message_favorite_guarded(&f.id, f.revision, &f.device_id, |c| c())
        .unwrap();
    assert!(node.favorite_media.contains(conv));
    shared.messages[0].expires_at_ms = Some(1);
    shared.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: shared,
            removed: false,
        })
        .unwrap();
    node.prune_expired_received().unwrap();
    assert!(!node.favorite_media.contains(conv));
    let orphan = node
        .favorite_media
        .store_bytes(conv, "orphan.txt", b"bytes")
        .unwrap();
    drop(node);
    let node = open(dir.path(), &account);
    // Opening only schedules cleanup. This fixture has no runtime worker, so
    // advance the same bounded owner explicitly. The old and reopened cutoffs
    // each need at most one row page and one empty completion page.
    assert!(orphan.exists());
    assert!(node.favorite_media.gc_pending().unwrap());
    for _ in 0..4 {
        if !node.favorite_media.gc_pending().unwrap() {
            break;
        }
        node.maintain_media().unwrap();
    }
    assert!(!node.favorite_media.gc_pending().unwrap());
    assert!(!node.favorite_media.contains(conv));
    assert!(!orphan.exists());
}

#[test]
fn missing_attachment_queue_filters_existing_files_before_bounding() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &Account::generate());
    let selected = source(&node);
    let base = node
        .save_message_favorite_guarded(selected, Default::default(), |c| c())
        .unwrap();
    for index in 1..=101u8 {
        let mut f = base.clone();
        f.id = hex::encode([index; 32]);
        f.source.message_ids.truncate(1);
        f.messages.truncate(1);
        let conv = ConversationId::new([index; 32]);
        f.messages[0].file = Some(FavoriteFile {
            file_conv: hex::encode(conv.as_bytes()),
            name: "file.txt".into(),
            size: 1,
            mime: "text/plain".into(),
            media: false,
        });
        f.revision += 1;
        node.favorites
            .lock()
            .unwrap()
            .merge(FavoriteRecord {
                generation: 1,
                favorite: f,
                removed: false,
            })
            .unwrap();
        if index <= 100 {
            node.favorite_media
                .store_bytes(conv, "file.txt", b"x")
                .unwrap();
        }
    }
    assert_eq!(
        node.missing_favorite_files(),
        vec![ConversationId::new([101; 32])]
    );
}

#[test]
fn oversized_group_capture_is_atomic_even_when_each_message_fits() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let peer = "ab".repeat(16);
    let conv = super::super::conversation::account_conversation_id(&node.account_id(), &peer);
    // Each message fits the snapshot budget; their selected group does not.
    let text = vec![b'x'; 140 * 1024];
    let ids: Vec<_> = (1..=2u8)
        .map(|index| {
            let id = [index; 32];
            let envelope = super::super::DmEnvelope::new(
                node.account_id(),
                peer.clone(),
                id,
                super::super::MessageBody::new(text.clone(), None).encode(),
            );
            node.sentlog
                .lock()
                .unwrap()
                .record(conv, index.into(), index.into(), &envelope.encode())
                .unwrap();
            hex::encode(id)
        })
        .collect();
    let selected = FavoriteSource {
        kind: "account".into(),
        target: peer,
        message_ids: ids,
    };
    assert!(
        node.save_message_favorite_guarded(selected.clone(), Default::default(), |c| c())
            .is_err()
    );
    assert!(
        node.list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    drop(node);
    let node = open(dir.path(), &account);
    assert!(
        node.list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    for id in &selected.message_ids {
        let mut single = selected.clone();
        single.message_ids = vec![id.clone()];
        let favorite = node
            .save_message_favorite_guarded(single, Default::default(), |c| c())
            .unwrap();
        assert_eq!(favorite.messages.len(), 1);
        assert_eq!(favorite.messages[0].text.as_bytes(), text);
    }
    assert_eq!(
        node.list_message_favorites("", 0, 10).unwrap().items.len(),
        2
    );
}

#[test]
fn rejected_metadata_edits_preserve_the_complete_durable_favorite() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    let metadata = FavoriteMetadata {
        title: "Keep title".into(),
        description: "Keep description".into(),
        notes: BTreeMap::from([(selected.message_ids[0].clone(), "Keep note".into())]),
    };
    let favorite = node
        .save_message_favorite_guarded(selected, metadata, |c| c())
        .unwrap();
    let mut oversized = favorite.metadata.clone();
    oversized.description = "界".repeat(4001);
    assert!(matches!(
        node.edit_message_favorite_guarded(
            &favorite.id,
            favorite.revision,
            &favorite.device_id,
            oversized,
            |c| c()
        ),
        Err(NodeError::InvalidInput(_))
    ));
    let mut foreign_note = favorite.metadata.clone();
    foreign_note.notes = BTreeMap::from([(hex::encode([99; 32]), "Not selected".into())]);
    assert!(matches!(
        node.edit_message_favorite_guarded(
            &favorite.id,
            favorite.revision,
            &favorite.device_id,
            foreign_note,
            |c| c()
        ),
        Err(NodeError::InvalidInput(_))
    ));
    assert_eq!(
        node.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite.clone()]
    );
    drop(node);
    let reopened = open(dir.path(), &account);
    assert_eq!(
        reopened.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite]
    );
}

#[test]
fn stale_and_denied_mutations_cannot_overwrite_or_remove_the_latest_durable_value() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let selected = source(&node);
    let initial = node
        .save_message_favorite_guarded(selected, Default::default(), |c| c())
        .unwrap();
    let latest = node
        .edit_message_favorite_guarded(
            &initial.id,
            initial.revision,
            &initial.device_id,
            FavoriteMetadata {
                title: "Latest accepted title".into(),
                ..Default::default()
            },
            |c| c(),
        )
        .unwrap();
    assert!(
        node.edit_message_favorite_guarded(
            &initial.id,
            initial.revision,
            &initial.device_id,
            Default::default(),
            |c| c()
        )
        .is_err()
    );
    assert!(
        node.remove_message_favorite_guarded(
            &initial.id,
            initial.revision,
            &initial.device_id,
            |c| c()
        )
        .is_err()
    );
    // A matching revision alone is insufficient: concurrent device identity is
    // part of the optimistic edit/removal token.
    let other_device = "00".repeat(16);
    assert_ne!(latest.device_id, other_device);
    assert!(
        node.edit_message_favorite_guarded(
            &latest.id,
            latest.revision,
            &other_device,
            Default::default(),
            |c| c()
        )
        .is_err()
    );
    assert!(
        node.remove_message_favorite_guarded(&latest.id, latest.revision, &other_device, |c| c())
            .is_err()
    );
    assert!(matches!(
        node.edit_message_favorite_guarded(
            &latest.id,
            latest.revision,
            &latest.device_id,
            Default::default(),
            |_| Err(NodeError::Authorization("retired".into()))
        ),
        Err(NodeError::Authorization(_))
    ));
    assert!(matches!(
        node.remove_message_favorite_guarded(
            &latest.id,
            latest.revision,
            &latest.device_id,
            |_| Err(NodeError::Authorization("retired".into()))
        ),
        Err(NodeError::Authorization(_))
    ));
    assert_eq!(
        node.list_message_favorites("", 0, 10).unwrap().items,
        vec![latest.clone()]
    );
    drop(node);
    let reopened = open(dir.path(), &account);
    assert_eq!(
        reopened.list_message_favorites("", 0, 10).unwrap().items,
        vec![latest]
    );
}

#[test]
fn encrypted_recovery_preserves_favorites_after_source_history_is_cleared() {
    use crate::recovery::key::RecoveryKey;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(&dir.path().join("source"), &account);
    let selected = source(&node);
    let metadata = FavoriteMetadata {
        title: "部署笔记 🌍".into(),
        description: "Retain this independently of chat history".into(),
        notes: BTreeMap::from([(selected.message_ids[0].clone(), "確認済み ✅".into())]),
    };
    let mut favorite = node
        .save_message_favorite_guarded(selected, metadata, |c| c())
        .unwrap();
    for (index, (name, mime, media, bytes)) in [
        (
            "说明.txt",
            "text/plain",
            false,
            b"retained generic".as_slice(),
        ),
        ("photo.png", "image/png", true, b"retained image".as_slice()),
    ]
    .into_iter()
    .enumerate()
    {
        let conv = ConversationId::new([70 + index as u8; 32]);
        favorite.messages[index].file = Some(FavoriteFile {
            file_conv: hex::encode(conv.as_bytes()),
            name: name.into(),
            size: bytes.len() as u64,
            mime: mime.into(),
            media,
        });
        node.favorite_media.store_bytes(conv, name, bytes).unwrap();
    }
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
    node.clear_account_conversation(&favorite.source.target)
        .unwrap();
    let original_device = node.user_id();
    let key = RecoveryKey::generate();
    let mut archive = Vec::new();
    node.write_encrypted_recovery_archive(&mut archive, &key, now_millis())
        .unwrap();
    let preview =
        Node::inspect_encrypted_recovery_archive(Cursor::new(&archive), &key, dir.path()).unwrap();
    assert_eq!(preview.messages, 0);
    let destination = dir.path().join("restored");
    let (restored, _) = Node::install_encrypted_recovery_archive(
        Cursor::new(&archive),
        &key,
        dir.path(),
        &destination,
        "fresh password",
    )
    .unwrap();
    assert_eq!(restored.account_id(), account.account_id());
    assert_ne!(restored.user_id(), original_device);
    assert_eq!(
        restored.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite.clone()]
    );
    for (index, expected) in [b"retained generic".as_slice(), b"retained image".as_slice()]
        .into_iter()
        .enumerate()
    {
        let bytes = restored
            .read_favorite_file(ConversationId::new([70 + index as u8; 32]))
            .unwrap();
        assert_eq!(Sha256::digest(&bytes), Sha256::digest(expected));
    }
    drop(restored);
    let reopened = Node::open_recovery_profile(&destination, "fresh password").unwrap();
    assert_eq!(
        reopened.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite.clone()]
    );
    assert!(Node::open_recovery_profile(&destination, "pw").is_err());
    let mut second_archive = Vec::new();
    reopened
        .write_encrypted_recovery_archive(&mut second_archive, &key, now_millis())
        .unwrap();
    let (second, preview) = Node::install_encrypted_recovery_archive(
        Cursor::new(&second_archive),
        &key,
        dir.path(),
        &dir.path().join("second-restore"),
        "third password",
    )
    .unwrap();
    assert_eq!(preview.favorites, 1);
    assert_eq!(preview.favorite_files, 2);
    assert_eq!(second.account_id(), account.account_id());
    assert_ne!(second.user_id(), reopened.user_id());
    assert_eq!(
        second.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite]
    );
    for (index, expected) in [b"retained generic".as_slice(), b"retained image".as_slice()]
        .into_iter()
        .enumerate()
    {
        let bytes = second
            .read_favorite_file(ConversationId::new([70 + index as u8; 32]))
            .unwrap();
        assert_eq!(Sha256::digest(&bytes), Sha256::digest(expected));
    }
}

#[tokio::test]
async fn protected_text_and_images_cannot_be_saved_in_mixed_favorites() {
    for mode in [
        super::super::MessageProtection::Click,
        super::super::MessageProtection::Authenticate,
    ] {
        let dir = tempfile::tempdir().unwrap();
        let node = open(dir.path(), &Account::generate());
        let mut selected = source(&node);
        let conv = super::super::conversation::account_conversation_id(
            &node.account_id(),
            &selected.target,
        );
        let id = [3; 32];
        let envelope = super::super::DmEnvelope::new(
            node.account_id(),
            selected.target.clone(),
            id,
            super::super::MessageBody::protected(b"private text".to_vec(), None, mode).encode(),
        );
        node.sentlog
            .lock()
            .unwrap()
            .record(conv, 3, 3, &envelope.encode())
            .unwrap();
        let mut single_text = selected.clone();
        single_text.message_ids = vec![hex::encode(id)];
        assert!(
            node.save_message_favorite_guarded(single_text, Default::default(), |c| c())
                .is_err()
        );
        assert!(node.favorites.lock().unwrap().current.is_empty());
        selected.message_ids.push(hex::encode(id));
        assert!(
            node.save_message_favorite_guarded(selected, Default::default(), |c| c())
                .is_err()
        );
        assert!(
            node.list_message_favorites("", 0, 10)
                .unwrap()
                .items
                .is_empty()
        );
        let channel = node.create_channel("private", vec![]).await.unwrap();
        let ordinary = node
            .enqueue_channel_message_reply_expiring_if(channel, b"ordinary", None, None, |c| c())
            .unwrap();
        let path = dir.path().join("private.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\nprivate pixels").unwrap();
        let (image, file) = node
            .enqueue_protected_file_channel_progress_expiring_if(
                channel,
                &path,
                crate::file::FileKind::Media,
                |_| {},
                None,
                mode,
                |c| c(),
            )
            .await
            .unwrap();
        let selection = FavoriteSource {
            kind: "channel".into(),
            target: hex::encode(channel.as_bytes()),
            message_ids: vec![ordinary.to_hex(), image.to_hex()],
        };
        let mut single_image = selection.clone();
        single_image.message_ids = vec![image.to_hex()];
        assert!(
            node.save_message_favorite_guarded(single_image, Default::default(), |c| c())
                .is_err()
        );
        assert!(node.favorites.lock().unwrap().current.is_empty());
        assert!(!node.favorite_media.contains(file));
        assert!(
            node.save_message_favorite_guarded(selection.clone(), Default::default(), |c| c())
                .is_err()
        );
        assert!(
            node.list_message_favorites("", 0, 10)
                .unwrap()
                .items
                .is_empty()
        );
        assert!(!node.favorite_media.contains(file));
        assert!(node.favorites.lock().unwrap().current.is_empty());
        let mut single_ordinary = selection;
        single_ordinary.message_ids = vec![ordinary.to_hex()];
        let favorite = node
            .save_message_favorite_guarded(single_ordinary, Default::default(), |c| c())
            .unwrap();
        assert_eq!(favorite.messages.len(), 1);
        assert_eq!(favorite.messages[0].id, ordinary.to_hex());
        assert_eq!(favorite.messages[0].text, "ordinary");
        assert!(favorite.messages[0].file.is_none());
        assert_eq!(node.favorites.lock().unwrap().current.len(), 1);
        assert!(!node.favorite_media.contains(file));
    }
}

#[tokio::test]
async fn existing_favorite_cannot_expose_a_canonical_protected_image() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let ordinary = source(&node);
    let mut record = node
        .save_message_favorite_guarded(ordinary, Default::default(), |c| c())
        .unwrap();
    let channel = node.create_channel("private", vec![]).await.unwrap();
    let path = dir.path().join("private.png");
    let bytes = b"\x89PNG\r\n\x1a\nprivate pixels";
    std::fs::write(&path, bytes).unwrap();
    let (id, file) = node
        .enqueue_protected_file_channel_progress_expiring_if(
            channel,
            &path,
            crate::file::FileKind::Media,
            |_| {},
            None,
            super::super::MessageProtection::Click,
            |c| c(),
        )
        .await
        .unwrap();
    record.source = FavoriteSource {
        kind: "channel".into(),
        target: hex::encode(channel.as_bytes()),
        message_ids: vec![id.to_hex()],
    };
    record.messages.truncate(1);
    record.messages[0].id = id.to_hex();
    record.messages[0].text = "private imported content".into();
    record.messages[0].file = Some(FavoriteFile {
        file_conv: hex::encode(file.as_bytes()),
        name: "private.png".into(),
        size: bytes.len() as u64,
        mime: "image/png".into(),
        media: true,
    });
    record.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: record.clone(),
            removed: false,
        })
        .unwrap();
    // Model a retained copy written before the current protection-aware client.
    node.favorite_media
        .store_bytes(file, "private.png", bytes)
        .unwrap();
    assert!(
        node.list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(
        node.inspect_message_favorites(&record.source)
            .unwrap()
            .existing
            .is_none()
    );
    assert!(!node.favorite_file_available(file));
    assert!(node.read_favorite_file(file).is_none());
    assert!(
        node.save_favorite_file(file, &dir.path().join("export.png"))
            .is_err()
    );
    assert!(!dir.path().join("export.png").exists());
    assert!(
        node.edit_message_favorite_guarded(
            &record.id,
            record.revision,
            &record.device_id,
            Default::default(),
            |c| c()
        )
        .is_err()
    );
    drop(node);
    let reopened = open(dir.path(), &account);
    assert!(
        reopened
            .list_message_favorites("", 0, 10)
            .unwrap()
            .items
            .is_empty()
    );
    assert!(reopened.read_favorite_file(file).is_none());
}

#[tokio::test]
async fn encrypted_recovery_excludes_canonical_protected_favorites() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(dir.path(), &account);
    let ordinary = source(&node);
    let ordinary_target = ordinary.target.clone();
    let mut record = node
        .save_message_favorite_guarded(ordinary, Default::default(), |c| c())
        .unwrap();
    let ordinary_file = ConversationId::new([74; 32]);
    let ordinary_bytes = b"ordinary independently retained bytes";
    record.messages[0].file = Some(FavoriteFile {
        file_conv: hex::encode(ordinary_file.as_bytes()),
        name: "ordinary.txt".into(),
        size: ordinary_bytes.len() as u64,
        mime: "text/plain".into(),
        media: false,
    });
    record.revision += 1;
    node.favorite_media
        .store_bytes(ordinary_file, "ordinary.txt", ordinary_bytes)
        .unwrap();
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: record.clone(),
            removed: false,
        })
        .unwrap();
    let channel = node.create_channel("private", vec![]).await.unwrap();
    let path = dir.path().join("private.png");
    let bytes = b"\x89PNG\r\n\x1a\nprivate pixels";
    std::fs::write(&path, bytes).unwrap();
    let (id, file) = node
        .enqueue_protected_file_channel_progress_expiring_if(
            channel,
            &path,
            crate::file::FileKind::Media,
            |_| {},
            None,
            super::super::MessageProtection::Click,
            |c| c(),
        )
        .await
        .unwrap();
    record.source = FavoriteSource {
        kind: "channel".into(),
        target: hex::encode(channel.as_bytes()),
        message_ids: vec![id.to_hex()],
    };
    record.messages.truncate(1);
    record.messages[0].id = id.to_hex();
    record.messages[0].text = "private imported content".into();
    record.messages[0].file = Some(FavoriteFile {
        file_conv: hex::encode(file.as_bytes()),
        name: "private.png".into(),
        size: bytes.len() as u64,
        mime: "image/png".into(),
        media: true,
    });
    record.id = super::recovery::canonical_id(&node.account_id(), &record.source).unwrap();
    record.metadata.title = "protected metadata sentinel".into();
    record.revision += 1;
    node.favorites
        .lock()
        .unwrap()
        .merge(FavoriteRecord {
            generation: 1,
            favorite: record.clone(),
            removed: false,
        })
        .unwrap();
    // Model a retained copy written before the current protection-aware client.
    node.favorite_media
        .store_bytes(file, "private.png", bytes)
        .unwrap();
    node.clear_account_conversation(&ordinary_target).unwrap();
    let key = crate::recovery::key::RecoveryKey::generate();
    let mut current = node;
    for generation in 0..2 {
        let mut snapshot = Vec::new();
        current
            .write_recovery_snapshot(&mut snapshot, now_millis())
            .unwrap();
        let mut exported = Vec::new();
        crate::recovery::snapshot::read_snapshot(std::io::Cursor::new(snapshot), |row| {
            if let crate::recovery::snapshot::SnapshotRecordV1::Favorite { payload } = row {
                exported.push(bincode::deserialize::<FavoriteRecord>(&payload).unwrap());
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(
            exported.len(),
            1,
            "protected Favorite must not enter snapshot"
        );
        assert_ne!(exported[0].favorite.id, record.id);
        let mut archive = Vec::new();
        current
            .write_encrypted_recovery_archive(&mut archive, &key, now_millis())
            .unwrap();
        let destination = dir.path().join(format!("restore-{generation}"));
        let (restored, preview) = Node::install_encrypted_recovery_archive(
            std::io::Cursor::new(archive),
            &key,
            dir.path(),
            &destination,
            "fresh-password",
        )
        .unwrap();
        assert_eq!(preview.favorites, 1);
        assert_eq!(preview.favorite_files, 1);
        assert_eq!(
            restored.read_favorite_file(ordinary_file).unwrap(),
            ordinary_bytes
        );
        assert!(!restored.favorite_media.contains(file));
        restored.clear_conversation(channel).unwrap();
        assert_eq!(
            restored
                .list_message_favorites("", 0, 10)
                .unwrap()
                .items
                .len(),
            1
        );
        drop(restored);
        current = Node::open_recovery_profile(&destination, "fresh-password").unwrap();
    }
}

#[test]
fn favorite_search_matches_sticker_presentation_and_respects_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), &Account::generate());
    let mut favorite = node
        .save_message_favorite_guarded(source(&node), Default::default(), |c| c())
        .unwrap();
    let v2 = "mt-sticker-v2-cGFjazpjYXRzOmNhbG0K8J-ZggrpnZnkuIvlv4M.png";
    for (name, mime, expiry, matched, hidden) in [
        (v2, "image/png", None, "静下心", "mt-sticker-v2"),
        (v2, "image/png", None, "🙂", "cGFjaz"),
        (
            "mt-sticker-v1-YnVpbHRpbjp3YXZlCvCfkYs.png",
            "image/png",
            None,
            "👋",
            "mt-sticker-v1",
        ),
        ("ordinary-report.png", "image/png", None, "report", "静下心"),
        (v2, "image/jpeg", None, "mt-sticker-v2", "静下心"),
        (v2, "image/png", Some(1), "first", "静下心"),
    ] {
        favorite.messages[1].file = Some(FavoriteFile {
            file_conv: hex::encode([91; 32]),
            name: name.into(),
            size: 100,
            mime: mime.into(),
            media: true,
        });
        favorite.messages[1].expires_at_ms = expiry;
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
        assert_eq!(
            node.list_message_favorites(matched, 0, 10)
                .unwrap()
                .items
                .len(),
            1,
            "{matched}"
        );
        assert!(
            node.list_message_favorites(hidden, 0, 10)
                .unwrap()
                .items
                .is_empty(),
            "{hidden}"
        );
    }
}

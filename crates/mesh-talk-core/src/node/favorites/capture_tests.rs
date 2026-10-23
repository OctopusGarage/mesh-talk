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
fn source(node: &Node) -> FavoriteSource {
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
    node.favorite_media
        .store_bytes(conv, "orphan.txt", b"bytes")
        .unwrap();
    drop(node);
    let node = open(dir.path(), &account);
    assert!(!node.favorite_media.contains(conv));
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

use super::*;
#[test]
fn durable_tombstone_rejects_offline_resurrection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("favorites.log");
    let mut store = FavoriteStore::open(&path, "pw").unwrap();
    let favorite = fixture();
    store
        .merge(FavoriteRecord {
            generation: 1,
            favorite: favorite.clone(),
            removed: false,
        })
        .unwrap();
    let mut removed = favorite.clone();
    removed.revision += 1;
    store
        .merge(FavoriteRecord {
            generation: 1,
            favorite: removed,
            removed: true,
        })
        .unwrap();
    assert!(
        !store
            .merge(FavoriteRecord {
                generation: 1,
                favorite,
                removed: false
            })
            .unwrap()
    );
    drop(store);
    let store = FavoriteStore::open(&path, "pw").unwrap();
    assert!(store.current.values().all(|r| r.removed));
    assert!(
        !std::fs::read(path)
            .unwrap()
            .windows(6)
            .any(|s| s == b"secret")
    );
}
fn fixture() -> MessageFavorite {
    MessageFavorite {
        id: "ab".repeat(32),
        source: FavoriteSource {
            kind: "account".into(),
            target: "ab".repeat(16),
            message_ids: vec!["cd".repeat(32)],
        },
        source_label: "Alice".into(),
        metadata: FavoriteMetadata::default(),
        messages: vec![FavoriteMessage {
            id: "cd".repeat(32),
            who: "ab".repeat(16),
            sender_name: "Alice".into(),
            text: "secret".into(),
            wall_clock: 1,
            expires_at_ms: None,
            sticker: None,
            file: None,
        }],
        saved_at_ms: 1,
        revision: 1,
        device_id: "ef".repeat(16),
    }
}
#[test]
fn expiry_projects_unavailable_without_erasing_notes() {
    let mut f = fixture();
    f.messages[0].expires_at_ms = Some(10);
    f.metadata.description = "remember this".into();
    let f = project(f, 10);
    assert!(f.messages[0].text.is_empty());
    assert_eq!(f.metadata.description, "remember this");
}

#[test]
fn safe_revision_exhaustion_rejects_edit_and_tombstone_wins_equal_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = FavoriteStore::open(&dir.path().join("favorites.log"), "pw").unwrap();
    let mut f = fixture();
    f.revision = MAX_SAFE_REVISION;
    f.device_id = "ff".repeat(16);
    assert!(
        store
            .merge(FavoriteRecord {
                generation: 1,
                favorite: f.clone(),
                removed: false
            })
            .unwrap()
    );
    assert!(store.next_revision().is_err());
    f.device_id = "00".repeat(16);
    assert!(
        store
            .merge(FavoriteRecord {
                generation: 1,
                favorite: f.clone(),
                removed: true
            })
            .unwrap()
    );
    f.revision += 1;
    assert!(
        !FavoriteRecord {
            generation: 1,
            favorite: f,
            removed: false
        }
        .valid()
    );
}

#[test]
fn metadata_limits_count_unicode_scalars_and_notes_must_reference_selection() {
    let f = fixture();
    let mut metadata = FavoriteMetadata {
        title: "你".repeat(200),
        description: String::new(),
        notes: BTreeMap::new(),
    };
    assert!(valid_metadata(&metadata, &f.source));
    metadata.title.push('你');
    assert!(!valid_metadata(&metadata, &f.source));
    metadata.title.clear();
    metadata.notes.insert("unknown".into(), "note".into());
    assert!(!valid_metadata(&metadata, &f.source));
}

#[test]
fn removal_wins_clock_ahead_offline_edit_until_explicit_resave() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = FavoriteStore::open(&dir.path().join("favorites.log"), "pw").unwrap();
    let mut removed = fixture();
    removed.revision = 2;
    store
        .merge(FavoriteRecord {
            generation: 1,
            favorite: removed,
            removed: true,
        })
        .unwrap();
    let mut offline = fixture();
    offline.revision = MAX_SAFE_REVISION - 1;
    assert!(
        !store
            .merge(FavoriteRecord {
                generation: 1,
                favorite: offline,
                removed: false
            })
            .unwrap()
    );
    let mut explicit = fixture();
    explicit.revision = 3;
    assert!(
        store
            .merge(FavoriteRecord {
                generation: 2,
                favorite: explicit,
                removed: false
            })
            .unwrap()
    );
    assert!(store.current.values().all(|r| !r.removed));
}

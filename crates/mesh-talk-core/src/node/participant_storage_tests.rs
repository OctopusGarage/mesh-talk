use super::*;

#[test]
fn participant_constructor_retains_exclusive_profile_ownership() {
    fn open(root: &Path) -> Result<Arc<Node>, LogError> {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
    }
    let directory = tempfile::tempdir().unwrap();
    let first = open(directory.path()).unwrap();
    assert!(
        open(directory.path()).is_err(),
        "a second Node owned the same profile"
    );
    let retained = first.clone();
    drop(first);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_err());
    drop(retained);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok());
}

#[test]
fn participant_reopen_rejects_unprepared_account_identity() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let first_account = Account::generate();
    let first_secret = first_account.secret_bytes();
    let open = |identity, account| {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &directory.path().join("messages.log"),
            &directory.path().join("sent.log"),
            "pw",
        )
    };
    drop(open(identity, first_account).unwrap());
    assert!(
        open(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Account::generate()
        )
        .is_err(),
        "unprepared account change was admitted"
    );
    assert!(
        open(
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            Account::from_secret_bytes(first_secret)
        )
        .is_ok()
    );
}

#[test]
fn participant_constructor_owns_exact_external_sent_parent() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &directory.path().join("custom.events"),
        &external.path().join("explicit.sent"),
        "pw",
    )
    .unwrap();
    assert!(external.path().join("explicit.sent").exists());
    assert!(!directory.path().join("sent.log").exists());
    assert!(
        crate::storage::profile::ProfileStorageLease::acquire(external.path()).is_err(),
        "external source root was not owned"
    );
    drop(node);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(external.path()).is_ok());
}

#[test]
fn failed_legacy_bootstrap_never_publishes_participant_authority() {
    for failure in ["password", "account", "corrupt"] {
        let directory = tempfile::tempdir().unwrap();
        let log_path = directory.path().join("messages.log");
        let identity = DeviceIdentity::generate();
        let secret = identity.secret_bytes();
        let account = Account::generate();
        let account_secret = account.secret_bytes();
        {
            let mut log = PersistentEventLog::open(&log_path, "pw").unwrap();
            log.append(Event::new(
                &identity,
                ConversationId::new([87; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Profile,
                b"existing ciphertext".to_vec(),
            ))
            .unwrap();
            let mut delivery =
                super::super::delivery_store::DeliveryStore::open_for_log(&log_path, "pw").unwrap();
            delivery
                .bind_profile(&log_path, "pw", &identity.public(), &account.account_id())
                .unwrap();
        }
        if failure == "corrupt" {
            std::fs::write(&log_path, b"invalid legacy authority").unwrap();
        }
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        let result = Node::open_with_account(
            identity,
            if failure == "account" {
                Account::generate()
            } else {
                account
            },
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &log_path,
            &directory.path().join("sent.log"),
            if failure == "password" {
                "wrong password"
            } else {
                "pw"
            },
        );
        assert!(result.is_err(), "invalid legacy {failure} was admitted");
        assert!(
            !directory.path().join("profile.db").exists(),
            "failed {failure} bootstrap published SQL authority"
        );
        if failure != "corrupt" {
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            assert!(
                Node::open_with_account(
                    DeviceIdentity::from_secret_bytes(secret.0, secret.1),
                    Account::from_secret_bytes(account_secret),
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &log_path,
                    &directory.path().join("sent.log"),
                    "pw"
                )
                .is_ok(),
                "correct retry was poisoned by failed {failure} bootstrap"
            );
        }
    }
}

fn open_profile(
    root: &Path,
    identity: DeviceIdentity,
    account: Account,
) -> Result<Arc<Node>, LogError> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &root.join("messages.log"),
        &root.join("sent.log"),
        "pw",
    )
}

#[test]
fn legacy_exact_prepared_account_adoption_bootstraps_control_metadata() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let old = Account::generate();
    let next = Account::generate();
    let log = directory.path().join("messages.log");
    {
        let mut delivery =
            super::super::delivery_store::DeliveryStore::open_for_log(&log, "pw").unwrap();
        delivery
            .bind_profile(&log, "pw", &identity.public(), &old.account_id())
            .unwrap();
        delivery
            .prepare_profile_adoption(&next.account_id())
            .unwrap();
    }
    let old_account = old.account_id();
    let node = open_profile(directory.path(), identity, next).unwrap();
    let retained: bool =
        node.storage
            .database()
            .read(|connection| {
                Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1)",
        [&old_account], |row| row.get(0))?)
            })
            .unwrap();
    assert!(retained, "legacy previous account namespace was lost");
}

#[test]
fn legacy_security_rekey_then_signed_link_retains_known_namespace_across_reopens() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let original = Account::generate();
    let original_public = original.public();
    let rekeyed = Account::generate();
    let rekeyed_public = rekeyed.public();
    let log = directory.path().join("messages.log");
    // Old binaries prepare security rekey without a signed account transition.
    {
        let mut delivery =
            super::super::delivery_store::DeliveryStore::open_for_log(&log, "pw").unwrap();
        delivery
            .bind_profile(&log, "pw", &identity.public(), &original.account_id())
            .unwrap();
        delivery
            .prepare_profile_adoption(&rekeyed.account_id())
            .unwrap();
    }
    let node = open_profile(directory.path(), identity, rekeyed).unwrap();
    let account_path = directory.path().join("account.keystore");
    crate::identity::account_keystore::save(&account_path, "pw", &node.account).unwrap();
    // This is the first signed transition B -> A, not a signed A -> B -> A cycle.
    node.prepare_account_transition(&original).unwrap();
    node.persist_account_adoption(
        &original_public,
        || {
            crate::identity::account_keystore::save(&account_path, "pw", &original)
                .map_err(|error| NodeError::Channel(error.to_string()))
        },
        || {
            crate::identity::account_keystore::load(&account_path, "pw")
                .map(|account| account.public())
                .map_err(|error| NodeError::Channel(error.to_string()))
        },
    )
    .unwrap();
    drop(node);
    for reopen in 0..2 {
        let node = open_profile(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            crate::identity::account_keystore::load(&account_path, "pw").unwrap(),
        )
        .unwrap();
        node.storage
            .database()
            .read(|connection| {
                for public in [&original_public, &rekeyed_public] {
                    let stored: Option<Vec<u8>> = connection.query_row(
                        "SELECT account FROM participant_account_namespaces WHERE account_id=?1",
                        [public.account_id()],
                        |row| row.get(0),
                    )?;
                    assert_eq!(
                        stored.as_deref(),
                        Some(public.ed25519_pub.as_slice()),
                        "reopen {reopen} lost namespace key"
                    );
                }
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn adoption_namespace_upgrade_and_promotion_are_atomic_and_reject_conflicts() {
    for failure in ["promotion", "conflicting-key"] {
        let directory = tempfile::tempdir().unwrap();
        let node = open_profile(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
        )
        .unwrap();
        let active = node.account.public();
        let next = Account::generate().public();
        let conflicting = Account::generate().public();
        let database = node.storage.database();
        database
            .transaction(|tx| {
                tx.execute(
                    "INSERT INTO participant_account_namespaces VALUES(?1,NULL)",
                    [next.account_id()],
                )?;
                if failure == "conflicting-key" {
                    tx.execute(
                        "UPDATE participant_account_namespaces SET account=?1 WHERE account_id=?2",
                        rusqlite::params![conflicting.ed25519_pub.as_slice(), next.account_id()],
                    )?;
                }
                Ok(())
            })
            .unwrap();
        crate::storage::profile::participant::prepare_adoption(database, &active, &next).unwrap();
        if failure == "promotion" {
            database.transaction(|tx| {
                tx.execute_batch("CREATE TEMP TRIGGER fail_promotion BEFORE UPDATE OF account ON participant_profile BEGIN SELECT RAISE(ABORT,'injected promotion failure'); END;")?;
                Ok(())
            }).unwrap();
        }
        assert!(
            crate::storage::profile::participant::finish_adoption(database, &next).is_err(),
            "{failure} was ignored"
        );
        database
            .read(|connection| {
                let (stored_active, stored_next): (Vec<u8>, Vec<u8>) = connection.query_row(
                    "SELECT account,next_account FROM participant_profile",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(stored_active, active.ed25519_pub);
                assert_eq!(stored_next, next.ed25519_pub);
                let namespace: Option<Vec<u8>> = connection.query_row(
                    "SELECT account FROM participant_account_namespaces WHERE account_id=?1",
                    [next.account_id()],
                    |row| row.get(0),
                )?;
                assert_eq!(
                    namespace.as_deref(),
                    (failure == "conflicting-key").then_some(conflicting.ed25519_pub.as_slice())
                );
                Ok(())
            })
            .unwrap();
        if failure == "promotion" {
            database
                .transaction(|tx| {
                    tx.execute_batch("DROP TRIGGER fail_promotion")?;
                    Ok(())
                })
                .unwrap();
            crate::storage::profile::participant::finish_adoption(database, &next).unwrap();
            crate::storage::profile::participant::finish_adoption(database, &next).unwrap();
            database
                .read(|connection| {
                    let namespace: Vec<u8> = connection.query_row(
                        "SELECT account FROM participant_account_namespaces WHERE account_id=?1",
                        [next.account_id()],
                        |row| row.get(0),
                    )?;
                    assert_eq!(namespace, next.ed25519_pub);
                    Ok(())
                })
                .unwrap();
        }
    }
}

#[test]
fn missing_main_with_orphan_wal_is_never_initialized() {
    let directory = tempfile::tempdir().unwrap();
    let orphan = directory.path().join("profile.db-wal");
    std::fs::write(&orphan, b"unowned existing authority").unwrap();
    assert!(
        open_profile(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate()
        )
        .is_err()
    );
    assert!(!directory.path().join("profile.db").exists());
    assert_eq!(
        std::fs::read(orphan).unwrap(),
        b"unowned existing authority"
    );
}

#[test]
fn device_and_role_mismatch_preserve_known_authority() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    drop(open_profile(directory.path(), identity, account).unwrap());
    let before = std::fs::read(directory.path().join("profile.db")).unwrap();
    assert!(
        open_profile(
            directory.path(),
            DeviceIdentity::generate(),
            Account::from_secret_bytes(account_secret)
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(directory.path().join("profile.db")).unwrap(),
        before
    );
    let lease = crate::storage::profile::ProfileStorageLease::acquire(directory.path()).unwrap();
    assert!(
        crate::storage::profile::context::ProfileStorage::open_relay(
            lease,
            &DeviceIdentity::from_secret_bytes(secret.0, secret.1).public(),
            &directory.path().join("office.log"),
            "pw",
            1
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(directory.path().join("profile.db")).unwrap(),
        before
    );
}

#[test]
fn participant_custom_sources_cannot_alias_account_or_runtime_owners() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    for name in [
        format!("account-devices-{}.log", account.account_id()),
        format!("favorites-{}.log", account.account_id()),
        "privacy.policy".into(),
        format!("messages.{}-reactions.log", account.account_id()),
    ] {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        assert!(
            Node::open_with_account(
                DeviceIdentity::from_secret_bytes(keys.0, keys.1),
                Account::from_secret_bytes(account_key),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &directory.path().join("messages.log"),
                &directory.path().join(&name),
                "pw"
            )
            .is_err(),
            "source alias {name} was admitted"
        );
        assert!(
            !directory.path().join("messages.log").exists(),
            "alias validation started a legacy writer"
        );
        assert!(!directory.path().join("profile.db").exists());
    }
}

#[test]
fn ambiguous_account_save_recovers_exact_loaded_key() {
    for outcome in ["old", "next", "third", "unreadable"] {
        let directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let old = Account::generate();
        let old_key = old.secret_bytes();
        let next = Account::generate();
        let next_key = next.secret_bytes();
        let third = Account::generate();
        let third_key = third.secret_bytes();
        let path = directory.path().join("account.keystore");
        crate::identity::account_keystore::save(&path, "pw", &old).unwrap();
        let node = open_profile(directory.path(), identity, old).unwrap();
        let result: Result<(), NodeError> = node.persist_account_adoption(
            &next.public(),
            || {
                match outcome {
                    "old" => {}
                    "next" => crate::identity::account_keystore::save(&path, "pw", &next).unwrap(),
                    "third" => {
                        crate::identity::account_keystore::save(&path, "pw", &third).unwrap()
                    }
                    _ => std::fs::write(&path, b"invalid account keystore").unwrap(),
                }
                Err(NodeError::Channel("injected ambiguous save failure".into()))
            },
            || {
                crate::identity::account_keystore::load(&path, "pw")
                    .map(|value| value.public())
                    .map_err(|e| NodeError::Channel(e.to_string()))
            },
        );
        assert!(result.is_err());
        drop(node);
        let loaded = match outcome {
            "old" => old_key,
            "next" => next_key,
            "third" => third_key,
            _ => old_key,
        };
        let reopened = open_profile(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(loaded),
        );
        if outcome == "third" {
            assert!(reopened.is_err());
        } else {
            assert!(reopened.is_ok(), "{outcome} recovery failed");
        }
    }
}

#[test]
fn explicit_close_refuses_retained_node_and_facade() {
    let directory = tempfile::tempdir().unwrap();
    let node = open_profile(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
    )
    .unwrap();
    let retained = node.clone();
    assert!(node.close_profile().is_err());
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_err());
    let storage = retained.storage.clone();
    assert!(retained.close_profile().is_err());
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_err());
    drop(storage);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok());
}

#[test]
fn failed_legacy_adoption_bootstrap_retains_previous_namespace_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let old = Account::generate();
    let old_id = old.account_id();
    let next = Account::generate();
    let next_key = next.secret_bytes();
    let log = directory.path().join("messages.log");
    {
        let mut delivery =
            super::super::delivery_store::DeliveryStore::open_for_log(&log, "pw").unwrap();
        delivery
            .bind_profile(&log, "pw", &identity.public(), &old_id)
            .unwrap();
        delivery
            .prepare_profile_adoption(&next.account_id())
            .unwrap();
    }
    let corrupt = directory
        .path()
        .join(format!("account-documents-{}.log", next.account_id()));
    std::fs::write(&corrupt, b"injected late legacy validation failure").unwrap();
    assert!(open_profile(directory.path(), identity, next).is_err());
    assert!(!directory.path().join("profile.db").exists());
    std::fs::remove_file(corrupt).unwrap();
    let reopened = open_profile(
        directory.path(),
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(next_key),
    )
    .unwrap();
    let retained: bool = reopened
        .storage
        .database()
        .read(|connection| {
            Ok(connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1)",
                [&old_id],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert!(
        retained,
        "failed pre-SQL adoption lost the exact previous account namespace"
    );
}

#[test]
fn reserved_lock_source_is_rejected_without_changing_source_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("node.lock");
    {
        let identity = DeviceIdentity::generate();
        let mut source = PersistentEventLog::open(&path, "pw").unwrap();
        source
            .append(Event::new(
                &identity,
                ConversationId::new([51; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Profile,
                b"source authority".to_vec(),
            ))
            .unwrap();
    }
    let original = std::fs::read(&path).unwrap();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    assert!(
        Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &path,
            &directory.path().join("sent.log"),
            "pw"
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[test]
fn legacy_adoption_publication_crash_cuts_recover_exact_account_namespace() {
    use crate::storage::profile::publication::tests::CUT;
    for cut in [
        "participant-before-publication",
        "participant-after-publication",
        "participant-after-legacy-binding",
        "participant-after-adoption-clear",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let previous = Account::generate();
        let previous_id = previous.account_id();
        let next = Account::generate();
        let next_key = next.secret_bytes();
        let log = directory.path().join("messages.log");
        {
            let mut delivery =
                super::super::delivery_store::DeliveryStore::open_for_log(&log, "pw").unwrap();
            delivery
                .bind_profile(&log, "pw", &identity.public(), &previous_id)
                .unwrap();
            delivery
                .prepare_profile_adoption(&next.account_id())
                .unwrap();
        }
        CUT.with(|value| value.replace(Some(cut)));
        assert!(
            open_profile(directory.path(), identity, next).is_err(),
            "{cut} did not interrupt"
        );
        CUT.with(|value| value.replace(None));
        let reopened = open_profile(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(next_key),
        )
        .unwrap();
        let (retained,pending): (bool,bool) = reopened.storage.database().read(|connection| Ok((
            connection.query_row("SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1)",[&previous_id],|row| row.get(0))?,
            connection.query_row("SELECT bootstrap_previous IS NOT NULL FROM participant_profile WHERE singleton=1",[],|row| row.get(0))?,
        ))).unwrap();
        assert!(retained, "{cut} lost previous account ID provenance");
        assert!(!pending, "{cut} did not finish legacy adoption");
    }
}

#[test]
fn suspended_adoption_cannot_replace_exact_pending_next_intent() {
    let directory = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let old = Account::generate();
    let next = Account::generate();
    let next_key = next.secret_bytes();
    let node = open_profile(directory.path(), identity, old).unwrap();
    node.persist_account_adoption(&next.public(), || Ok(()), || Ok(node.account.public()))
        .unwrap();
    let unrelated = Account::generate();
    let mut save_called = false;
    assert!(
        node.persist_account_adoption(
            &unrelated.public(),
            || {
                save_called = true;
                Ok(())
            },
            || Ok(node.account.public())
        )
        .is_err()
    );
    assert!(!save_called);
    drop(node);
    assert!(
        open_profile(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(next_key)
        )
        .is_ok()
    );
}

#[test]
fn bare_filename_profile_preserves_source_semantics() {
    if std::env::var_os("MESH_TEST_BARE_PROFILE").is_some() {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            Path::new("archive.events"),
            Path::new("transcript.saved"),
            "pw",
        )
        .unwrap();
        assert!(Path::new("archive.events").is_file());
        assert!(Path::new("transcript.saved").is_file());
        assert!(!Path::new("messages.log").exists());
        assert!(!Path::new("sent.log").exists());
        node.close_profile().unwrap();
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact","node::node::participant_storage_tests::bare_filename_profile_preserves_source_semantics","--nocapture"])
        .env("MESH_TEST_BARE_PROFILE","1").current_dir(directory.path()).status().unwrap().success());
}

#[test]
fn prepared_sql_account_adoption_requires_both_exact_durable_owners() {
    for cut in [
        "delivery-only-old",
        "prepared-old",
        "prepared-next",
        "missing-delivery",
        "third-key",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let keys = identity.secret_bytes();
        let old = Account::generate();
        let old_key = old.secret_bytes();
        let next = Account::generate();
        let next_key = next.secret_bytes();
        let node = open_profile(directory.path(), identity, old).unwrap();
        if cut != "missing-delivery" {
            node.delivery
                .lock()
                .unwrap()
                .prepare_profile_adoption(&next.account_id())
                .unwrap();
        }
        if cut != "delivery-only-old" {
            crate::storage::profile::participant::prepare_adoption(
                node.storage.database(),
                &node.account.public(),
                &next.public(),
            )
            .unwrap();
        }
        drop(node);
        let loaded = match cut {
            "delivery-only-old" | "prepared-old" => Account::from_secret_bytes(old_key),
            "third-key" => Account::generate(),
            _ => Account::from_secret_bytes(next_key),
        };
        let reopened = open_profile(
            directory.path(),
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            loaded,
        );
        if matches!(cut, "missing-delivery" | "third-key") {
            assert!(reopened.is_err(), "{cut} did not fail closed");
        } else {
            let reopened = reopened.unwrap();
            let pending: bool = reopened.storage.database().read(|connection| Ok(connection.query_row(
                "SELECT next_account IS NOT NULL FROM participant_profile WHERE singleton=1",[],|row| row.get(0))?)).unwrap();
            assert!(!pending, "{cut} left an unresolved account transition");
        }
    }
}

#[cfg(unix)]
#[test]
fn explicit_external_parent_aliases_share_the_same_owned_source() {
    let directory = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    let alias = directory.path().join("external-alias");
    std::os::unix::fs::symlink(external.path(), &alias).unwrap();
    let identity = DeviceIdentity::generate();
    let keys = identity.secret_bytes();
    let account = Account::generate();
    let account_key = account.secret_bytes();
    let open = |identity, account, sent: &Path| {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &directory.path().join("custom.events"),
            sent,
            "pw",
        )
    };
    let node = open(identity, account, &alias.join("explicit.transcript")).unwrap();
    assert!(crate::storage::profile::ProfileStorageLease::acquire(external.path()).is_err());
    assert!(external.path().join("explicit.transcript").is_file());
    node.close_profile().unwrap();
    let node = open(
        DeviceIdentity::from_secret_bytes(keys.0, keys.1),
        Account::from_secret_bytes(account_key),
        &external.path().join("explicit.transcript"),
    )
    .unwrap();
    node.close_profile().unwrap();
    let original = std::fs::read(directory.path().join("custom.events")).unwrap();
    std::fs::hard_link(
        directory.path().join("custom.events"),
        directory.path().join("hardlink-source"),
    )
    .unwrap();
    assert!(
        open(
            DeviceIdentity::from_secret_bytes(keys.0, keys.1),
            Account::from_secret_bytes(account_key),
            &external.path().join("explicit.transcript")
        )
        .is_err()
    );
    assert_eq!(
        std::fs::read(directory.path().join("custom.events")).unwrap(),
        original
    );
}

#[test]
fn header_only_legacy_sources_are_valid_bootstrap_input() {
    let directory = tempfile::tempdir().unwrap();
    drop(PersistentEventLog::open(&directory.path().join("messages.log"), "pw").unwrap());
    drop(crate::node::sentlog::SentLog::open(&directory.path().join("sent.log"), "pw").unwrap());
    let node = open_profile(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
    )
    .unwrap();
    assert!(directory.path().join("profile.db").is_file());
    assert!(node.log.lock().unwrap().conversations().unwrap().is_empty());
}

#[test]
fn custom_sources_cannot_occupy_a_different_account_sidecar_namespace() {
    let directory = tempfile::tempdir().unwrap();
    let other = Account::generate();
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    let result = Node::open_with_account(
        DeviceIdentity::generate(),
        Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &directory.path().join("messages.log"),
        &directory
            .path()
            .join(format!("account-devices-{}.log", other.account_id())),
        "pw",
    );
    assert!(
        result.is_err(),
        "custom transcript occupied an existing account-domain namespace"
    );
    assert!(!directory.path().join("messages.log").exists());
}

#[test]
fn cli_identity_preflight_rejects_an_owned_legacy_sidecar_name() {
    let directory = tempfile::tempdir().unwrap();
    let lease = crate::storage::profile::ProfileStorageLease::acquire(directory.path()).unwrap();
    assert!(
        lease
            .validate_identity_sources(&directory.path().join("ratchet.sessions"))
            .is_err()
    );
    assert!(!directory.path().join("ratchet.sessions").exists());
}

#[test]
fn portable_case_aliases_are_rejected_before_source_writes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("NODE.LOCK");
    std::fs::write(&source, b"reserved source bytes").unwrap();
    let pair_directory = tempfile::tempdir().unwrap();
    for (events, sent) in [
        (source.clone(), directory.path().join("sent.log")),
        (
            pair_directory.path().join("Case.events"),
            pair_directory.path().join("case.EVENTS"),
        ),
    ] {
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        assert!(
            Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &events,
                &sent,
                "pw"
            )
            .is_err()
        );
    }
    assert_eq!(std::fs::read(source).unwrap(), b"reserved source bytes");
    assert!(!pair_directory.path().join("Case.events").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canceled_peer_cache_writer_retains_profile_after_last_node_drop() {
    let directory = tempfile::tempdir().unwrap();
    let node = open_profile(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
    )
    .unwrap();
    node.configure_privacy(
        directory.path(),
        "pw",
        &node.signed_announce("Owner", 1),
        Arc::new(crate::discovery::DiscoveryVisibility::new(true)),
    )
    .unwrap();
    let peer = crate::discovery::Announce::new_with_account(
        &DeviceIdentity::generate(),
        &Account::generate(),
        "Persisted peer",
        2,
    );
    node.roster
        .lock()
        .unwrap()
        .update(&peer, "127.0.0.1".parse().unwrap(), &node.user_id());
    let (entered, started) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    *node.peer_snapshot_hook.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
    }));
    let work = node.runtime_work.clone();
    let sender = node.clone();
    let caller = tokio::spawn(async move { sender.cached_peer_snapshot_async().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    caller.abort();
    assert!(matches!(caller.await,Err(error) if error.is_cancelled()));
    drop(node);
    let released_early =
        crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok();
    release.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), work.drain())
        .await
        .unwrap();
    assert!(
        !released_early,
        "peer-cache facade wrote after its profile lease was released"
    );
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok());
}

#[test]
fn retained_privacy_facade_prevents_checked_profile_close() {
    let directory = tempfile::tempdir().unwrap();
    let node = open_profile(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
    )
    .unwrap();
    let facade = node.privacy.clone();
    let closed = node.close_profile().is_ok();
    let released_early =
        crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok();
    drop(facade);
    assert!(!closed, "checked close ignored retained privacy facade");
    assert!(!released_early);
    assert!(crate::storage::profile::ProfileStorageLease::acquire(directory.path()).is_ok());
}

#[test]
fn reaction_delivery_and_opaque_names_are_reserved_before_any_writer() {
    for kind in [
        "case-reactions",
        "other-reactions",
        "delivery",
        "opaque",
        "file-identities",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let source = directory.path().join("messages.log");
        {
            let mut log = PersistentEventLog::open(&source, "pw").unwrap();
            log.append(Event::new(
                &identity,
                ConversationId::new([81; 32]),
                1,
                vec![],
                1,
                0,
                EventKind::Profile,
                b"existing source bytes".to_vec(),
            ))
            .unwrap();
        }
        let before = std::fs::read(&source).unwrap();
        let leaf = match kind {
            "case-reactions" => format!("MESSAGES.{}-REACTIONS.LOG", account.account_id()),
            "other-reactions" => format!(
                "messages.{}-reactions.log",
                Account::generate().account_id()
            ),
            "delivery" => "MESSAGES.LOG.DELIVERY-OWNER".into(),
            "file-identities" => "FILE-MESSAGE-IDENTITIES.LOG".into(),
            _ => "other.events.opaque-receive".into(),
        };
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (file, _) = mpsc::unbounded_channel();
        let result = Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &source,
            &directory.path().join(&leaf),
            "pw",
        );
        assert!(result.is_err(), "{kind} reserved namespace was admitted");
        assert_eq!(std::fs::read(&source).unwrap(), before);
        assert!(!directory.path().join(&leaf).exists());
        assert!(
            !directory.path().join("received.log").exists(),
            "{kind} started an ordinary writer before rejection"
        );
    }
}

#[test]
fn identity_preflight_rejects_reaction_delivery_and_opaque_names() {
    let directory = tempfile::tempdir().unwrap();
    let lease = crate::storage::profile::ProfileStorageLease::acquire(directory.path()).unwrap();
    for leaf in [
        format!(
            "MESSAGES.{}-REACTIONS.LOG",
            Account::generate().account_id()
        ),
        "messages.log.delivery-transactions".into(),
        "other.events.opaque-receive".into(),
    ] {
        assert!(
            lease
                .validate_identity_sources(&directory.path().join(&leaf))
                .is_err(),
            "identity occupied {leaf}"
        );
        assert!(!directory.path().join(leaf).exists());
    }
}

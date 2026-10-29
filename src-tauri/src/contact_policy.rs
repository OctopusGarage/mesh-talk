//! Local account-scoped contact presentation policy, separate from network admission.

mod deletion;
// Glob also forwards Tauri command macros.
pub use deletion::*;

#[cfg(test)]
mod native_mesh_tests;

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const ME: &str = "cccccccccccccccccccccccccccccccc";

    #[test]
    fn deleting_offline_contact_persists_without_becoming_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        store
            .delete_unlocked("alice", A, ME, false, || Ok(()))
            .unwrap();
        let reopened = HiddenContactsState::new(dir.path().to_owned());
        let snapshot = reopened.get(Some("alice"), "alice").unwrap();
        assert!(snapshot.contacts.is_empty());
        assert_eq!(snapshot.deleted, vec![A.to_owned()]);
        assert!(reopened.get(Some("bob"), "bob").unwrap().deleted.is_empty());
        assert!(
            reopened
                .visible_accounts("alice", &[], vec![A.into()])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened
                .visible_accounts("alice", &[A.into()], vec![A.into()])
                .unwrap(),
            vec![A.to_owned()]
        );
        assert!(
            reopened
                .get(Some("alice"), "alice")
                .unwrap()
                .deleted
                .is_empty()
        );
        assert_eq!(
            reopened
                .visible_accounts("alice", &[], vec![A.into()])
                .unwrap(),
            vec![A.to_owned()]
        );
    }

    #[test]
    fn deletion_rejects_online_self_invalid_and_failed_save() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        assert!(
            store
                .delete_unlocked("alice", A, ME, true, || Ok(()))
                .is_err()
        );
        assert!(
            store
                .delete_unlocked("alice", ME, ME, false, || Ok(()))
                .is_err()
        );
        assert!(
            store
                .delete_unlocked("alice", "invalid", ME, false, || Ok(()))
                .is_err()
        );
        assert!(
            store
                .get(Some("alice"), "alice")
                .unwrap()
                .deleted
                .is_empty()
        );
        std::fs::create_dir_all(store.path("alice")).unwrap();
        assert!(
            store
                .delete_unlocked("alice", A, ME, false, || Ok(()))
                .is_err()
        );
    }

    #[test]
    fn deletion_validates_before_erasing_and_does_not_commit_failed_erasure() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        assert!(
            store
                .delete_unlocked("alice", A, ME, true, || panic!(
                    "online history must remain"
                ))
                .is_err()
        );
        assert!(
            store
                .delete_unlocked("alice", A, ME, false, || Err("erase failed".into()))
                .is_err()
        );
        assert!(!store.path("alice").exists());
    }

    #[test]
    fn owner_length_contract_accepts_128_and_rejects_129_bytes() {
        let accepted = "a".repeat(128);
        let rejected = "a".repeat(129);
        assert!(authorize(Some(&accepted), &accepted).is_ok());
        assert!(authorize(Some(&rejected), &rejected).is_err());
    }

    #[test]
    fn policy_file_size_contract_accepts_two_mib_and_preserves_oversized_data() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let path = store.path("alice");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = br#"{"owner":"alice","contacts":[]}"#.to_vec();
        // Fixed public limits, independent of the implementation constant.
        bytes.resize(2_097_152, b' ');
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            store
                .get(Some("alice"), "alice")
                .unwrap()
                .contacts
                .is_empty()
        );
        bytes.push(b' ');
        std::fs::write(&path, &bytes).unwrap();
        assert!(store.get(Some("alice"), "alice").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn non_not_found_read_failure_is_not_an_empty_policy() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let blocked = store.path("alice");
        std::fs::create_dir_all(&blocked).unwrap();
        std::fs::write(blocked.join("preserved"), b"untouched").unwrap();
        assert!(store.get(Some("alice"), "alice").is_err());
        assert_eq!(
            std::fs::read(blocked.join("preserved")).unwrap(),
            b"untouched"
        );
        // Unix reports ENOTDIR for a file in an intermediate path. Windows maps
        // that situation to NotFound, so use the final-path directory above there.
        #[cfg(unix)]
        {
            let blocked_root = dir.path().join("blocked-root");
            std::fs::write(&blocked_root, b"untouched").unwrap();
            let store = HiddenContactsState::new(blocked_root.clone());
            assert!(store.get(Some("alice"), "alice").is_err());
            assert_eq!(std::fs::read(blocked_root).unwrap(), b"untouched");
        }
    }

    #[test]
    fn persistence_isolated_idempotent_and_offline_restore() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        store
            .update(Some("alice"), "alice", A, true, "Alice", Some(ME))
            .unwrap();
        store
            .update(Some("alice"), "alice", A, true, "Alice", Some(ME))
            .unwrap();
        store
            .update(Some("alice"), "alice", B, true, "Bob", Some(ME))
            .unwrap();
        assert_eq!(store.get(Some("bob"), "bob").unwrap().contacts.len(), 0);
        let reopened = HiddenContactsState::new(dir.path().to_owned());
        assert_eq!(
            reopened.get(Some("alice"), "alice").unwrap().contacts.len(),
            2
        );
        let snapshot = reopened
            .update(Some("alice"), "alice", A, false, "", None)
            .unwrap();
        assert_eq!(
            snapshot.contacts,
            vec![HiddenContact {
                account_id: B.into(),
                name: "Bob".into()
            }]
        );
    }

    #[test]
    fn authorization_validation_and_self_rejection() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        assert!(store.get(None, "alice").is_err());
        assert!(store.get(Some("alice"), "bob").is_err());
        for owner in ["../alice", "a/b", "a\\b", "", ".", ".."] {
            assert!(store.get(Some(owner), owner).is_err());
        }
        for account in ["../bad", "AAAA", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"] {
            assert!(
                store
                    .update(Some("alice"), "alice", account, true, "name", Some(ME))
                    .is_err()
            );
        }
        assert!(
            store
                .update(Some("alice"), "alice", ME, true, "me", Some(ME))
                .is_err()
        );
        assert!(
            store
                .update(Some("alice"), "alice", A, true, "name", None)
                .is_err()
        );
        let snapshot = store
            .update(Some("alice"), "alice", A, true, &"x".repeat(257), Some(ME))
            .unwrap();
        assert_eq!(snapshot.contacts[0].name.chars().count(), 256);
    }

    #[test]
    fn corrupt_file_and_save_failure_are_errors_and_preserve_data() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let parent = dir.path().join("accounts/alice");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join("hidden-contacts.json");
        std::fs::write(&path, b"invalid json").unwrap();
        assert!(store.get(Some("alice"), "alice").is_err());
        assert!(
            store
                .update(Some("alice"), "alice", A, true, "name", Some(ME))
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid json");
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, b"untouched").unwrap();
        let blocked_store = HiddenContactsState::new(blocked.clone());
        assert!(
            blocked_store
                .update(Some("alice"), "alice", A, true, "name", Some(ME))
                .is_err()
        );
        assert_eq!(std::fs::read(blocked).unwrap(), b"untouched");
    }

    #[test]
    fn concurrent_updates_do_not_lose_records() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(HiddenContactsState::new(dir.path().to_owned()));
        let threads: Vec<_> = (0..16)
            .map(|index| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .update(
                            Some("alice"),
                            "alice",
                            &format!("{index:032x}"),
                            true,
                            "name",
                            Some(ME),
                        )
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(
            store.get(Some("alice"), "alice").unwrap().contacts.len(),
            16
        );
    }

    #[test]
    fn file_validation_limits_and_failed_atomic_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let parent = dir.path().join("accounts/alice");
        std::fs::create_dir_all(&parent).unwrap();
        let path = parent.join("hidden-contacts.json");
        for value in [
            serde_json::json!({ "owner": "bob", "contacts": [] }),
            serde_json::json!({ "owner": "alice", "contacts": [{ "account_id": "bad", "name": "name" }] }),
            serde_json::json!({ "owner": "alice", "contacts": [], "deleted": ["bad"] }),
            serde_json::json!({ "owner": "alice", "contacts": [], "deleted": [A, A] }),
            serde_json::json!({ "owner": "alice", "contacts": [{ "account_id": A, "name": "name" }], "deleted": [A] }),
            serde_json::json!({ "owner": "alice", "contacts": [{ "account_id": A, "name": "invalid\nname" }] }),
            serde_json::json!({ "owner": "alice", "contacts": [{ "account_id": A, "name": "name" }, { "account_id": A, "name": "name" }] }),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                store
                    .update(Some("alice"), "alice", B, false, "", None)
                    .is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let snapshot = HiddenContactsSnapshot {
            owner: "alice".into(),
            deleted: vec![],
            contacts: (0..MAX_CONTACTS)
                .map(|index| HiddenContact {
                    account_id: format!("{index:032x}"),
                    name: "name".into(),
                })
                .collect(),
        };
        store.save(&snapshot).unwrap();
        assert!(
            store
                .update(Some("alice"), "alice", A, true, "name", Some(ME))
                .is_err()
        );
        assert_eq!(
            store.get(Some("alice"), "alice").unwrap().contacts.len(),
            MAX_CONTACTS
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("preserved"), b"original").unwrap();
        assert!(store.save(&snapshot).is_err());
        assert_eq!(std::fs::read(path.join("preserved")).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn policy_has_private_file_and_directory_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        store
            .update(Some("alice"), "alice", A, true, "name", Some(ME))
            .unwrap();
        let path = store.path("alice");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn directory_sync_failure_after_commit_returns_persisted_success() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let snapshot = HiddenContactsSnapshot {
            owner: "alice".into(),
            deleted: vec![],
            contacts: vec![HiddenContact {
                account_id: A.into(),
                name: "name".into(),
            }],
        };
        store
            .save_with_directory_sync(&snapshot, |_| {
                Err(std::io::Error::other("injected sync failure"))
            })
            .unwrap();
        assert_eq!(
            store.get(Some("alice"), "alice").unwrap().contacts,
            snapshot.contacts
        );
    }

    pub(super) fn session() -> crate::state::SessionState {
        let session = crate::state::SessionState::default();
        session.set(
            "token".into(),
            crate::services::user::User {
                user_id: "alice".into(),
                account_login_name: None,
                name: "alice".into(),
                display_name: "Alice".into(),
                address: String::new(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "password".into(),
        );
        session
    }

    #[test]
    fn transaction_holds_current_session_until_disk_operation_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let session = session();
        let clearing_session = session.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_rx.recv().unwrap();
            clearing_session.clear();
            done_tx.send(()).unwrap();
        });
        store
            .transaction(&session, "alice", || {
                started_tx.send(()).unwrap();
                assert!(
                    done_rx
                        .recv_timeout(std::time::Duration::from_millis(50))
                        .is_err()
                );
                store.load("alice")
            })
            .unwrap();
        thread.join().unwrap();
    }

    #[test]
    fn mismatched_runtime_namespace_cannot_hide_contact() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        assert!(
            store
                .update_for_session(&session(), "alice", A, true, "name", Some(("bob", ME)))
                .is_err()
        );
        assert!(
            store
                .get(Some("alice"), "alice")
                .unwrap()
                .contacts
                .is_empty()
        );
    }
    #[test]
    fn queued_mutation_reauthorizes_after_logout() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let session = session();
        let guard = store.lock.lock().unwrap();
        let queued_store = store.clone();
        let queued_session = session.clone();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            queued_store.update_for_session(
                &queued_session,
                "alice",
                A,
                true,
                "name",
                Some(("alice", ME)),
            )
        });
        started_rx.recv().unwrap();
        session.clear();
        drop(guard);
        assert!(thread.join().unwrap().is_err());
        assert!(!store.path("alice").exists());
    }

    #[test]
    fn authorized_session_hides_and_restores_offline() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let session = session();
        store
            .update_for_session(&session, "alice", A, true, "name", Some(("alice", ME)))
            .unwrap();
        assert!(
            store
                .update_for_session(&session, "alice", ME, true, "self", Some(("alice", ME)))
                .is_err()
        );
        assert!(
            store
                .update_for_session(&session, "alice", A, false, "", None)
                .unwrap()
                .contacts
                .is_empty()
        );
    }
    #[test]
    fn live_identity_is_read_only_inside_authorized_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let session = session();
        session.clear();
        let called = std::cell::Cell::new(false);
        assert!(
            store
                .update_with_identity(&session, "alice", A, true, "name", || {
                    called.set(true);
                    (Some(("alice".to_owned(), ME.to_owned())), ())
                })
                .is_err()
        );
        assert!(!called.get());
        let live_session = self::session();
        assert!(
            store
                .update_with_identity(&live_session, "alice", A, true, "name", || {
                    assert!(store.lock.try_lock().is_err());
                    // The node's current crypto identity may have changed through adoption
                    // since the frontend prepared the request. Consult it in this lock.
                    (Some(("alice".to_owned(), A.to_owned())), ())
                })
                .is_err()
        );
        assert!(!store.path("alice").exists());
    }

    #[test]
    fn untrusted_peer_names_cannot_prevent_hiding() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let snapshot = store
            .update(
                Some("alice"),
                "alice",
                A,
                true,
                &format!("  \n{}\t  ", "🦀".repeat(300)),
                Some(ME),
            )
            .unwrap();
        assert_eq!(snapshot.contacts[0].name, "🦀".repeat(256));
        assert!(store.get(Some("alice"), "alice").is_ok());
        let snapshot = store
            .update(Some("alice"), "alice", A, true, "\r\n\t  ", Some(ME))
            .unwrap();
        assert_eq!(snapshot.contacts[0].name, &A[..8]);
        assert!(
            store
                .update(Some("alice"), "alice", A, false, &"\n".repeat(3000), None)
                .unwrap()
                .contacts
                .is_empty()
        );
    }

    #[test]
    fn queued_contact_commands_leave_session_available_for_runtime_installation() {
        use std::sync::mpsc::channel;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let auth = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(dir.path().to_owned()),
            ),
        ));
        let app_state = crate::state::AppState::new(auth);
        let signed_in = session().get().unwrap();
        app_state
            .session()
            .set(signed_in.token, signed_in.user, signed_in.password);
        let node_state = crate::chat_commands::NodeState::empty();
        let app = tauri::test::mock_builder()
            .manage(app_state.clone())
            .manage(node_state.clone())
            .manage(HiddenContactsState::new(dir.path().to_owned()))
            .invoke_handler(tauri::generate_handler![
                set_contact_hidden,
                delete_contact,
                crate::chat_commands::list_accounts
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        for cmd in ["list_accounts", "set_contact_hidden", "delete_contact"] {
            let runtime_guard = node_state.0.blocking_write();
            let (entered, waiting) = channel();
            *app_state.owner_command_captured.lock().unwrap() = Some(entered);
            let window = webview.clone();
            let request = std::thread::spawn(move || {
                tauri::test::get_ipc_response(
                    &window,
                    tauri::webview::InvokeRequest {
                        cmd: cmd.into(),
                        callback: tauri::ipc::CallbackFn(0),
                        error: tauri::ipc::CallbackFn(1),
                        url: if cfg!(windows) {
                            "http://tauri.localhost"
                        } else {
                            "tauri://localhost"
                        }
                        .parse()
                        .unwrap(),
                        body: tauri::ipc::InvokeBody::Json(serde_json::json!({
                            "owner": "alice", "account": A, "hidden": true, "name": "Alice"
                        })),
                        headers: Default::default(),
                        invoke_key: tauri::test::INVOKE_KEY.into(),
                    },
                )
                .is_err()
            });
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            let session = app_state.session().clone();
            let (captured, available) = channel();
            let observer = std::thread::spawn(move || {
                captured.send(session.capture().is_ok()).unwrap();
            });
            let session_available = available.recv_timeout(Duration::from_secs(1));
            // Release the runtime even on regression, so this test fails without
            // leaving blocked workers behind in the test process.
            drop(runtime_guard);
            observer.join().unwrap();
            assert!(
                request.join().unwrap(),
                "empty runtime must reject the command"
            );
            assert_eq!(
                session_available.ok(),
                Some(true),
                "{cmd} locked the session while waiting for the runtime"
            );
        }
    }

    #[test]
    fn queued_contact_mutations_reject_same_owner_relogin() {
        use std::sync::mpsc::channel;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let app_state =
            crate::state::AppState::new(crate::services::auth_service::AuthService::new(Arc::new(
                mesh_talk_core::identity::manager::IdentityManager::new(
                    mesh_talk_core::storage::file_manager::FileManager::new(dir.path().to_owned()),
                ),
            )));
        let signed_in = session().get().unwrap();
        app_state.session().set(
            signed_in.token.clone(),
            signed_in.user.clone(),
            signed_in.password.clone(),
        );
        let node_state = crate::chat_commands::NodeState::empty();
        let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            dir.path(),
            "alice",
            "Alice",
            "password",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        let node = runtime.handle();
        tauri::async_runtime::block_on(node.react_to_account(
            A,
            mesh_talk_core::eventlog::EventId::new([9; 32]),
            "👍",
            false,
        ))
        .unwrap();
        let before = node.account_reactions(A).unwrap();
        assert_eq!(before.len(), 1);
        *node_state.0.blocking_write() = Some(runtime);
        let app = tauri::test::mock_builder()
            .manage(app_state.clone())
            .manage(node_state.clone())
            .manage(HiddenContactsState::new(dir.path().to_owned()))
            .invoke_handler(tauri::generate_handler![
                set_contact_hidden,
                delete_contact,
                crate::chat_commands::list_accounts
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        for cmd in ["delete_contact", "set_contact_hidden", "list_accounts"] {
            let guard = node_state.0.blocking_write();
            let (entered, waiting) = channel();
            *app_state.owner_command_captured.lock().unwrap() = Some(entered);
            let window = webview.clone();
            let pending = std::thread::spawn(move || {
                tauri::test::get_ipc_response(&window,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(), callback: tauri::ipc::CallbackFn(0), error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) { "http://tauri.localhost" } else { "tauri://localhost" }.parse().unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({"owner":"alice", "account":A, "hidden":true, "name":"Alice"})),
                    headers: Default::default(), invoke_key: tauri::test::INVOKE_KEY.into(),
                }).is_err()
            });
            waiting.recv_timeout(Duration::from_secs(5)).unwrap();
            app_state.session().publish(
                "replacement".into(),
                signed_in.user.clone(),
                signed_in.password.clone(),
            );
            drop(guard);
            assert!(
                pending.join().unwrap(),
                "{cmd} must reject the replaced entry lease"
            );
            assert_eq!(
                node.account_reactions(A).unwrap(),
                before,
                "replacement session's durable data survives"
            );
        }
        tauri::async_runtime::block_on(node_state.0.blocking_write().take().unwrap().stop());
    }

    #[test]
    fn registered_ipc_commands_authorize_persist_and_restore_offline() {
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let auth = crate::services::auth_service::AuthService::new(std::sync::Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(dir.path().to_owned()),
            ),
        ));
        let app_state = crate::state::AppState::new(auth);
        let node_state = crate::chat_commands::NodeState::empty();
        let app = tauri::test::mock_builder()
            .manage(app_state.clone())
            .manage(node_state.clone())
            .manage(HiddenContactsState::new(dir.path().to_owned()))
            .invoke_handler(tauri::generate_handler![
                get_hidden_contacts,
                set_contact_hidden,
                delete_contact,
                crate::chat_commands::list_accounts
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |cmd: &str, body: serde_json::Value| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: cmd.into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(body),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
            .map(|body| body.deserialize::<HiddenContactsSnapshot>().unwrap())
        };
        assert!(
            invoke(
                "get_hidden_contacts",
                serde_json::json!({ "owner": "alice" })
            )
            .is_err()
        );
        let signed_in = session().get().unwrap();
        app_state
            .session()
            .set(signed_in.token, signed_in.user, signed_in.password);
        let snapshot = invoke(
            "get_hidden_contacts",
            serde_json::json!({ "owner": "alice" }),
        )
        .unwrap();
        assert_eq!(snapshot.owner, "alice");
        assert!(snapshot.contacts.is_empty());
        assert!(invoke("get_hidden_contacts", serde_json::json!({ "owner": "bob" })).is_err());
        assert!(
            invoke(
                "get_hidden_contacts",
                serde_json::json!({ "ownerId": "alice" })
            )
            .is_err()
        );
        assert!(invoke(
            "set_contact_hidden",
            serde_json::json!({ "owner": "alice", "accountId": A, "hidden": true, "name": "name" })
        )
        .is_err());
        assert!(invoke(
            "set_contact_hidden",
            serde_json::json!({ "owner": "alice", "account": A, "hidden": true, "name": "name" })
        )
        .is_err());

        let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            dir.path(),
            "alice",
            "Alice",
            "password",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        let own = runtime.account_id().to_owned();
        *node_state.0.blocking_write() = Some(runtime);
        assert!(invoke(
            "set_contact_hidden",
            serde_json::json!({ "owner": "alice", "account": own, "hidden": true, "name": "self" })
        )
        .is_err());
        let hidden = invoke("set_contact_hidden", serde_json::json!({ "owner": "alice", "account": A, "hidden": true, "name": "\nAlice\t" })).unwrap();
        assert_eq!(
            hidden.contacts,
            vec![HiddenContact {
                account_id: A.into(),
                name: "Alice".into()
            }]
        );
        let persisted: HiddenContactsSnapshot = serde_json::from_slice(
            &std::fs::read(dir.path().join("accounts/alice/hidden-contacts.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted.contacts, hidden.contacts);
        assert_eq!(
            invoke(
                "get_hidden_contacts",
                serde_json::json!({ "owner": "alice" })
            )
            .unwrap()
            .contacts,
            hidden.contacts
        );
        assert!(
            invoke(
                "delete_contact",
                serde_json::json!({ "owner": "bob", "account": A })
            )
            .is_err()
        );
        let deleted = invoke(
            "delete_contact",
            serde_json::json!({ "owner": "alice", "account": A }),
        )
        .unwrap();
        assert_eq!(deleted.deleted, vec![A.to_owned()]);
        assert!(deleted.contacts.is_empty());
        assert!(
            invoke(
                "delete_contact",
                serde_json::json!({ "owner": "alice", "account": own })
            )
            .is_err()
        );
        // Preserve the original offline restore coverage by hiding the contact again.
        invoke(
            "set_contact_hidden",
            serde_json::json!({ "owner": "alice", "account": A, "hidden": true, "name": "Alice" }),
        )
        .unwrap();
        // Stop the node: restoration remains a local authenticated operation.
        app.state::<crate::chat_commands::NodeState>()
            .0
            .blocking_write()
            .take();
        assert!(
            invoke(
                "set_contact_hidden",
                serde_json::json!({ "owner": "alice", "account": A, "hidden": false, "name": "" })
            )
            .unwrap()
            .contacts
            .is_empty()
        );
        app_state.session().clear();
        assert!(
            invoke(
                "set_contact_hidden",
                serde_json::json!({ "owner": "alice", "account": A, "hidden": false, "name": "" })
            )
            .is_err()
        );
    }
}
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    io::Write,
    path::PathBuf,
    sync::{Arc, Mutex},
};

const MAX_CONTACTS: usize = 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiddenContact {
    pub account_id: String,
    pub name: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiddenContactsSnapshot {
    pub owner: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deleted: Vec<String>,
    pub contacts: Vec<HiddenContact>,
}
/// One application-wide lock serializes complete read/modify/replace operations.
/// Disk remains authoritative, including after a failed save.
#[derive(Clone)]
pub struct HiddenContactsState {
    root: PathBuf,
    lock: Arc<Mutex<()>>,
}
impl Default for HiddenContactsState {
    fn default() -> Self {
        Self::new(crate::data_dir())
    }
}

fn valid_account(account: &str) -> bool {
    account.len() == 32
        && account
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_name(name: &str) -> bool {
    name.len() <= 1024 && name.chars().count() <= 256 && !name.chars().any(char::is_control)
}
fn normalized_name(name: &str, account: &str) -> String {
    let bounded: String = name
        .trim_matches(|character: char| character.is_control() || character.is_whitespace())
        .chars()
        .filter(|character| !character.is_control())
        .take(256)
        .collect();
    if bounded.is_empty() {
        account[..8].to_owned()
    } else {
        bounded
    }
}
fn authorize(current: Option<&str>, owner: &str) -> Result<(), String> {
    if current != Some(owner) {
        return Err("Hidden contacts require the current signed-in account".into());
    }
    if owner.is_empty()
        || owner.len() > 128
        || !owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid contact policy owner".into());
    }
    Ok(())
}

impl HiddenContactsState {
    #[cfg(test)]
    fn update_with_identity<G>(
        &self,
        session: &crate::state::SessionState,
        owner: &str,
        account: &str,
        hidden: bool,
        name: &str,
        identity: impl FnOnce() -> (Option<(String, String)>, G),
    ) -> Result<HiddenContactsSnapshot, String> {
        self.transaction(session, owner, || {
            // Read the identity only after authorization. The caller acquires
            // any runtime guard before entering the policy/session transaction
            // and retains it through the write, including account adoption.
            let (own, _guard) = identity();
            if hidden && own.as_ref().map(|(namespace, _)| namespace.as_str()) != Some(owner) {
                return Err("Hiding a contact requires this account's running node".into());
            }
            self.update_unlocked(
                owner,
                account,
                hidden,
                name,
                own.as_ref().map(|(_, account)| account.as_str()),
            )
        })
    }
    /// Acquire policy serialization before matching the originating session. The
    /// caller retains any runtime guard and validates its installation inside
    /// this synchronous closure; no session guard is held while waiting.
    pub(crate) fn transaction_for_lease<T>(
        &self,
        app: &crate::state::AppState,
        lease: &crate::state::SessionLease,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Hidden contacts lock unavailable")?;
        app.session().matching(lease, |_| {
            authorize(Some(lease.owner()), lease.owner())?;
            if app.app_lock().is_locked() {
                return Err("App is locked".into());
            }
            operation()
        })?
    }

    pub(crate) fn transaction<T>(
        &self,
        session: &crate::state::SessionState,
        owner: &str,
        operation: impl FnOnce() -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Hidden contacts lock unavailable")?;
        // Policy lock first, then live session lock: a request queued behind a
        // previous disk operation must reauthorize after acquiring that lock.
        session.with_owner(|current| {
            authorize(current, owner)?;
            operation()
        })
    }
    #[cfg(test)]
    fn update_for_session(
        &self,
        session: &crate::state::SessionState,
        owner: &str,
        account: &str,
        hidden: bool,
        name: &str,
        own: Option<(&str, &str)>,
    ) -> Result<HiddenContactsSnapshot, String> {
        self.update_with_identity(session, owner, account, hidden, name, || {
            (
                own.map(|(namespace, account)| (namespace.to_owned(), account.to_owned())),
                (),
            )
        })
    }
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            lock: Arc::new(Mutex::new(())),
        }
    }
    fn path(&self, owner: &str) -> PathBuf {
        self.root
            .join("accounts")
            .join(owner)
            .join("hidden-contacts.json")
    }
    fn load(&self, owner: &str) -> Result<HiddenContactsSnapshot, String> {
        let path = self.path(owner);
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(HiddenContactsSnapshot {
                    owner: owner.into(),
                    deleted: vec![],
                    contacts: vec![],
                });
            }
            Err(_) => return Err("Could not read hidden contacts".into()),
        };
        if file
            .metadata()
            .map_err(|_| "Could not inspect hidden contacts")?
            .len()
            > MAX_FILE_BYTES
        {
            return Err("Hidden contacts file is too large".into());
        }
        let snapshot: HiddenContactsSnapshot =
            serde_json::from_reader(file).map_err(|_| "Invalid hidden contacts file")?;
        let mut accounts = HashSet::new();
        if snapshot.owner != owner
            || snapshot.contacts.len() + snapshot.deleted.len() > MAX_CONTACTS
            || snapshot
                .deleted
                .iter()
                .any(|account| !valid_account(account) || !accounts.insert(account))
            || snapshot.contacts.iter().any(|contact| {
                !valid_account(&contact.account_id)
                    || !valid_name(&contact.name)
                    || !accounts.insert(&contact.account_id)
            })
        {
            return Err("Invalid hidden contacts file".into());
        }
        Ok(snapshot)
    }
    #[cfg(test)]
    fn get(&self, current: Option<&str>, owner: &str) -> Result<HiddenContactsSnapshot, String> {
        authorize(current, owner)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Hidden contacts lock unavailable")?;
        self.load(owner)
    }
    fn save_with_directory_sync(
        &self,
        snapshot: &HiddenContactsSnapshot,
        sync: impl FnOnce(&std::path::Path) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let path = self.path(&snapshot.owner);
        let parent = path.parent().ok_or("Invalid contact policy path")?;
        std::fs::create_dir_all(parent).map_err(|_| "Could not create contact policy directory")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "Could not protect contact policy directory")?;
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| "Could not create contact policy file")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            temporary
                .as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| "Could not protect contact policy file")?;
        }
        serde_json::to_writer(&mut temporary, snapshot)
            .map_err(|_| "Could not serialize hidden contacts")?;
        temporary
            .flush()
            .map_err(|_| "Could not write hidden contacts")?;
        temporary
            .as_file()
            .sync_all()
            .map_err(|_| "Could not sync hidden contacts")?;
        temporary
            .persist(&path)
            .map_err(|_| "Could not replace hidden contacts")?;
        // Atomic replacement is the commit point. A later durability failure cannot
        // be reported as a rejected mutation: the new policy is already visible.
        if sync(parent).is_err() {
            log::warn!("Hidden contacts committed; directory durability sync unavailable");
        }
        Ok(())
    }
    fn save(&self, snapshot: &HiddenContactsSnapshot) -> Result<(), String> {
        self.save_with_directory_sync(snapshot, |parent| {
            #[cfg(unix)]
            {
                std::fs::File::open(parent).and_then(|directory| directory.sync_all())
            }
            #[cfg(not(unix))]
            {
                let _ = parent;
                Ok(())
            }
        })
    }
    #[cfg(test)]
    fn update(
        &self,
        current: Option<&str>,
        owner: &str,
        account: &str,
        hidden: bool,
        name: &str,
        own_account: Option<&str>,
    ) -> Result<HiddenContactsSnapshot, String> {
        authorize(current, owner)?;
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "Hidden contacts lock unavailable")?;
        self.update_unlocked(owner, account, hidden, name, own_account)
    }
    fn update_unlocked(
        &self,
        owner: &str,
        account: &str,
        hidden: bool,
        name: &str,
        own_account: Option<&str>,
    ) -> Result<HiddenContactsSnapshot, String> {
        self.update_batch_unlocked(
            owner,
            &[HiddenContactChange {
                account: account.into(),
                hidden,
                name: name.into(),
            }],
            own_account,
            None,
        )
    }
    fn update_batch_unlocked(
        &self,
        owner: &str,
        records: &[HiddenContactChange],
        own_account: Option<&str>,
        expected: Option<&std::collections::HashMap<String, Option<String>>>,
    ) -> Result<HiddenContactsSnapshot, String> {
        if records.len() > 2048 || records.iter().any(|row| !valid_account(&row.account)) {
            return Err("Invalid hidden contacts batch".into());
        }
        for row in records.iter().filter(|row| row.hidden) {
            let own = own_account.ok_or("Hiding a contact requires a running node")?;
            if own == row.account {
                return Err("Cannot hide your own account".into());
            }
        }
        let mut snapshot = self.load(owner)?;
        if let Some(expected) = expected
            && records.iter().any(|row| {
                // Linked preferences cannot clear local deletion suppression.
                // Explicit local restore uses the non-CAS setter; rediscovery
                // separately owns clearing a deletion for an online contact.
                snapshot.deleted.contains(&row.account)
                    || !expected.contains_key(&row.account)
                    || snapshot
                        .contacts
                        .iter()
                        .find(|contact| contact.account_id == row.account)
                        .map(|contact| &contact.name)
                        != expected.get(&row.account).and_then(|name| name.as_ref())
            })
        {
            return Err("Local hidden contacts changed during linked import".into());
        }
        for row in records {
            snapshot.deleted.retain(|id| id != &row.account);
            snapshot
                .contacts
                .retain(|contact| contact.account_id != row.account);
            if row.hidden {
                snapshot.contacts.push(HiddenContact {
                    account_id: row.account.clone(),
                    name: normalized_name(&row.name, &row.account),
                });
            }
        }
        if snapshot.contacts.len() + snapshot.deleted.len() > MAX_CONTACTS {
            return Err("Too many hidden contacts".into());
        }
        snapshot
            .contacts
            .sort_by(|a, b| a.account_id.cmp(&b.account_id));
        self.save(&snapshot)?;
        Ok(snapshot)
    }
}

#[tauri::command]
pub async fn get_hidden_contacts(
    owner: String,
    app: tauri::State<'_, crate::state::AppState>,
    policy: tauri::State<'_, HiddenContactsState>,
) -> Result<HiddenContactsSnapshot, String> {
    let session = app.session().clone();
    let policy = policy.inner().clone();
    tokio::task::spawn_blocking(move || {
        policy.transaction(&session, &owner, || policy.load(&owner))
    })
    .await
    .map_err(|_| "Hidden contacts task failed")?
}

#[tauri::command]
pub async fn set_contact_hidden(
    owner: String,
    account: String,
    hidden: bool,
    name: String,
    app: tauri::State<'_, crate::state::AppState>,
    node: tauri::State<'_, crate::chat_commands::NodeState>,
    policy: tauri::State<'_, HiddenContactsState>,
) -> Result<HiddenContactsSnapshot, String> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &node, &owner)
        .map_err(|error| error.to_string())?;
    let lease = admission.lease().clone();
    let node = node.inner().clone();
    #[cfg(test)]
    let captured = app.owner_command_captured.clone();
    let app = app.inner().clone();
    let policy = policy.inner().clone();
    tokio::task::spawn_blocking(move || {
        let guard = if hidden {
            #[cfg(test)]
            if let Some(entered) = captured.lock().unwrap().as_ref() {
                let _ = entered.send("contact-runtime-wait");
            }
            Some(node.0.blocking_read())
        } else {
            None
        };
        policy.transaction_for_lease(&app, &lease, || {
            let own = guard.as_ref().and_then(|guard| guard.as_ref());
            if hidden {
                node.check_installation(&lease)
                    .map_err(|error| error.to_string())?;
                if own.and_then(|runtime| runtime.host_account_id()) != Some(owner.as_str()) {
                    return Err("Hiding a contact requires this account's running node".into());
                }
            }
            policy.update_unlocked(
                &owner,
                &account,
                hidden,
                &name,
                own.map(|runtime| runtime.account_id()),
            )
        })
    })
    .await
    .map_err(|_| "Hidden contacts task failed")?
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HiddenContactChange {
    pub account: String,
    pub hidden: bool,
    pub name: String,
}

#[tauri::command]
pub async fn set_contact_hidden_batch(
    owner: String,
    expected_account: String,
    records: Vec<HiddenContactChange>,
    expected: std::collections::HashMap<String, Option<String>>,
    app: tauri::State<'_, crate::state::AppState>,
    node: tauri::State<'_, crate::chat_commands::NodeState>,
    policy: tauri::State<'_, HiddenContactsState>,
) -> Result<HiddenContactsSnapshot, String> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &node, &owner)
        .map_err(|e| e.to_string())?;
    let lease = admission.lease().clone();
    let node = node.inner().clone();
    let app = app.inner().clone();
    let policy = policy.inner().clone();
    // Bound awaiting the runtime before entering the synchronous persistence transaction.
    let runtime = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        node.0.clone().read_owned(),
    )
    .await
    .map_err(|_| "Runtime observation timed out")?;
    tokio::task::spawn_blocking(move || {
        policy.transaction_for_lease(&app, &lease, || {
            node.check_installation(&lease).map_err(|e| e.to_string())?;
            let own = runtime
                .as_ref()
                .ok_or("Contact synchronization requires a running node")?;
            if own.host_account_id() != Some(owner.as_str()) || own.account_id() != expected_account
            {
                return Err("Contact preferences belong to another account".into());
            }
            policy.update_batch_unlocked(&owner, &records, Some(own.account_id()), Some(&expected))
        })
    })
    .await
    .map_err(|_| "Hidden contacts task failed")?
}

#[cfg(test)]
mod batch_tests {
    use super::*;
    #[test]
    fn linked_batch_rejects_a_stale_crypto_account_for_the_same_local_owner() {
        use tauri::Manager;
        let dir = tempfile::tempdir().unwrap();
        let app_state =
            crate::state::AppState::new(crate::services::auth_service::AuthService::new(Arc::new(
                mesh_talk_core::identity::manager::IdentityManager::new(
                    mesh_talk_core::storage::file_manager::FileManager::new(dir.path().to_owned()),
                ),
            )));
        let signed_in = super::tests::session().get().unwrap();
        app_state
            .session()
            .set(signed_in.token, signed_in.user, signed_in.password);
        let node = crate::chat_commands::NodeState::empty();
        let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            dir.path(),
            "alice",
            "Alice",
            "password",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        let current_account = runtime.account_id().to_owned();
        let previous_account = "f".repeat(32);
        assert_ne!(current_account, previous_account);
        *node.0.blocking_write() = Some(runtime);
        let policy = HiddenContactsState::new(dir.path().to_owned());
        let app = tauri::test::mock_builder()
            .manage(app_state)
            .manage(node.clone())
            .manage(policy.clone())
            .invoke_handler(tauri::generate_handler![set_contact_hidden_batch])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let rejected: Vec<_> = [false, true].into_iter().map(|hidden| {
            tauri::test::get_ipc_response(&webview, tauri::webview::InvokeRequest {
                cmd: "set_contact_hidden_batch".into(), callback: tauri::ipc::CallbackFn(0), error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) { "http://tauri.localhost" } else { "tauri://localhost" }.parse().unwrap(),
                body: tauri::ipc::InvokeBody::Json(serde_json::json!({
                    "owner": "alice", "expectedAccount": previous_account,
                    "records": [{ "account": "b".repeat(32), "hidden": hidden, "name": "Old account" }],
                    "expected": { "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb": null }
                })), headers: Default::default(), invoke_key: tauri::test::INVOKE_KEY.into(),
            }).is_err()
        }).collect();
        tauri::async_runtime::block_on(node.0.blocking_write().take().unwrap().stop());
        assert!(
            rejected.into_iter().all(|rejected| rejected),
            "both hidden updates and clears must reject an old cryptographic account"
        );
        assert!(
            !policy.path("alice").exists(),
            "stale import must not write the retained local user's preferences"
        );
        let _ = app.state::<HiddenContactsState>();
    }

    #[test]
    fn linked_hidden_batch_cannot_restore_a_contact_deleted_after_its_snapshot() {
        for hidden in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let store = HiddenContactsState::new(dir.path().to_owned());
            let own = "a".repeat(32);
            let account = "b".repeat(32);
            // A visible contact and a deleted contact both have no hidden-name
            // entry. The import must also honor the local deletion tombstone.
            let expected = std::collections::HashMap::from([(account.clone(), None)]);
            store
                .delete_unlocked("owner", &account, &own, false, || Ok(()))
                .unwrap();
            let rows = [HiddenContactChange {
                account: account.clone(),
                hidden,
                name: "Remote".into(),
            }];
            assert!(
                store
                    .update_batch_unlocked("owner", &rows, Some(&own), Some(&expected))
                    .is_err()
            );
            let local = store.load("owner").unwrap();
            assert!(local.deleted.contains(&account));
            assert!(local.contacts.is_empty());
            // An explicit local restore still has its established semantics.
            store
                .update_unlocked("owner", &account, false, "Local", Some(&own))
                .unwrap();
            assert!(!store.load("owner").unwrap().deleted.contains(&account));
        }
    }

    #[test]
    fn hidden_batch_rejects_self_before_persisting_other_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let own = "a".repeat(32);
        let rows = vec![
            HiddenContactChange {
                account: "b".repeat(32),
                hidden: true,
                name: "Bob".into(),
            },
            HiddenContactChange {
                account: own.clone(),
                hidden: true,
                name: "Me".into(),
            },
        ];
        assert!(
            store
                .update_batch_unlocked("owner", &rows, Some(&own), None)
                .is_err()
        );
        assert!(!store.path("owner").exists());
        let result = store
            .update_batch_unlocked("owner", &rows[..1], Some(&own), None)
            .unwrap();
        assert_eq!(result.contacts.len(), 1);
    }
    #[test]
    fn linked_hidden_batch_preserves_a_newer_local_change() {
        let dir = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(dir.path().to_owned());
        let own = "a".repeat(32);
        let account = "b".repeat(32);
        let rows = [HiddenContactChange {
            account: account.clone(),
            hidden: true,
            name: "Local".into(),
        }];
        store
            .update_batch_unlocked("owner", &rows, Some(&own), None)
            .unwrap();
        let expected = std::collections::HashMap::from([(account.clone(), None)]);
        assert!(
            store
                .update_batch_unlocked(
                    "owner",
                    &[HiddenContactChange {
                        name: "Remote".into(),
                        ..rows[0].clone()
                    }],
                    Some(&own),
                    Some(&expected)
                )
                .is_err()
        );
        assert_eq!(store.load("owner").unwrap().contacts[0].name, "Local");
    }
}

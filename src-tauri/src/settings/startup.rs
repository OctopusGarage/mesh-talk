//! Transactional switching of the installation's saved-session protection policy.
use super::*;
use crate::{app_lock::SensitiveAction, state::AppState};

pub(super) async fn change<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    auth: AppState,
    state: SettingsState,
    owner: Option<String>,
    next: AppSettings,
    password: Option<String>,
) -> Result<(), CommandError> {
    if !cfg!(target_os = "macos") && !cfg!(test) {
        return Err(CommandError::InvalidInput(
            "Startup verification is a macOS setting".into(),
        ));
    }
    let owner = owner.ok_or_else(|| {
        CommandError::Authorization("Sign in to change startup verification".into())
    })?;
    let lease = auth
        .session()
        .capture()
        .map_err(CommandError::Authorization)?;
    if lease.owner() != owner {
        return Err(CommandError::Authorization(
            "account session mismatch".into(),
        ));
    }
    let save = state.1.clone().lock_owned().await;
    let (revision, previous) = state.snapshot(&owner)?;
    let mut comparable = next.clone();
    comparable.startup_verification = previous.startup_verification;
    comparable.last_user = previous.last_user.clone();
    if serde_json::to_value(&comparable).unwrap() != serde_json::to_value(&previous).unwrap() {
        return Err(CommandError::InvalidInput(
            "Change startup verification separately from other settings".into(),
        ));
    }
    if previous.startup_verification == next.startup_verification {
        return Ok(());
    }
    let proof = SensitiveAction::verify(
        &auth,
        &lease,
        password,
        "change Mesh-Talk startup verification",
    )
    .await?;
    let ordering = auth.auth_operation.clone().lock_owned().await;
    // The actual worker retains both ordering guards even if the IPC future is dropped.
    tokio::task::spawn_blocking(move || {
        let (_save, _ordering) = (save, ordering);
        proof.checked(|| state.commit_current(&owner, revision, || Ok(())))?;
        let (username, secret) = auth
            .session()
            .matching(&lease, |info| {
                (
                    info.user.login_name().to_owned(),
                    zeroize::Zeroizing::new(info.account_login_password.clone()),
                )
            })
            .map_err(CommandError::Authorization)?;
        let mut next = previous.clone();
        next.startup_verification = !previous.startup_verification;
        let mut global = GlobalSettings::from(&next);
        if previous.stay_signed_in {
            global.last_user = Some(username.clone());
            next.last_user = global.last_user.clone();
        }
        #[cfg(test)]
        let path = state
            .2
            .clone()
            .or_else(|| config_store::config_path(&app, SETTINGS_FILE));
        #[cfg(not(test))]
        let path = config_store::config_path(&app, SETTINGS_FILE);
        let path = path.ok_or_else(|| CommandError::Io("No settings directory".into()))?;
        if path.is_dir() {
            return Err(CommandError::Io(
                "Settings destination is a directory".into(),
            ));
        }
        let prepared = config_store::prepare_account_write(&path, &global)?;
        let rollback =
            config_store::prepare_account_write(&path, &GlobalSettings::from(&previous))?;
        if previous.stay_signed_in
            && !crate::session_store::save_with_policy(
                &username,
                &secret,
                next.startup_verification,
            )
        {
            if !next.startup_verification {
                crate::session_store::clear_local(&username).map_err(CommandError::Io)?;
            }
            return Err(CommandError::Service(
                "Could not save automatic sign-in credentials; startup verification unchanged"
                    .into(),
            ));
        }
        let result = auth
            .session()
            .matching(&lease, |_| {
                proof.commit(|| {
                    if state.snapshot(&owner)?.0 != revision {
                        return Err(CommandError::Authorization("settings superseded".into()));
                    }
                    // No native calls under authority. The small local-file removal
                    // occurs only after final authorization, so a cancelled prompt
                    // or app lock cannot destroy the previous recovery entry.
                    if next.startup_verification
                        && let Err(error) = crate::session_store::clear_local(&username)
                    {
                        // Removal may have succeeded before its directory sync
                        // failed. Keep the selected OFF provider recoverable.
                        if previous.stay_signed_in
                            && !crate::session_store::save_with_policy(&username, &secret, false)
                        {
                            log::error!("Could not restore local saved login after failed removal");
                        }
                        return Err(CommandError::Io(error));
                    }
                    if let Err(error) = prepared.commit_durable() {
                        // Rename may have succeeded before directory sync failed.
                        // Restore the old selection before its recovery credential.
                        let rolled_back = rollback.commit_durable().is_ok();
                        if !rolled_back {
                            log::error!(
                                "Could not restore startup policy after failed durable commit"
                            );
                        }
                        if rolled_back
                            && next.startup_verification
                            && previous.stay_signed_in
                            && !crate::session_store::save_with_policy(&username, &secret, false)
                        {
                            log::error!(
                                "Could not restore local saved login after failed settings commit"
                            );
                        }
                        return Err(error);
                    }
                    state.update_for_owner(&owner, next)?;
                    Ok(())
                })
            })
            .map_err(CommandError::Authorization)
            .and_then(|result| result);
        if result.is_err() {
            // Authentication ordering prevents replacement login writes from being
            // removed by this compensation after the originating session retires.
            if global.startup_verification {
                // Preserve the selected local provider if the originating session
                // still exists (for example, the app merely locked). Retirement
                // cleanup owns removal; a replacement login cannot overtake us.
                if auth.session().matching(&lease, |_| ()).is_err() {
                    crate::session_store::clear(&username);
                }
            } else {
                crate::session_store::clear_local(&username).map_err(CommandError::Io)?;
            }
        }
        result
    })
    .await
    .map_err(|_| CommandError::Service("Startup verification worker stopped".into()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};

    fn fixture(
        name: &str,
    ) -> (
        tempfile::TempDir,
        tauri::App<tauri::test::MockRuntime>,
        AppState,
        SettingsState,
        String,
    ) {
        let root = tempfile::tempdir().unwrap();
        let auth = AppState::new(AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().into()),
            ),
        )));
        let username = format!("startup-{name}");
        let user = User {
            user_id: username.clone(),
            name: username.clone(),
            account_login_name: None,
            display_name: name.into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        };
        auth.session()
            .publish("token".into(), user, "credential".into());
        let state = SettingsState::isolated(root.path().join("settings.json"));
        state.activate_at(&username, &root.path().join("account.json"));
        let app = tauri::test::mock_builder()
            .manage(auth.clone())
            .manage(auth.app_lock().clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        (root, app, auth, state, username)
    }

    #[tokio::test]
    async fn startup_verification_switch_migrates_without_reading_old_keychain_and_reprotects() {
        let (root, app, auth, state, username) = fixture("roundtrip");
        crate::session_store::before_next_io(
            &username,
            "load",
            Box::new(|| panic!("migration read old Keychain")),
        );
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = false;
        change(
            app.handle().clone(),
            auth.clone(),
            state.clone(),
            Some(username.clone()),
            next,
            None,
        )
        .await
        .unwrap();
        assert!(!state.get().startup_verification);
        assert_eq!(
            crate::session_store::load_with_policy(&username, false).as_deref(),
            Some("credential")
        );
        let persisted: GlobalSettings =
            serde_json::from_slice(&std::fs::read(root.path().join("settings.json")).unwrap())
                .unwrap();
        assert!(!persisted.startup_verification);
        assert_eq!(persisted.last_user.as_deref(), Some(username.as_str()));
        // Switching on creates protected credentials and removes local recovery.
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = true;
        change(
            app.handle().clone(),
            auth,
            state.clone(),
            Some(username.clone()),
            next,
            None,
        )
        .await
        .unwrap();
        assert!(state.get().startup_verification);
        assert!(crate::session_store::load_with_policy(&username, false).is_none());
        crate::session_store::clear(&username);
    }

    #[tokio::test]
    async fn startup_verification_failed_protection_commit_preserves_local_recovery() {
        let (root, app, auth, state, username) = fixture("reprotect-failure");
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = false;
        change(
            app.handle().clone(),
            auth.clone(),
            state.clone(),
            Some(username.clone()),
            next,
            None,
        )
        .await
        .unwrap();
        std::fs::remove_file(root.path().join("settings.json")).unwrap();
        std::fs::create_dir(root.path().join("settings.json")).unwrap();
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = true;
        assert!(
            change(
                app.handle().clone(),
                auth.clone(),
                state.clone(),
                Some(username.clone()),
                next.clone(),
                None
            )
            .await
            .is_err()
        );
        assert!(!state.get().startup_verification);
        assert_eq!(
            crate::session_store::load_with_policy(&username, false).as_deref(),
            Some("credential")
        );
        std::fs::remove_dir(root.path().join("settings.json")).unwrap();
        config_store::write_account(
            &root.path().join("settings.json"),
            &GlobalSettings::from(&state.get()),
        )
        .unwrap();
        // An app lock during protected-provider work must reject the stale proof
        // without deleting the selected local recovery entry.
        let handle = app.handle().clone();
        let locked = auth.clone();
        crate::session_store::before_next_io(
            &username,
            "save",
            Box::new(move || {
                use tauri::Manager;
                crate::app_lock::lock_app(handle.state(), handle.state()).unwrap();
            }),
        );
        auth.app_lock().set_identity_verifier(|| Ok(()));
        assert!(
            change(
                app.handle().clone(),
                auth,
                state.clone(),
                Some(username.clone()),
                next,
                None
            )
            .await
            .is_err()
        );
        assert!(
            locked.app_lock().is_locked(),
            "protected save must reach the real lock trigger"
        );
        assert!(!state.get().startup_verification);
        assert_eq!(
            crate::session_store::load_with_policy(&username, false).as_deref(),
            Some("credential")
        );
        crate::session_store::clear(&username);
    }

    #[tokio::test]
    async fn startup_verification_partial_removal_failure_restores_selected_provider() {
        let (_root, app, auth, state, username) = fixture("partial-clear");
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = false;
        change(
            app.handle().clone(),
            auth.clone(),
            state.clone(),
            Some(username.clone()),
            next,
            None,
        )
        .await
        .unwrap();
        crate::session_store::fail_local_clear_after_removal(&username);
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let mut next = state.get();
        next.startup_verification = true;
        assert!(
            change(
                app.handle().clone(),
                auth.clone(),
                state.clone(),
                Some(username.clone()),
                next.clone(),
                None
            )
            .await
            .is_err()
        );
        assert!(!state.get().startup_verification);
        assert_eq!(
            crate::session_store::load_with_policy(&username, false).as_deref(),
            Some("credential")
        );
        auth.app_lock().set_identity_verifier(|| Ok(()));
        change(
            app.handle().clone(),
            auth,
            state.clone(),
            Some(username.clone()),
            next,
            None,
        )
        .await
        .unwrap();
        assert!(state.get().startup_verification);
        assert!(crate::session_store::load_with_policy(&username, false).is_none());
        crate::session_store::clear(&username);
    }

    #[tokio::test]
    async fn startup_verification_denial_and_persistence_failure_preserve_policy() {
        let (root, app, auth, state, username) = fixture("denied");
        auth.app_lock()
            .set_identity_verifier(|| Err(CommandError::Authentication("cancelled".into())));
        let mut next = state.get();
        next.startup_verification = false;
        assert!(
            change(
                app.handle().clone(),
                auth.clone(),
                state.clone(),
                Some(username.clone()),
                next.clone(),
                None
            )
            .await
            .is_err()
        );
        assert!(state.get().startup_verification);
        assert!(crate::session_store::load_with_policy(&username, false).is_none());
        assert!(!root.path().join("settings.json").exists());
        // An invalid destination must fail before creating recovery credentials.
        std::fs::create_dir(root.path().join("settings.json")).unwrap();
        auth.app_lock().set_identity_verifier(|| Ok(()));
        assert!(
            change(
                app.handle().clone(),
                auth,
                state.clone(),
                Some(username.clone()),
                next,
                None
            )
            .await
            .is_err()
        );
        assert!(state.get().startup_verification);
        assert!(crate::session_store::load_with_policy(&username, false).is_none());
    }

    #[tokio::test]
    async fn startup_verification_retired_write_is_compensated_before_worker_finishes() {
        let (_root, app, auth, state, username) = fixture("retired");
        auth.app_lock().set_identity_verifier(|| Ok(()));
        let retired = auth.clone();
        crate::session_store::before_next_io(
            &format!("local:{username}"),
            "save",
            Box::new(move || retired.session().clear()),
        );
        let mut next = state.get();
        next.startup_verification = false;
        assert!(
            change(
                app.handle().clone(),
                auth,
                state.clone(),
                Some(username.clone()),
                next,
                None
            )
            .await
            .is_err()
        );
        assert!(state.get().startup_verification);
        assert!(crate::session_store::load_with_policy(&username, false).is_none());
    }
}

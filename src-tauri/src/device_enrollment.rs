//! Accountless device enrollment; publication reuses the recovery transaction.

use crate::{
    chat_commands::NodeState,
    commands::{CommandError, LoginResult},
    state::AppState,
};
use mesh_talk_core::{
    identity::{device::DeviceIdentity, manager::IdentityManager},
    node::{DeviceEnrollment, EnrolledDevice, EnrollmentSource, Node, SyncCategory},
    storage::file_manager::FileManager,
};
use serde::Serialize;
use std::{
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};
use zeroize::Zeroizing;

#[derive(Clone, Default)]
pub(crate) struct EnrollmentState(Arc<Mutex<Option<Attempt>>>);
struct Attempt {
    id: String,
    generation: u64,
    device: Arc<DeviceEnrollment>,
    sources: Vec<(String, EnrollmentSource)>,
    publishing: bool,
}
#[derive(Serialize)]
pub struct EnrollmentStarted {
    id: String,
}
#[derive(Serialize)]
pub struct EnrollmentSourceInfo {
    id: String,
    device_id: String,
    account_id: String,
    nickname: String,
    addresses: Vec<String>,
}
fn opaque_id() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}
fn expired() -> CommandError {
    CommandError::Authorization("Device enrollment expired; start again".into())
}
fn signed_out(app: &AppState, generation: u64) -> Result<(), CommandError> {
    app.session()
        .without_session(generation, || ())
        .map_err(CommandError::Authorization)
}

#[tauri::command]
pub async fn begin_device_enrollment(
    app_state: tauri::State<'_, AppState>,
) -> Result<EnrollmentStarted, CommandError> {
    let _auth = app_state.auth_operation.lock().await;
    let generation = app_state.session().generation();
    signed_out(&app_state, generation)?;
    let id = opaque_id();
    *app_state.enrollment.0.lock().map_err(|_| expired())? = Some(Attempt {
        id: id.clone(),
        generation,
        device: Arc::new(DeviceEnrollment::new(DeviceIdentity::generate())),
        sources: Vec::new(),
        publishing: false,
    });
    Ok(EnrollmentStarted { id })
}

#[tauri::command]
pub async fn probe_device_enrollment(
    app_state: tauri::State<'_, AppState>,
    enrollment_id: String,
    targets: Vec<String>,
) -> Result<Vec<EnrollmentSourceInfo>, CommandError> {
    if targets.len() > 64 {
        return Err(CommandError::Validation(
            "Too many discovery addresses".into(),
        ));
    }
    let targets: Vec<SocketAddr> = targets
        .iter()
        .map(|target| {
            target.parse().map_err(|_| {
                CommandError::Validation("Enter an IP address and discovery port".into())
            })
        })
        .collect::<Result<_, _>>()?;
    let (device, generation) = {
        let state = app_state.enrollment.0.lock().map_err(|_| expired())?;
        let attempt = state
            .as_ref()
            .filter(|a| a.id == enrollment_id && !a.publishing)
            .ok_or_else(expired)?;
        (attempt.device.clone(), attempt.generation)
    };
    signed_out(&app_state, generation)?;
    let sources = device
        .probe_sources(&targets, Duration::from_secs(3))
        .await?;
    signed_out(&app_state, generation)?;
    let mut state = app_state.enrollment.0.lock().map_err(|_| expired())?;
    let attempt = state
        .as_mut()
        .filter(|a| a.id == enrollment_id && !a.publishing)
        .ok_or_else(expired)?;
    attempt.sources = sources
        .into_iter()
        .map(|source| (opaque_id(), source))
        .collect();
    Ok(attempt
        .sources
        .iter()
        .map(|(id, source)| EnrollmentSourceInfo {
            id: id.clone(),
            device_id: source.public_identity().user_id(),
            account_id: source.account_id(),
            nickname: source.name().into(),
            addresses: vec![source.address().to_string()],
        })
        .collect())
}

#[tauri::command]
pub fn cancel_device_enrollment(
    app_state: tauri::State<'_, AppState>,
    enrollment_id: String,
) -> Result<(), CommandError> {
    let mut state = app_state.enrollment.0.lock().map_err(|_| expired())?;
    if let Some(attempt) = state.as_ref().filter(|a| a.id == enrollment_id) {
        if attempt.publishing {
            return Err(CommandError::Validation(
                "Finishing device enrollment".into(),
            ));
        }
        *state = None;
    }
    Ok(())
}

/// No live users/accounts directory exists until all source proof and staged
/// credentials have been verified. Caller holds lifecycle and auth authority.
fn stage_enrolled(
    root: &Path,
    enrolled: &EnrolledDevice,
    publish: impl FnOnce(&Path, &str, &str) -> Result<(), CommandError>,
) -> Result<crate::shared_account_login::AccountLogin, CommandError> {
    let credentials =
        crate::shared_account_login::AccountLogin::decode(enrolled.access_snapshot())?;
    let profile = enrolled.profile().ok_or_else(|| {
        CommandError::LinkUpgradeRequired(
            "Update the source device to transfer its verified profile".into(),
        )
    })?;
    let identity_password = Zeroizing::new(opaque_id());
    let node_password = Zeroizing::new(opaque_id());
    std::fs::create_dir_all(root)
        .map_err(|_| CommandError::Io("Could not create device storage".into()))?;
    let stage = tempfile::Builder::new()
        .prefix("recovery-stage-")
        .tempdir_in(root)
        .map_err(|_| CommandError::Io("Could not stage linked device".into()))?;
    let manager = IdentityManager::new(FileManager::new(stage.path().to_path_buf()));
    let user = manager
        .register_user(&credentials.login_name, &identity_password)
        .map_err(|_| CommandError::Auth("Could not prepare linked sign-in".into()))?;
    manager
        .set_display_name(
            &credentials.login_name,
            &identity_password,
            &profile.display_name,
        )
        .map_err(|_| CommandError::Auth("Invalid source nickname".into()))?;
    let directory = stage.path().join("accounts").join(&user.user_id);
    std::fs::create_dir_all(&directory)
        .map_err(|_| CommandError::Io("Could not stage linked account".into()))?;
    enrolled.persist_profile(&directory, &node_password)?;
    manager
        .install_account_login(
            &credentials.login_name,
            &identity_password,
            &node_password,
            &enrolled.account_id(),
            &credentials.login_name,
            &credentials.password,
        )
        .map_err(|_| CommandError::Auth("Could not prepare shared sign-in".into()))?;
    let verified = manager
        .authenticate_account_login(&credentials.login_name, &credentials.password)
        .map_err(|_| CommandError::Auth("Shared sign-in verification failed".into()))?;
    if verified.user.user_id != user.user_id {
        return Err(expired());
    }
    let reopened = Node::open_recovery_profile(&directory, &node_password)
        .map_err(|_| CommandError::Crypto("Linked account verification failed".into()))?;
    if reopened.account_id() != enrolled.account_id() {
        return Err(expired());
    }
    if credentials.sync_all {
        for category in [
            SyncCategory::Attachments,
            SyncCategory::Profile,
            SyncCategory::Appearance,
            SyncCategory::Contacts,
        ] {
            reopened.set_device_sync_category(category, true)?;
        }
    }
    crate::shared_account_login::stage_applied_login(&directory, enrolled.access_snapshot())?;
    reopened
        .close_profile()
        .map_err(|_| CommandError::Crypto("Linked profile still has active storage work".into()))?;
    publish(stage.path(), &credentials.login_name, &user.user_id)?;
    Ok(credentials)
}

fn publish_verified(
    root: &Path,
    app: &AppState,
    enrollment_id: &str,
    generation: u64,
    stage: &Path,
    username: &str,
    owner: &str,
) -> Result<(), CommandError> {
    let mut state = app.enrollment.0.lock().map_err(|_| expired())?;
    let attempt = state
        .as_mut()
        .filter(|a| a.id == enrollment_id && a.generation == generation && !a.publishing)
        .ok_or_else(expired)?;
    signed_out(app, generation)?;
    attempt.publishing = true;
    let result = app
        .auth_service()
        .identity_manager()
        .publish_new_account_login(username, || {
            app.session()
                .without_session(generation, || {
                    crate::recovery::publish_staged_profile(root, stage, username, owner)
                })
                .map_err(CommandError::Authorization)?
        });
    let result = match result {
        Ok(result) => result,
        Err(_) => Err(CommandError::Validation(
            "This login name already exists on this device; sign in to that local user to link it"
                .into(),
        )),
    };
    if result.is_err() {
        attempt.publishing = false;
    }
    result
}

#[tauri::command]
pub async fn complete_device_enrollment<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
    enrollment_id: String,
    source_id: String,
    code: String,
    include_history: bool,
) -> Result<LoginResult, CommandError> {
    let code = Zeroizing::new(code);
    let (device, source, generation) = {
        let state = app_state.enrollment.0.lock().map_err(|_| expired())?;
        let attempt = state
            .as_ref()
            .filter(|a| a.id == enrollment_id && !a.publishing)
            .ok_or_else(expired)?;
        let source = attempt
            .sources
            .iter()
            .find(|(id, _)| id == &source_id)
            .ok_or_else(expired)?
            .1
            .clone();
        (attempt.device.clone(), source, attempt.generation)
    };
    signed_out(&app_state, generation)?;
    let enrolled = device.enroll(&source, &code, include_history).await?;
    let auth = app_state.auth_operation.clone().lock_owned().await;
    let lifecycle = node_state.lock_profile_lifecycle().await;
    signed_out(&app_state, generation)?;
    let root = crate::recovery::data_root(&node_state);
    let app = app_state.inner().clone();
    let (user, token, lease, node_password, credentials) = tokio::task::spawn_blocking(move || {
        let _auth = auth;
        let _lifecycle = lifecycle;
        let credentials = stage_enrolled(&root, &enrolled, |stage, username, owner| {
            publish_verified(
                &root,
                &app,
                &enrollment_id,
                generation,
                stage,
                username,
                owner,
            )
        })?;
        let (user, token, lease) = app
            .auth_service()
            .login_session(credentials.login_name.clone(), credentials.password.clone())
            .map_err(|_| {
                CommandError::Auth(
                    "Device linked; sign in with the source account to continue".into(),
                )
            })?;
        let node_password = app
            .session()
            .matching(&lease, |info| info.node_storage_password.clone())
            .map_err(CommandError::Authorization)?;
        *app.enrollment.0.lock().map_err(|_| expired())? = None;
        Ok::<_, CommandError>((user, token, lease, node_password, credentials))
    })
    .await
    .map_err(|_| CommandError::Internal("Device enrollment worker stopped".into()))??;
    let saved_session = app_state
        .session()
        .matching(&lease, |_| {
            settings_state.activate(lease.owner());
            crate::commands::persist_saved_login(
                &app_handle,
                &settings_state,
                &user,
                &credentials.password,
            )
        })
        .map_err(CommandError::Authorization)?;
    crate::runtime_bootstrap::spawn_node_runtime(
        app_handle,
        lease,
        node_password,
        node_state.inner().clone(),
        app_state.session().clone(),
    );
    Ok(LoginResult {
        success: true,
        token: Some(token),
        user: Some(user.into()),
        saved_session: Some(saved_session),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::node::NodeRuntime;

    #[tokio::test]
    async fn verified_enrollment_publishes_shared_login_and_profile_without_temporary_account() {
        assert_verified_enrollment(true).await;
        assert_verified_enrollment(false).await;
    }

    async fn assert_verified_enrollment(include_history: bool) {
        let source_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = socket.local_addr().unwrap().port();
        drop(socket);
        let runtime = NodeRuntime::start(
            source_root.path(),
            "00000000-0000-4000-8000-000000000001",
            "Source nickname",
            "source storage password",
            port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let source = runtime.handle();
        source.set_profile_display_name("Source nickname").unwrap();
        let password = "  shared raw password  ";
        let payload = serde_json::to_vec(&serde_json::json!({"version":1,"login_name":"Federer","password":password,"sync_all":true})).unwrap();
        source
            .publish_account_access_if(payload, |commit| commit())
            .unwrap();
        let announce = source.signed_announce("Source nickname", runtime.listen_tcp_port());
        let selected =
            EnrollmentSource::from_announce(announce, "127.0.0.1".parse().unwrap()).unwrap();
        let code = runtime.start_linking();
        let enrollment = DeviceEnrollment::new(DeviceIdentity::generate());
        assert!(!target_root.path().join("users").exists());
        assert!(!target_root.path().join("accounts").exists());
        let enrolled = enrollment
            .enroll(&selected, &code, include_history)
            .await
            .unwrap();
        assert!(
            !target_root.path().join("users").exists(),
            "network pairing cannot publish local users"
        );
        assert!(stage_enrolled(target_root.path(), &enrolled, |_, _, _| Err(expired())).is_err());
        assert!(
            !target_root.path().join("users").exists(),
            "cancelled enrollment must not reserve the login name"
        );
        assert_eq!(
            std::fs::read_dir(target_root.path()).unwrap().count(),
            0,
            "staged secrets removed after cancellation"
        );
        let manager = Arc::new(IdentityManager::new(FileManager::new(
            target_root.path().to_path_buf(),
        )));
        let app = AppState::new(crate::services::auth_service::AuthService::new(
            manager.clone(),
        ));
        let generation = app.session().generation();
        *app.enrollment.0.lock().unwrap() = Some(Attempt {
            id: "attempt".into(),
            generation,
            device: Arc::new(enrollment),
            sources: Vec::new(),
            publishing: false,
        });
        app.session().clear();
        assert!(
            stage_enrolled(target_root.path(), &enrolled, |stage, name, owner| {
                publish_verified(
                    target_root.path(),
                    &app,
                    "attempt",
                    generation,
                    stage,
                    name,
                    owner,
                )
            })
            .is_err(),
            "a session transition during preparation must prevent publication"
        );
        assert!(!target_root.path().join("users").exists());
        let generation = app.session().generation();
        app.enrollment
            .0
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .generation = generation;
        assert!(
            stage_enrolled(target_root.path(), &enrolled, |stage, name, owner| {
                publish_verified(
                    target_root.path(),
                    &app,
                    "cancelled-attempt",
                    generation,
                    stage,
                    name,
                    owner,
                )
            })
            .is_err(),
            "cancelled/replaced attempt cannot publish into the same signed-out session"
        );
        stage_enrolled(target_root.path(), &enrolled, |stage, name, owner| {
            publish_verified(
                target_root.path(),
                &app,
                "attempt",
                generation,
                stage,
                name,
                owner,
            )
        })
        .unwrap();
        let resolved = manager
            .authenticate_account_login("Federer", password)
            .unwrap();
        assert_eq!(resolved.user.effective_display_name(), "Source nickname");
        assert_ne!(resolved.identity_password, password);
        assert_ne!(resolved.node_storage_password, password);
        assert!(
            manager
                .authenticate_account_login("Federer", password.trim())
                .is_err()
        );
        let directory = target_root
            .path()
            .join("accounts")
            .join(&resolved.user.user_id);
        let reopened =
            Node::open_recovery_profile(&directory, &resolved.node_storage_password).unwrap();
        assert_eq!(reopened.account_id(), source.account_id());
        assert_eq!(
            reopened.user_id(),
            enrolled.device_public_identity().user_id()
        );
        assert_ne!(reopened.user_id(), source.user_id());
        let policy = reopened.device_sync_policy();
        assert_eq!(
            policy.history, include_history,
            "explicit history consent survives source full-sync defaults"
        );
        assert!(policy.attachments && policy.profile && policy.contacts && policy.appearance);
        let applied: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join("account-login-applied.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(applied["revision"], enrolled.access_snapshot().revision());
        assert_eq!(applied["account_id"], enrolled.account_id());
        assert_eq!(
            reopened
                .account_profile(&source.account_id())
                .unwrap()
                .unwrap()
                .display_name,
            "Source nickname"
        );
        assert!(
            manager
                .publish_new_account_login("Federer", || panic!("must not overwrite existing user"))
                .is_err()
        );
        runtime.stop().await;
    }
}

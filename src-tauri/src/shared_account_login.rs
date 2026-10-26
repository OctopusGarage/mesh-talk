//! Private desktop account sign-in synchronization. Public profiles never carry credentials.
use crate::{
    chat_commands::{NodeState, RuntimeAuthority},
    commands::CommandError,
    runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::{
    identity::errors::IdentityError,
    node::{AccountAccessSnapshot, NodeError, NodeRuntime, SyncCategory},
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tauri::{Emitter, Manager};
use zeroize::{Zeroize, Zeroizing};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AccountLogin {
    version: u8,
    pub(crate) login_name: String,
    pub(crate) password: String,
    /// Explicit source consent to enable all five categories once for this revision.
    #[serde(default)]
    pub(crate) sync_all: bool,
}
impl Drop for AccountLogin {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}
impl AccountLogin {
    pub(crate) fn decode(snapshot: &AccountAccessSnapshot) -> Result<Self, CommandError> {
        let value: Self = serde_json::from_slice(snapshot.payload())
            .map_err(|_| CommandError::Validation("Invalid private account login record".into()))?;
        if value.version != 1
            || !mesh_talk_core::identity::user::User::validate_username(&value.login_name)
            || value.password.trim().is_empty()
        {
            return Err(CommandError::Validation(
                "Unsupported private account login record".into(),
            ));
        }
        Ok(value)
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Applied {
    account_id: String,
    revision: u64,
}

/// Only for a verified, unpublished profile whose canonical envelope and local
/// policy have already been installed. Publication commits this marker with both.
pub(crate) fn stage_applied_login(
    directory: &Path,
    snapshot: &AccountAccessSnapshot,
) -> Result<(), CommandError> {
    crate::config_store::write_account(
        &directory.join("account-login-applied.json"),
        &Applied {
            account_id: snapshot.account_id(),
            revision: snapshot.revision(),
        },
    )
}

#[derive(Serialize)]
pub struct AccountLoginSyncStatus {
    pub account_id: String,
    pub available_revision: Option<u64>,
    pub applied_revision: Option<u64>,
    pub devices: Vec<mesh_talk_core::node::AccountAccessSyncStatus>,
}

pub(crate) fn sync_status(
    root: &Path,
    owner: &str,
    runtime: &NodeRuntime,
) -> Result<AccountLoginSyncStatus, CommandError> {
    let stamp: Applied =
        crate::config_store::read_account(&path(root, owner, "account-login-applied.json")?);
    let account_id = runtime.account_id();
    Ok(AccountLoginSyncStatus {
        available_revision: runtime
            .handle()
            .account_access_snapshot()
            .map(|snapshot| snapshot.revision()),
        applied_revision: (stamp.account_id == account_id && stamp.revision > 0)
            .then_some(stamp.revision),
        devices: runtime.handle().account_access_sync_status(),
        account_id: account_id.into(),
    })
}
#[derive(Default, Serialize, Deserialize)]
struct Adoption {
    previous_account: String,
    next_account: String,
    device: String,
    #[serde(default)]
    policy: Option<mesh_talk_core::node::DeviceSyncPolicy>,
}

fn path(root: &Path, owner: &str, filename: &str) -> Result<PathBuf, CommandError> {
    crate::config_store::account_path(root, owner, filename)
        .ok_or_else(|| CommandError::Authorization("Invalid local storage owner".into()))
}

/// Explicit source action only. Joining startup must never publish its former credentials.
pub(crate) fn initialize_source(
    app: &AppState,
    admission: &RuntimeAdmission,
    runtime: &NodeRuntime,
) -> Result<(), CommandError> {
    runtime
        .handle()
        .authorize_legacy_channel_migration_if(|commit| admission.authorize_accept(runtime, commit))
        .map_err(CommandError::from)?;
    let policy = runtime.handle().device_sync_policy();
    let sync_all = policy.history
        && policy.attachments
        && policy.profile
        && policy.appearance
        && policy.contacts;
    let mut value = if let Some(snapshot) = runtime.handle().account_access_snapshot() {
        let value = AccountLogin::decode(&snapshot)?;
        if value.sync_all == sync_all {
            return Ok(());
        }
        // A device preparing a code may still have stale local credentials.
        // Only its explicitly selected sync intent can update a proven record.
        value
    } else {
        let info = app
            .session()
            .matching(admission.lease(), |info| info.clone())
            .map_err(CommandError::Authorization)?;
        AccountLogin {
            version: 1,
            login_name: info.user.login_name().into(),
            password: info.account_login_password.clone(),
            sync_all,
        }
    };
    value.sync_all = sync_all;
    let bytes = Zeroizing::new(
        serde_json::to_vec(&value)
            .map_err(|_| CommandError::Internal("Unable to prepare account login".into()))?,
    );
    runtime
        .handle()
        .publish_account_access_if(bytes.to_vec(), |commit| {
            admission.authorize_accept(runtime, commit)
        })
        .map_err(CommandError::from)?;
    Ok(())
}

/// Runs before target key adoption. Only metadata is written; no secret leaves encrypted stores.
pub(crate) fn preflight(
    root: &Path,
    app: &AppState,
    admission: &RuntimeAdmission,
    runtime: &NodeRuntime,
    snapshot: &AccountAccessSnapshot,
) -> Result<(), NodeError> {
    let value = AccountLogin::decode(snapshot)
        .map_err(|_| NodeError::InvalidInput("Invalid account login record".into()))?;
    let info = app
        .session()
        .matching(admission.lease(), |info| info.clone())
        .map_err(NodeError::Authorization)?;
    app.auth_service().identity_manager().validate_account_login_name(&info.user.name, &value.login_name)
        .map_err(|_| NodeError::InvalidInput("The source login name already belongs to another local user; no account was replaced".into()))?;
    let policy = runtime.handle().device_sync_policy();
    admission
        .checked(runtime, || {
            crate::config_store::write_account(
                &path(
                    root,
                    admission.lease().owner(),
                    "account-login-adoption.json",
                )?,
                &Adoption {
                    previous_account: runtime.account_id().into(),
                    next_account: snapshot.account_id(),
                    device: runtime.user_id().into(),
                    policy: Some(policy),
                },
            )
        })
        .map_err(|error| NodeError::Authorization(error.to_string()))
}

fn install(
    root: &Path,
    app: &AppState,
    admission: &RuntimeAdmission,
    runtime: &NodeRuntime,
    snapshot: &AccountAccessSnapshot,
) -> Result<Option<(crate::services::user::User, String)>, CommandError> {
    install_with_policy(
        root,
        app,
        admission,
        runtime,
        snapshot,
        |category, enabled| {
            admission.checked(runtime, || {
                runtime
                    .handle()
                    .set_device_sync_category(category, enabled)
                    .map_err(CommandError::from)
            })
        },
    )
}

fn install_with_policy(
    root: &Path,
    app: &AppState,
    admission: &RuntimeAdmission,
    runtime: &NodeRuntime,
    snapshot: &AccountAccessSnapshot,
    mut set_category: impl FnMut(SyncCategory, bool) -> Result<(), CommandError>,
) -> Result<Option<(crate::services::user::User, String)>, CommandError> {
    if snapshot.account_id() != runtime.account_id() {
        return Err(CommandError::Authorization(
            "Account login belongs to another account".into(),
        ));
    }
    let value = AccountLogin::decode(snapshot)?;
    let stamp_path = path(
        root,
        admission.lease().owner(),
        "account-login-applied.json",
    )?;
    let info = app
        .session()
        .matching(admission.lease(), |info| info.clone())
        .map_err(CommandError::Authorization)?;
    let applied: Applied = crate::config_store::read_account(&stamp_path);
    if applied.account_id == runtime.account_id()
        && applied.revision == snapshot.revision()
        && info.user.login_name() == value.login_name
        && info.account_login_password == value.password
    {
        return Ok(None);
    }
    let manager = app.auth_service().identity_manager();
    let authorize = |commit: &mut dyn FnMut() -> Result<(), IdentityError>| {
        admission
            .checked(runtime, || Ok(commit()))
            .map_err(|_| IdentityError::StorageError("Account login session replaced".into()))?
    };
    let adoption_path = path(
        root,
        admission.lease().owner(),
        "account-login-adoption.json",
    )?;
    let adoption: Adoption = crate::config_store::read_account(&adoption_path);
    // Default installation refuses an envelope bound to a different account.
    // Only an explicit, device-bound adoption permits that particular transition.
    let outcome = manager.install_account_login_if(
        &info.user.name,
        &info.password,
        &info.node_storage_password,
        runtime.account_id(),
        &value.login_name,
        &value.password,
        authorize,
    );
    if outcome.is_err()
        && adoption.next_account == runtime.account_id()
        && adoption.device == runtime.user_id()
        && adoption.previous_account != adoption.next_account
    {
        manager
            .install_account_login_for_adoption_if(
                &info.user.name,
                &info.password,
                &info.node_storage_password,
                runtime.account_id(),
                &value.login_name,
                &value.password,
                &adoption.previous_account,
                authorize,
            )
            .map_err(|_| {
                CommandError::Service(
                    "Unable to apply shared account login; existing local data was preserved"
                        .into(),
                )
            })?;
    } else {
        outcome.map_err(|_| {
            CommandError::Service(
                "Unable to apply shared account login; existing local data was preserved".into(),
            )
        })?;
    }
    let user = admission.update_session(runtime, |current| {
        // The namespace lock has been released before reentering session authority.
        current.user.account_login_name = Some(value.login_name.clone());
        current.account_login_password = value.password.clone();
        Ok(current.user.clone())
    })?;
    // The stamp commits only after every selected policy write succeeds. A
    // partial failure retries this revision; an applied revision never enforces
    // policy again, so later manual opt-outs remain local choices.
    let selected_policy = (adoption.next_account == runtime.account_id()
        && adoption.device == runtime.user_id())
    .then_some(adoption.policy)
    .flatten();
    if selected_policy.is_some() || value.sync_all {
        for category in [
            SyncCategory::History,
            SyncCategory::Attachments,
            SyncCategory::Profile,
            SyncCategory::Appearance,
            SyncCategory::Contacts,
        ] {
            set_category(
                category,
                selected_policy.is_none_or(|policy| policy.enabled(category)),
            )?;
        }
    }
    admission.checked(runtime, || {
        crate::config_store::write_account(
            &stamp_path,
            &Applied {
                account_id: runtime.account_id().into(),
                revision: snapshot.revision(),
            },
        )?;
        if adoption.next_account == runtime.account_id() && adoption.device == runtime.user_id() {
            let _ = std::fs::remove_file(&adoption_path);
        }
        Ok(())
    })?;
    Ok(Some((user, info.user.login_name().into())))
}

/// Retains auth serialization and runtime lifecycle through blocking KDF and atomic publication.
pub(crate) async fn apply_current<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    root: PathBuf,
    app: AppState,
    node: NodeState,
    admission: RuntimeAdmission,
) -> Result<(), CommandError> {
    apply_cancellable(handle, root, app, node, admission, None).await
}

async fn apply_cancellable<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    root: PathBuf,
    app: AppState,
    node: NodeState,
    admission: RuntimeAdmission,
    stopping: Option<&mut tokio::sync::oneshot::Receiver<()>>,
) -> Result<(), CommandError> {
    let (auth, guard) = {
        let acquire = async {
            let auth = app.auth_operation.clone().lock_owned().await;
            let guard = node.0.clone().read_owned().await;
            (auth, guard)
        };
        if let Some(stopping) = stopping {
            tokio::select! { _ = stopping => return Ok(()), guards = acquire => guards }
        } else {
            acquire.await
        }
    };
    tokio::task::spawn_blocking(move || {
        let _auth = auth;
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        admission.checked(runtime, || Ok(()))?;
        let Some(snapshot) = runtime.handle().account_access_snapshot() else { return Ok(()); };
        if let Some((user, previous_login)) = install(&root, &app, &admission, runtime, &snapshot)? {
            let password = app.session().matching(admission.lease(), |info| info.account_login_password.clone())
                .map_err(CommandError::Authorization)?;
            let saved_session = handle.try_state::<crate::settings::SettingsState>().map(|settings| {
                let status = crate::commands::persist_saved_login(&handle, &settings, &user, &password);
                if previous_login != user.login_name() { crate::session_store::clear(&previous_login); }
                status
            });
            admission.checked(runtime, || {
                let _ = handle.emit("account-profile-changed", serde_json::json!({
                    "user": crate::commands::UserInfo::from(user), "account_id": runtime.account_id(),
                    "device_id": runtime.user_id(), "saved_session": saved_session }));
                Ok(())
            })?;
        }
        Ok(())
    }).await.map_err(|_| CommandError::Internal("Account login application interrupted".into()))?
}

pub(crate) async fn reconcile<R: tauri::Runtime>(
    handle: tauri::AppHandle<R>,
    root: PathBuf,
    authority: RuntimeAuthority,
    app: AppState,
    node: NodeState,
    mut stopping: tokio::sync::oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tokio::select! { _ = &mut stopping => return, _ = interval.tick() => {} }
        if authority.current(|_| ()).is_err() {
            return;
        }
        let Ok(admission) = RuntimeAdmission::capture_lease(&app, &node, &authority.lease) else {
            continue;
        };
        // Do not cancel a blocking credential commit midway through runtime retirement.
        if let Err(error) = apply_cancellable(
            handle.clone(),
            root.clone(),
            app.clone(),
            node.clone(),
            admission,
            Some(&mut stopping),
        )
        .await
        {
            log::warn!("Shared account login reconciliation failed: {error}");
        }
    }
}

#[cfg(test)]
mod tests;

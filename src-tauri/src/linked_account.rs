//! Host session transition after device linking persists a new account secret.

use crate::chat_commands::NodeState;
use crate::commands::CommandError;
use crate::runtime_bootstrap::spawn_node_runtime;
use crate::state::AppState;

/// Restart the installed runtime under the current session so it reloads the
/// linked account keystore and re-advertises. Startup remains serialized by
/// `RuntimeAuthority`; a replaced session cannot use the previous password.
pub(crate) async fn adopt_linked_account<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: &AppState,
    node_state: &NodeState,
) -> Result<(), CommandError> {
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    let password = {
        let guard = node_state.0.read().await;
        app_state
            .session()
            .matching(&lease, |_| {
                node_state.check_installation(&lease)?;
                guard
                    .as_ref()
                    .filter(|runtime| runtime.host_account_id() == Some(lease.owner()))
                    .map(|runtime| runtime.restart_password().to_string())
                    .ok_or_else(CommandError::not_started)
            })
            .map_err(CommandError::Authentication)??
    };
    // Startup reloads the adopted key asynchronously. Do not announce success
    // before this exact session has applied the source account's login record.
    let root = crate::data_dir();
    let intent_path =
        crate::config_store::account_path(&root, lease.owner(), "account-login-adoption.json")
            .ok_or_else(|| CommandError::Authorization("Invalid local owner".into()))?;
    let expected = std::fs::read(&intent_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| {
            value
                .get("next_account")
                .and_then(|id| id.as_str())
                .map(str::to_owned)
        });
    spawn_node_runtime(
        app_handle.clone(),
        lease.clone(),
        password,
        node_state.clone(),
        app_state.session().clone(),
    );
    if let Some(expected) = expected {
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            loop {
                app_state
                    .session()
                    .matching(&lease, |_| ())
                    .map_err(CommandError::Authorization)?;
                let ready = {
                    let guard = node_state.0.read().await;
                    guard.as_ref().is_some_and(|runtime| {
                        runtime.account_id() == expected
                            && runtime.handle().account_access_snapshot().is_some()
                    })
                };
                if ready {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            let admission = crate::runtime_admission::RuntimeAdmission::capture_lease(
                app_state, node_state, &lease,
            )?;
            crate::shared_account_login::apply_current(
                app_handle,
                root,
                app_state.clone(),
                node_state.clone(),
                admission,
            )
            .await
        })
        .await
        .map_err(|_| {
            CommandError::Service(
                "Account adopted; applying its shared login is still pending. Keep this app open"
                    .into(),
            )
        })??;
    }
    Ok(())
}

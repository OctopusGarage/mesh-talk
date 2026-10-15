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
        let guard = node_state.0.lock().await;
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
    spawn_node_runtime(
        app_handle,
        lease,
        password,
        node_state.clone(),
        app_state.session().clone(),
    );
    Ok(())
}

//! Owner-scoped invisible-mode settings. Network enforcement lives in the core.
use crate::{chat_commands::NodeState, state::AppState};
use mesh_talk_core::node::PrivacySnapshot;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct PrivacyResponse {
    owner: String,
    #[serde(flatten)]
    policy: PrivacySnapshot,
}
enum Change {
    Read,
    Mode(bool),
    Allow(String, bool),
    Initiate(String),
}
async fn dispatch(
    app: &AppState,
    node: &NodeState,
    owner: String,
    change: Change,
) -> Result<PrivacyResponse, String> {
    let session = app.session().clone();
    let generation = session.with_owner(|current| {
        if current != Some(owner.as_str()) {
            return Err("Unauthorized privacy owner".into());
        }
        Ok::<_, String>(session.generation())
    })?;
    // Never retain the synchronous session guard across an async network gate.
    // The generation check is repeated by the core immediately before commit.
    // That check authorizes the synchronous commit region: logout rejects queued
    // operations, but need not cancel a commit already started for the old owner.
    // Keep the runtime lifecycle guard until completion so it can never target
    // a replacement session's runtime or account directory.
    let guard = node.0.lock().await;
    let runtime = guard.as_ref().ok_or("Node not started")?;
    if runtime.host_account_id() != Some(owner.as_str()) {
        return Err("Privacy runtime owner mismatch".into());
    }
    let authorize = || {
        if session.generation() == generation {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Session changed",
            ))
        }
    };
    let result = match change {
        Change::Read => authorize(),
        Change::Mode(value) => runtime.set_invisible_if(value, authorize).await,
        Change::Allow(account, allowed) => {
            runtime.set_allowed_if(&account, allowed, authorize).await
        }
        Change::Initiate(account) => runtime.initiate_contact_if(&account, authorize).await,
    };
    result.map_err(|_| "Could not update network privacy".to_string())?;
    Ok(PrivacyResponse {
        owner,
        policy: runtime.privacy_snapshot(),
    })
}

#[tauri::command]
pub async fn get_privacy(
    owner: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<PrivacyResponse, String> {
    dispatch(&app, &node, owner, Change::Read).await
}
#[tauri::command]
pub async fn set_invisible(
    owner: String,
    invisible: bool,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<PrivacyResponse, String> {
    dispatch(&app, &node, owner, Change::Mode(invisible)).await
}
#[tauri::command]
pub async fn set_privacy_allowed(
    owner: String,
    account: String,
    allowed: bool,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<PrivacyResponse, String> {
    dispatch(&app, &node, owner, Change::Allow(account, allowed)).await
}
#[tauri::command]
pub async fn initiate_privacy_contact(
    owner: String,
    account: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<PrivacyResponse, String> {
    dispatch(&app, &node, owner, Change::Initiate(account)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    #[test]
    fn registered_privacy_commands_authorize_and_persist_real_runtime() {
        let root = tempfile::tempdir().unwrap();
        let app_state = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_owned()),
        ))));
        let node = NodeState::empty();
        let app = tauri::test::mock_builder()
            .manage(app_state.clone())
            .manage(node.clone())
            .invoke_handler(tauri::generate_handler![
                get_privacy,
                set_invisible,
                set_privacy_allowed,
                initiate_privacy_contact
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let invoke = |command: &str, body| {
            tauri::test::get_ipc_response(
                &webview,
                tauri::webview::InvokeRequest {
                    cmd: command.into(),
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
            .map(|body| body.deserialize::<PrivacyResponse>().unwrap())
        };
        assert!(invoke("get_privacy", serde_json::json!({"owner":"alice"})).is_err());
        app_state.session().set(
            "test-token".into(),
            User {
                user_id: "alice".into(),
                name: "alice".into(),
                display_name: "Alice".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "alice",
            "Alice",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        *node.0.blocking_lock() = Some(runtime);
        let read = invoke("get_privacy", serde_json::json!({"owner":"alice"})).unwrap();
        assert_eq!(read.owner, "alice");
        assert!(!read.policy.invisible);
        let changed = invoke(
            "set_invisible",
            serde_json::json!({"owner":"alice", "invisible":true}),
        )
        .unwrap();
        assert!(changed.policy.invisible);
        assert!(invoke(
            "set_invisible",
            serde_json::json!({"owner":"other", "invisible":false})
        )
        .is_err());
        assert!(invoke(
            "set_privacy_allowed",
            serde_json::json!({"owner":"alice", "account":"invalid", "allowed":true})
        )
        .is_err());
        assert!(invoke(
            "set_privacy_allowed",
            serde_json::json!({"owner":"alice", "account":"b".repeat(32), "allowed":true})
        )
        .is_err());
        assert!(invoke(
            "initiate_privacy_contact",
            serde_json::json!({"owner":"alice", "account":"b".repeat(32)})
        )
        .is_err());
        assert!(invoke("get_privacy", serde_json::json!({"ownerId":"alice"})).is_err());
        // Poll until the operation waits for the runtime lifecycle lock, then
        // log out. A previously authorized queued write must not become durable.
        tauri::async_runtime::block_on(async {
            let guard = node.0.lock().await;
            let mut queued = std::pin::pin!(dispatch(
                &app_state,
                &node,
                "alice".into(),
                Change::Mode(false)
            ));
            std::future::poll_fn(|cx| {
                assert!(std::future::Future::poll(queued.as_mut(), cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            app_state.session().clear();
            drop(guard);
            assert!(queued.await.is_err());
        });
        assert!(invoke(
            "set_invisible",
            serde_json::json!({"owner":"alice", "invisible":false})
        )
        .is_err());
        assert!(
            node.0
                .blocking_lock()
                .as_ref()
                .unwrap()
                .privacy_snapshot()
                .invisible
        );
        node.0.blocking_lock().take();
        let saved = mesh_talk_core::node::PrivacyPolicy::open(
            &root.path().join("accounts/alice/privacy.policy"),
            "pw",
        )
        .unwrap();
        assert!(saved.snapshot().invisible);
    }
}

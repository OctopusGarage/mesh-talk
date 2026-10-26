//! Owner-scoped invisible-mode settings. Network enforcement lives in the core.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::node::{AccountBindingChange, PrivacySnapshot};
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
    let admission = RuntimeAdmission::capture(app, node, &owner).map_err(|error| match &error {
        CommandError::Authorization(message)
            if message == "not logged in" || message == "session owner mismatch" =>
        {
            "Unauthorized privacy owner".into()
        }
        _ => privacy_error(error),
    })?;
    if matches!(change, Change::Read) {
        return admission
            .with_runtime(|runtime| {
                Ok(PrivacyResponse {
                    owner,
                    policy: runtime.privacy_snapshot(),
                })
            })
            .await
            .map_err(privacy_error);
    }
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                // Check before waiting; the guarded commit checks again after the gate.
                admission.checked(runtime, || Ok(()))?;
                let node = runtime.handle();
                let authorize = |commit: &mut dyn FnMut() -> std::io::Result<()>| {
                    admission.authorize_io(runtime, commit)
                };
                let result = match change {
                    Change::Read => unreachable!(),
                    Change::Mode(value) => node.set_invisible_guarded(value, authorize).await,
                    Change::Allow(account, allowed) => {
                        node.set_allowed_guarded(&account, allowed, authorize).await
                    }
                    Change::Initiate(account) => {
                        node.initiate_contact_guarded(&account, authorize).await
                    }
                };
                result.map_err(|error| CommandError::Io(error.to_string()))?;
                admission.checked(runtime, || {
                    Ok(PrivacyResponse {
                        owner,
                        policy: runtime.privacy_snapshot(),
                    })
                })
            })
        })
        .await
        .map_err(privacy_error)
}

fn privacy_error(error: CommandError) -> String {
    match error {
        CommandError::NotStarted(_) => "Node not started".into(),
        CommandError::Authorization(message) if message == "runtime owner mismatch" => {
            "Privacy runtime owner mismatch".into()
        }
        _ => "Could not update network privacy".into(),
    }
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

#[derive(Serialize)]
pub struct AccountBindingChangesResponse {
    owner: String,
    changes: Vec<AccountBindingChange>,
}

#[tauri::command]
pub async fn get_account_binding_changes(
    owner: String,
    account: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<AccountBindingChangesResponse, CommandError> {
    RuntimeAdmission::capture(&app, &node, &owner)?
        .with_runtime(|runtime| {
            Ok(AccountBindingChangesResponse {
                owner,
                changes: runtime.handle().account_binding_changes(&account),
            })
        })
        .await
}

#[tauri::command]
pub async fn accept_account_binding_change(
    owner: String,
    change: AccountBindingChange,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<PrivacyResponse, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    admission
        .detached(move |runtime, admission| {
            Box::pin(async move {
                runtime
                    .handle()
                    .accept_account_binding_change_guarded(&change, |commit| {
                        admission.authorize_io(runtime, commit)
                    })
                    .await
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::InvalidInput {
                            CommandError::Validation(error.to_string())
                        } else {
                            CommandError::Io(error.to_string())
                        }
                    })?;
                admission.checked(runtime, || {
                    Ok(PrivacyResponse {
                        owner,
                        policy: runtime.privacy_snapshot(),
                    })
                })
            })
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_privacy_ipc_retains_lifecycle_until_rejected_commit_finishes() {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_owned()),
        ))));
        app.session().set(
            "token".into(),
            User {
                user_id: "alice".into(),
                account_login_name: None,
                name: "alice".into(),
                display_name: "Alice".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let state = NodeState::empty();
        let runtime = mesh_talk_core::node::NodeRuntime::start(
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
        )
        .await
        .unwrap();
        let node = runtime.handle();
        *state.0.write().await = Some(runtime);
        let documents = || {
            ["privacy.policy", "peer-proofs", "peer-routes"]
                .map(|name| std::fs::read(root.path().join("accounts/alice").join(name)).unwrap())
        };
        let before = documents();
        let (entered, held) = std::sync::mpsc::channel();
        let (release, released) = std::sync::mpsc::channel();
        let holder_node = node.clone();
        let holder = std::thread::spawn(move || {
            tauri::async_runtime::block_on(holder_node.set_allowed_if(
                &"a".repeat(32),
                false,
                move || {
                    entered.send(()).unwrap();
                    released.recv().unwrap();
                    Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "test holder",
                    ))
                },
            ))
        });
        held.recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (producer_entered, producer_started) = std::sync::mpsc::channel();
        *app.owner_command_captured.lock().unwrap() = Some(producer_entered);
        let caller_app = app.clone();
        let caller_state = state.clone();
        let caller = tokio::spawn(async move {
            dispatch(
                &caller_app,
                &caller_state,
                "alice".into(),
                Change::Mode(true),
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                match producer_started.try_recv() {
                    Ok("owner-producer") => break,
                    Ok(_) => continue,
                    Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                    Err(error) => panic!("detached producer did not start: {error}"),
                }
            }
        })
        .await
        .expect("detached producer owns the runtime lifecycle before IPC cancellation");
        *app.owner_command_captured.lock().unwrap() = None;
        caller.abort();
        assert!(matches!(caller.await, Err(error) if error.is_cancelled()));
        assert!(
            state.0.try_write().is_err(),
            "cancelled IPC released live runtime producer"
        );
        app.session().clear();
        release.send(()).unwrap();
        assert!(holder.join().unwrap().is_err());
        let runtime = state.0.write().await.take().unwrap();
        assert!(!runtime.privacy_snapshot().invisible);
        assert_eq!(documents(), before);
        runtime.stop().await;
    }

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
                account_login_name: None,
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
        *node.0.blocking_write() = Some(runtime);
        let read = invoke("get_privacy", serde_json::json!({"owner":"alice"})).unwrap();
        assert_eq!(read.owner, "alice");
        assert!(!read.policy.invisible);
        let changed = invoke(
            "set_invisible",
            serde_json::json!({"owner":"alice", "invisible":true}),
        )
        .unwrap();
        assert!(changed.policy.invisible);
        assert!(
            invoke(
                "set_invisible",
                serde_json::json!({"owner":"other", "invisible":false})
            )
            .is_err()
        );
        assert!(
            invoke(
                "set_privacy_allowed",
                serde_json::json!({"owner":"alice", "account":"invalid", "allowed":true})
            )
            .is_err()
        );
        assert!(
            invoke(
                "set_privacy_allowed",
                serde_json::json!({"owner":"alice", "account":"b".repeat(32), "allowed":true})
            )
            .is_err()
        );
        assert!(
            invoke(
                "initiate_privacy_contact",
                serde_json::json!({"owner":"alice", "account":"b".repeat(32)})
            )
            .is_err()
        );
        assert!(invoke("get_privacy", serde_json::json!({"ownerId":"alice"})).is_err());
        // Poll until the operation waits for the runtime lifecycle lock, then
        // log out. A previously authorized queued write must not become durable.
        tauri::async_runtime::block_on(async {
            let guard = node.0.write().await;
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
        assert!(
            invoke(
                "set_invisible",
                serde_json::json!({"owner":"alice", "invisible":false})
            )
            .is_err()
        );
        assert!(
            node.0
                .blocking_write()
                .as_ref()
                .unwrap()
                .privacy_snapshot()
                .invisible
        );
        node.0.blocking_write().take();
        let saved = mesh_talk_core::node::PrivacyPolicy::open(
            &root.path().join("accounts/alice/privacy.policy"),
            "pw",
        )
        .unwrap();
        assert!(saved.snapshot().invisible);
    }
}

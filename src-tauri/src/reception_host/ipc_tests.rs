//! Exercise registered IPC against a real bounded participant and host lease.
use super::*;
use crate::{
    chat_commands::RuntimeAuthority,
    services::{auth_service::AuthService, user::User},
};
use mesh_talk_core::{
    identity::manager::IdentityManager,
    node::{NodeRuntime, RuntimeConfig, RuntimeStartup},
    storage::file_manager::FileManager,
};
use serde_json::{Value, json};

struct Fixture {
    _root: tempfile::TempDir,
    app: AppState,
    node: NodeState,
    view: tauri::WebviewWindow<tauri::test::MockRuntime>,
    scope: Scope,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let app = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.path().to_owned()),
        ))));
        app.session().set(
            "session".into(),
            User {
                user_id: "owner".into(),
                account_login_name: None,
                name: "Owner".into(),
                display_name: "Owner".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "pw".into(),
        );
        let node = NodeState::empty();
        let host = ReceptionHostState::default();
        let authority = RuntimeAuthority::request(
            app.session().clone(),
            app.session().capture().unwrap(),
            node.clone(),
        )
        .unwrap();
        let scope = tauri::async_runtime::block_on(async {
            let permit = authority.begin().await.unwrap();
            let runtime = NodeRuntime::start_owned(
                RuntimeStartup::bounded(RuntimeConfig {
                    base_dir: root.path(),
                    account_id: "owner",
                    display_name: "Owner",
                    password: "pw",
                    discovery_port: 0,
                    post_office_enabled: false,
                    candidate_source: None,
                }),
                |identity| {
                    authority.observe_identity(identity).unwrap();
                    Ok(())
                },
                |launch| Ok(launch("Owner")),
            )
            .await
            .unwrap();
            assert!(runtime.handle().bounded_delivery());
            let scope = Scope {
                owner: "owner".into(),
                account_id: runtime.account_id().into(),
                device_id: runtime.user_id().into(),
                runtime_id: authority.runtime_id().unwrap(),
            };
            assert!(permit.install(runtime).await);
            scope
        });
        host.install(scope.clone());
        let mock = tauri::test::mock_builder()
            .manage(app.clone())
            .manage(app.app_lock().clone())
            .manage(node.clone())
            .manage(host)
            .invoke_handler(tauri::generate_handler![
                owner_reception_page,
                owner_reception_ack,
                crate::app_lock::lock_app
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = tauri::WebviewWindowBuilder::new(&mock, "main", Default::default())
            .build()
            .unwrap();
        Self {
            _root: root,
            app,
            node,
            view,
            scope,
        }
    }
    fn invoke(&self, command: &str, body: Value) -> Result<Value, Value> {
        tauri::test::get_ipc_response(
            &self.view,
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
        .map(|body| body.deserialize().unwrap())
    }
    fn pull(&self, after: Value, reset: bool) -> Result<Value, Value> {
        self.invoke(
            "owner_reception_page",
            json!({"scope":self.scope,"after":after,"through":null,"reset":reset}),
        )
    }
    fn ack(&self, page: &Value) -> Result<Value, Value> {
        self.invoke("owner_reception_ack", json!({"scope":self.scope,"token":page["token"],"after":page["after"],"next":page["next"]}))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        tauri::async_runtime::block_on(async {
            let runtime = self.node.0.write().await.take();
            if let Some(runtime) = runtime {
                runtime.stop().await;
            }
        });
    }
}

#[test]
fn registered_pull_and_ack_keep_one_page_and_fence_replaced_owner_and_app_lock() {
    let fixture = Fixture::new();
    let first = fixture.pull(Value::Null, true).unwrap();
    assert_eq!(first["scope"], json!(fixture.scope));
    assert!(fixture.pull(first["next"].clone(), false).is_err());
    let mut forged = first.clone();
    forged["token"] = json!("unissued");
    assert!(fixture.ack(&forged).is_err());
    fixture.ack(&first).unwrap();
    // A delayed duplicate of the previous ACK cannot clear this later page.
    let second = fixture.pull(first["next"].clone(), false).unwrap();
    fixture.ack(&first).unwrap();
    assert!(fixture.pull(second["next"].clone(), false).is_err());
    fixture.ack(&second).unwrap();
    fixture.invoke("lock_app", json!({})).unwrap();
    assert!(fixture.pull(second["next"].clone(), false).is_err());
    assert!(fixture.ack(&second).is_err());
    fixture.app.session().clear();
    assert!(fixture.pull(second["next"].clone(), true).is_err());
}

#[test]
fn renderer_reload_replays_current_profiles_and_replaces_only_its_scoped_page() {
    let fixture = Fixture::new();
    tauri::async_runtime::block_on(async {
        fixture
            .node
            .0
            .read()
            .await
            .as_ref()
            .unwrap()
            .handle()
            .set_avatar(Some(b"data:image/png;base64,fixture".to_vec()))
            .await
            .unwrap();
    });
    let first = fixture.pull(Value::Null, true).unwrap();
    assert!(
        first["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| receipt["kind"] == "profile")
    );
    let resumed = fixture.pull(first["next"].clone(), true).unwrap();
    assert_ne!(first["token"], resumed["token"]);
    assert!(fixture.ack(&first).is_err());
    fixture.ack(&resumed).unwrap();
    let unchanged = fixture.pull(resumed["next"].clone(), false).unwrap();
    assert!(
        !unchanged["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| receipt["kind"] == "profile")
    );
    fixture.ack(&unchanged).unwrap();
    let reloaded = fixture.pull(unchanged["next"].clone(), true).unwrap();
    assert!(
        reloaded["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|receipt| receipt["kind"] == "profile")
    );
    let stale_scope = Scope {
        runtime_id: "retired".into(),
        ..fixture.scope.clone()
    };
    assert!(
        fixture
            .invoke(
                "owner_reception_page",
                json!({"scope":stale_scope,"after":reloaded["next"],"through":null,"reset":true})
            )
            .is_err()
    );
    fixture.ack(&reloaded).unwrap();
}

use super::*;
use crate::services::auth_service::AuthService;
use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
use std::{sync::Arc, time::Duration};

struct Fixture {
    root: tempfile::TempDir,
    app: AppState,
    node: NodeState,
    admission: RuntimeAdmission,
}

impl Fixture {
    async fn new(name: &str, raw_password: &str) -> Self {
        let root = tempfile::tempdir().unwrap();
        let auth = AuthService::new(Arc::new(IdentityManager::new(FileManager::new(
            root.path().to_owned(),
        ))));
        auth.register(name.into(), raw_password.into(), "fixture".into())
            .unwrap();
        auth.login(name.into(), raw_password.into()).unwrap();
        let app = AppState::new(auth);
        let node = NodeState::empty();
        let lease = app.session().capture().unwrap();
        let authority =
            RuntimeAuthority::request(app.session().clone(), lease.clone(), node.clone()).unwrap();
        let permit = authority.begin().await.unwrap();
        let runtime = start(root.path(), lease.owner(), name, raw_password).await;
        assert!(permit.install(runtime).await);
        let admission = RuntimeAdmission::capture_lease(&app, &node, &lease).unwrap();
        Self {
            root,
            app,
            node,
            admission,
        }
    }

    async fn stop(&self) {
        let runtime = self.node.0.write().await.take().unwrap();
        runtime.stop().await;
    }
}

async fn start(root: &Path, owner: &str, name: &str, password: &str) -> NodeRuntime {
    let reserve = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let port = reserve.local_addr().unwrap().port();
    drop(reserve);
    NodeRuntime::start(
        root,
        owner,
        name,
        password,
        port,
        |_| {},
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    )
    .await
    .unwrap()
}

async fn initialize(fixture: &Fixture) -> AccountAccessSnapshot {
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    initialize_source(&fixture.app, &fixture.admission, runtime).unwrap();
    let snapshot = runtime.handle().account_access_snapshot().unwrap();
    install(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    snapshot
}

async fn discover(source: &NodeRuntime, target: &NodeRuntime) {
    target
        .probe_endpoint(([127, 0, 0, 1], source.discovery_port()).into())
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if target
                .peers()
                .iter()
                .any(|peer| peer.public.user_id() == source.user_id())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("signed loopback discovery must expose source account");
}

#[tokio::test]
async fn source_install_preserves_session_authority_and_never_exposes_secrets() {
    let fixture = Fixture::new("Federer", " source-password ").await;
    let before = fixture.app.session().get().unwrap();
    let snapshot = initialize(&fixture).await;
    let after = fixture.app.session().get().unwrap();
    assert_eq!(before.token, after.token);
    assert_eq!(before.user.user_id, after.user.user_id);
    assert!(
        fixture
            .app
            .session()
            .matching(fixture.admission.lease(), |_| ())
            .is_ok()
    );
    assert_eq!(after.user.login_name(), "Federer");
    assert_eq!(after.password, "source-password");
    assert_eq!(after.node_storage_password, " source-password ");
    let public =
        serde_json::to_string(&crate::commands::UserInfo::from(after.user.clone())).unwrap();
    for output in [public, format!("{after:?}"), format!("{snapshot:?}")] {
        assert!(!output.contains("source-password"));
    }
    assert!(
        fixture
            .app
            .auth_service()
            .verify_credentials("Federer", " source-password ")
            .is_ok()
    );
    assert!(
        fixture
            .app
            .auth_service()
            .verify_credentials("Federer", "source-password")
            .is_err()
    );
    let guard = fixture.node.0.read().await;
    assert!(
        install(
            fixture.root.path(),
            &fixture.app,
            &fixture.admission,
            guard.as_ref().unwrap(),
            &snapshot
        )
        .unwrap()
        .is_none()
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn v4_pairing_adopts_login_preserving_storage_device_and_encrypted_history() {
    let source = Fixture::new("Federer", " source-password ").await;
    let target = Fixture::new("Messi10", " target-password ").await;
    {
        let guard = source.node.0.read().await;
        let runtime = guard.as_ref().unwrap();
        for category in all_categories() {
            runtime
                .handle()
                .set_device_sync_category(category, true)
                .unwrap();
        }
    }
    initialize(&source).await;
    let previous_snapshot = initialize(&target).await;
    let before = target.app.session().get().unwrap();
    let source_guard = source.node.0.read().await;
    let source_runtime = source_guard.as_ref().unwrap();
    let target_guard = target.node.0.read().await;
    let target_runtime = target_guard.as_ref().unwrap();
    let device = target_runtime.user_id().to_owned();
    let old_account = target_runtime.account_id().to_owned();
    target_runtime
        .send_to_account(&old_account, b"retained local history", None)
        .await
        .unwrap();
    assert_eq!(
        target_runtime
            .account_history(&old_account, 10)
            .unwrap()
            .len(),
        1
    );
    let history_path = target
        .root
        .path()
        .join("accounts")
        .join(&before.user.user_id)
        .join("sent.log");
    let encrypted_history = std::fs::read(&history_path).unwrap();
    assert!(
        !encrypted_history
            .windows(b"retained local history".len())
            .any(|bytes| bytes == b"retained local history")
    );
    // The current desktop's full-sync confirmation explicitly selects these
    // categories before preflight; legacy already-linked devices are covered by
    // direct record application tests without this new adoption policy field.
    for category in all_categories() {
        target_runtime
            .handle()
            .set_device_sync_category(category, true)
            .unwrap();
    }
    discover(source_runtime, target_runtime).await;
    let code = source_runtime.start_linking();
    let account = target_runtime
        .link_device_with_account_access_checked_if(
            source_runtime.user_id(),
            &code,
            |snapshot| {
                preflight(
                    target.root.path(),
                    &target.app,
                    &target.admission,
                    target_runtime,
                    snapshot,
                )
            },
            |commit| target.admission.authorize_accept(target_runtime, commit),
        )
        .await
        .unwrap();
    assert_eq!(account, source_runtime.account_id());
    let adoption_path = path(
        target.root.path(),
        target.admission.lease().owner(),
        "account-login-adoption.json",
    )
    .unwrap();
    assert!(adoption_path.exists());
    // A final old-runtime reconciliation may run before the restart. Force a new
    // old-account revision so this exercises a real write, not the stamp no-op.
    let old_snapshot = target_runtime
        .handle()
        .publish_account_access_if(previous_snapshot.payload().to_vec(), |commit| commit())
        .unwrap();
    assert!(
        install(
            target.root.path(),
            &target.app,
            &target.admission,
            target_runtime,
            &old_snapshot
        )
        .unwrap()
        .is_some()
    );
    assert!(
        adoption_path.exists(),
        "old-account reconciliation must retain the pending adoption authority"
    );
    drop(target_guard);
    let runtime = target.node.0.write().await.take().unwrap();
    runtime.stop().await;
    let authority = RuntimeAuthority::request(
        target.app.session().clone(),
        target.admission.lease().clone(),
        target.node.clone(),
    )
    .unwrap();
    let permit = authority.begin().await.unwrap();
    let restarted = start(
        target.root.path(),
        &before.user.user_id,
        "Messi10",
        " target-password ",
    )
    .await;
    assert_eq!(restarted.user_id(), device);
    assert_eq!(restarted.account_id(), account);
    assert_eq!(
        std::fs::read(&history_path).unwrap(),
        encrypted_history,
        "account adoption must retain the encrypted history bytes"
    );
    assert!(permit.install(restarted).await);
    let admission =
        RuntimeAdmission::capture_lease(&target.app, &target.node, target.admission.lease())
            .unwrap();
    let guard = target.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    let snapshot = runtime
        .handle()
        .account_access_snapshot()
        .expect("V3 access survives restart");
    install(
        target.root.path(),
        &target.app,
        &admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    assert!(
        !adoption_path.exists(),
        "successful new-account application consumes the adoption authority"
    );
    let policy = runtime.handle().device_sync_policy();
    assert!(
        policy.history
            && policy.attachments
            && policy.profile
            && policy.appearance
            && policy.contacts
    );
    let after = target.app.session().get().unwrap();
    assert_eq!(after.user.user_id, before.user.user_id);
    assert_eq!(after.user.name, "Messi10");
    assert_eq!(after.token, before.token);
    assert_eq!(after.password, "target-password");
    assert_eq!(after.node_storage_password, " target-password ");
    assert_eq!(after.account_login_password, " source-password ");
    let auth = target.app.auth_service();
    assert_eq!(
        auth.verify_credentials("Federer", " source-password ")
            .unwrap(),
        before.user.user_id
    );
    assert!(
        auth.verify_credentials("Messi10", " target-password ")
            .is_err()
    );
    assert!(
        target
            .root
            .path()
            .join("users/Messi10/meta/user.json")
            .exists()
    );
    assert!(!target.root.path().join("users/Federer").exists());
    drop(guard);
    drop(source_guard);
    target.stop().await;
    source.stop().await;
}

#[tokio::test]
async fn collision_preflight_rejects_v4_before_account_keystore_adoption() {
    let source = Fixture::new("Federer", "source-password").await;
    let target = Fixture::new("Messi10", "target-password").await;
    initialize(&source).await;
    target
        .app
        .auth_service()
        .register(
            "Federer".into(),
            "unrelated-password".into(),
            "fixture".into(),
        )
        .unwrap();
    let sg = source.node.0.read().await;
    let source_runtime = sg.as_ref().unwrap();
    let tg = target.node.0.read().await;
    let runtime = tg.as_ref().unwrap();
    let old_account = runtime.account_id().to_owned();
    discover(source_runtime, runtime).await;
    let code = source_runtime.start_linking();
    assert!(
        runtime
            .link_device_with_account_access_checked_if(
                source_runtime.user_id(),
                &code,
                |snapshot| preflight(
                    target.root.path(),
                    &target.app,
                    &target.admission,
                    runtime,
                    snapshot
                ),
                |commit| target.admission.authorize_accept(runtime, commit),
            )
            .await
            .is_err()
    );
    let owner = target.admission.lease().owner();
    let account = mesh_talk_core::identity::account_keystore::load(
        &target
            .root
            .path()
            .join("accounts")
            .join(owner)
            .join("account.keystore"),
        "target-password",
    )
    .unwrap();
    assert_eq!(account.account_id(), old_account);
    assert!(
        !target
            .root
            .path()
            .join("users/Messi10/meta/account_login.bin")
            .exists()
    );
    assert!(
        target
            .app
            .auth_service()
            .verify_credentials("Messi10", "target-password")
            .is_ok()
    );
    assert!(
        target
            .app
            .auth_service()
            .verify_credentials("Federer", "unrelated-password")
            .is_ok()
    );
    drop(tg);
    drop(sg);
    target.stop().await;
    source.stop().await;
}

#[tokio::test]
async fn stale_same_owner_lease_cannot_publish_envelope_or_update_session() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    initialize_source(&fixture.app, &fixture.admission, runtime).unwrap();
    let snapshot = runtime.handle().account_access_snapshot().unwrap();
    fixture
        .app
        .auth_service()
        .login("Federer".into(), "source-password".into())
        .unwrap();
    let replacement = fixture.app.session().get().unwrap();
    assert!(
        install(
            fixture.root.path(),
            &fixture.app,
            &fixture.admission,
            runtime,
            &snapshot
        )
        .is_err()
    );
    assert!(
        !fixture
            .root
            .path()
            .join("users/Federer/meta/account_login.bin")
            .exists()
    );
    let after = fixture.app.session().get().unwrap();
    assert_eq!(after.token, replacement.token);
    assert_eq!(
        after.user.account_login_name,
        replacement.user.account_login_name
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn malformed_login_record_cannot_replace_current_credentials() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    for payload in [
        r#"{"version":1,"login_name":"../escape","password":"secret"}"#,
        r#"{"version":2,"login_name":"Federer","password":"secret"}"#,
        r#"{"version":1,"login_name":"Federer","password":" "}"#,
        r#"{"version":1,"login_name":"Federer","password":"secret","extra":true}"#,
    ] {
        runtime
            .handle()
            .publish_account_access_if(payload.as_bytes().to_vec(), |commit| commit())
            .unwrap();
        let snapshot = runtime.handle().account_access_snapshot().unwrap();
        assert!(AccountLogin::decode(&snapshot).is_err());
        assert!(
            install(
                fixture.root.path(),
                &fixture.app,
                &fixture.admission,
                runtime,
                &snapshot
            )
            .is_err()
        );
    }
    assert!(
        !fixture
            .root
            .path()
            .join("users/Federer/meta/account_login.bin")
            .exists()
    );
    assert!(
        fixture
            .app
            .auth_service()
            .verify_credentials("Federer", "source-password")
            .is_ok()
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn reconciliation_shutdown_cancels_waiting_for_auth_serialization() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let held_auth = fixture.app.auth_operation.clone().lock_owned().await;
    let (shutdown, mut stopping) = tokio::sync::oneshot::channel();
    let applying = apply_cancellable(
        app.handle().clone(),
        fixture.root.path().to_owned(),
        fixture.app.clone(),
        fixture.node.clone(),
        fixture.admission.clone(),
        Some(&mut stopping),
    );
    tokio::pin!(applying);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut applying)
            .await
            .is_err()
    );
    shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), &mut applying)
        .await
        .expect("retirement must not wait for the auth lock")
        .unwrap();
    assert!(fixture.app.auth_operation.try_lock().is_err());
    drop(held_auth);
    fixture.stop().await;
}

#[tokio::test]
async fn reconciliation_shutdown_releases_auth_when_waiting_for_runtime() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let app = tauri::test::mock_builder()
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let held_runtime = fixture.node.0.clone().write_owned().await;
    let (shutdown, mut stopping) = tokio::sync::oneshot::channel();
    let applying = apply_cancellable(
        app.handle().clone(),
        fixture.root.path().to_owned(),
        fixture.app.clone(),
        fixture.node.clone(),
        fixture.admission.clone(),
        Some(&mut stopping),
    );
    tokio::pin!(applying);
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut applying)
            .await
            .is_err()
    );
    shutdown.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(1), &mut applying)
        .await
        .expect("retirement must cancel runtime admission waiting")
        .unwrap();
    assert!(
        fixture.app.auth_operation.try_lock().is_ok(),
        "cancellation must release its acquired auth lock"
    );
    drop(held_runtime);
    fixture.stop().await;
}

fn all_categories() -> [mesh_talk_core::node::SyncCategory; 5] {
    use mesh_talk_core::node::SyncCategory::*;
    [History, Attachments, Profile, Appearance, Contacts]
}

#[tokio::test]
async fn explicit_full_sync_source_upgrades_intent_without_replacing_proven_credentials() {
    let fixture = Fixture::new("Messi10", "target-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    let original = runtime.handle().publish_account_access_if(
        serde_json::to_vec(&serde_json::json!({"version":1,"login_name":"Federer","password":"source-password"})).unwrap(),
        |commit| commit(),
    ).unwrap();
    for category in all_categories() {
        runtime
            .handle()
            .set_device_sync_category(category, true)
            .unwrap();
    }
    initialize_source(&fixture.app, &fixture.admission, runtime).unwrap();
    let upgraded = runtime.handle().account_access_snapshot().unwrap();
    let value: serde_json::Value = serde_json::from_slice(upgraded.payload()).unwrap();
    assert_eq!(value["sync_all"], true);
    assert_eq!(value["login_name"], "Federer");
    assert_eq!(value["password"], "source-password");
    assert!(upgraded.revision() > original.revision());
    initialize_source(&fixture.app, &fixture.admission, runtime).unwrap();
    assert_eq!(
        runtime
            .handle()
            .account_access_snapshot()
            .unwrap()
            .revision(),
        upgraded.revision()
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn legacy_record_without_full_sync_consent_preserves_device_opt_outs() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    let before = runtime.handle().device_sync_policy();
    let snapshot = runtime.handle().publish_account_access_if(
        serde_json::to_vec(&serde_json::json!({"version":1,"login_name":"Federer","password":"source-password"})).unwrap(),
        |commit| commit(),
    ).unwrap();
    install(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    assert_eq!(runtime.handle().device_sync_policy(), before);
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn applied_full_sync_record_does_not_override_later_manual_opt_outs() {
    let fixture = Fixture::new("Federer", "source-password").await;
    {
        let guard = fixture.node.0.read().await;
        for category in all_categories() {
            guard
                .as_ref()
                .unwrap()
                .handle()
                .set_device_sync_category(category, true)
                .unwrap();
        }
    }
    let snapshot = initialize(&fixture).await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    runtime
        .handle()
        .set_device_sync_category(mesh_talk_core::node::SyncCategory::Contacts, false)
        .unwrap();
    let before = fixture.app.session().get().unwrap();
    assert!(
        install(
            fixture.root.path(),
            &fixture.app,
            &fixture.admission,
            runtime,
            &snapshot
        )
        .unwrap()
        .is_none()
    );
    assert!(!runtime.handle().device_sync_policy().contacts);
    assert_eq!(fixture.app.session().get().unwrap().token, before.token);
    assert!(
        fixture
            .app
            .session()
            .matching(fixture.admission.lease(), |_| ())
            .is_ok()
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn partial_full_sync_policy_failure_has_no_applied_stamp_and_retries() {
    let fixture = Fixture::new("Federer", "source-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    let snapshot = runtime
        .handle()
        .publish_account_access_if(
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "login_name": "Federer", "password": "source-password", "sync_all": true,
            }))
            .unwrap(),
            |commit| commit(),
        )
        .unwrap();
    let before = fixture.app.session().get().unwrap();
    let mut calls = 0;
    let failed = install_with_policy(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
        |category, enabled| {
            calls += 1;
            if calls == 3 {
                return Err(CommandError::Io("injected policy write failure".into()));
            }
            fixture.admission.checked(runtime, || {
                runtime
                    .handle()
                    .set_device_sync_category(category, enabled)
                    .map_err(CommandError::from)
            })
        },
    );
    assert!(failed.is_err());
    assert!(runtime.handle().device_sync_policy().attachments);
    assert!(!runtime.handle().device_sync_policy().contacts);
    let stamp_path = path(
        fixture.root.path(),
        fixture.admission.lease().owner(),
        "account-login-applied.json",
    )
    .unwrap();
    assert!(
        !stamp_path.exists(),
        "partial policy writes cannot acknowledge the revision"
    );
    assert!(
        install(
            fixture.root.path(),
            &fixture.app,
            &fixture.admission,
            runtime,
            &snapshot,
        )
        .unwrap()
        .is_some()
    );
    let applied: Applied = crate::config_store::read_account(&stamp_path);
    assert_eq!(applied.revision, snapshot.revision());
    let policy = runtime.handle().device_sync_policy();
    assert!(
        policy.history
            && policy.attachments
            && policy.profile
            && policy.appearance
            && policy.contacts
    );
    assert_eq!(fixture.app.session().get().unwrap().token, before.token);
    assert!(
        fixture
            .app
            .session()
            .matching(fixture.admission.lease(), |_| ())
            .is_ok()
    );
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn login_sync_status_distinguishes_received_from_installed_without_credentials() {
    let fixture = Fixture::new("Federer", "private fixture login password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    let empty = sync_status(
        fixture.root.path(),
        fixture.admission.lease().owner(),
        runtime,
    )
    .unwrap();
    assert_eq!(empty.available_revision, None);
    assert_eq!(empty.applied_revision, None);
    initialize_source(&fixture.app, &fixture.admission, runtime).unwrap();
    let received = sync_status(
        fixture.root.path(),
        fixture.admission.lease().owner(),
        runtime,
    )
    .unwrap();
    assert!(received.available_revision.is_some());
    assert_eq!(
        received.applied_revision, None,
        "received credentials are not yet installed"
    );
    let snapshot = runtime.handle().account_access_snapshot().unwrap();
    install(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    let installed = sync_status(
        fixture.root.path(),
        fixture.admission.lease().owner(),
        runtime,
    )
    .unwrap();
    assert_eq!(installed.available_revision, installed.applied_revision);
    let public = serde_json::to_string(&installed).unwrap();
    assert!(!public.contains("private fixture login password"));
    assert!(!public.contains("Federer"));
    drop(guard);
    fixture.stop().await;
}

#[tokio::test]
async fn explicit_joiner_category_choices_override_source_full_sync_default() {
    let fixture = Fixture::new("Federer", "target-password").await;
    let guard = fixture.node.0.read().await;
    let runtime = guard.as_ref().unwrap();
    runtime
        .handle()
        .set_device_sync_category(SyncCategory::History, false)
        .unwrap();
    let chosen = runtime.handle().device_sync_policy();
    let snapshot = runtime.handle().publish_account_access_if(
        serde_json::to_vec(&serde_json::json!({"version":1,"login_name":"Federer","password":"source-password","sync_all":true})).unwrap(),
        |commit| commit(),
    ).unwrap();
    preflight(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    install(
        fixture.root.path(),
        &fixture.app,
        &fixture.admission,
        runtime,
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        runtime.handle().device_sync_policy(),
        chosen,
        "mandatory shared login must not override the joining user's explicit category consent"
    );
    drop(guard);
    fixture.stop().await;
}

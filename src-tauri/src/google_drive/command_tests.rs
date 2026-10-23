//! Exercise registered native commands with real encrypted storage and a loopback Drive API.
use super::*;
use mesh_talk_core::identity::manager::IdentityManager;
use mesh_talk_core::node::{Node, NodeRuntime};
use mesh_talk_core::recovery::key::RecoveryKey;
use serde_json::{Value, json};
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const CONNECTION: &str = "native-cloud-fixture-connection-0001";
const PASSWORD: &str = "fixture-password-123";

struct Fixture {
    root: tempfile::TempDir,
    app: tauri::App<tauri::test::MockRuntime>,
    view: tauri::WebviewWindow<tauri::test::MockRuntime>,
    state: AppState,
    node: NodeState,
    _discovery_reservation: tokio::net::UdpSocket,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = AppState::new(crate::services::auth_service::AuthService::new(Arc::new(
            IdentityManager::new(FileManager::new(root.path().to_path_buf())),
        )));
        let node = NodeState::empty();
        let reservation = tauri::async_runtime::block_on(async {
            mesh_talk_core::transport::net::discovery_socket(0).unwrap()
        });
        *node.4.lock().unwrap() = Some((
            root.path().to_path_buf(),
            reservation.local_addr().unwrap().port(),
        ));
        let app = tauri::test::mock_builder()
            .manage(state.clone())
            .manage(state.app_lock().clone())
            .manage(node.clone())
            .manage(DriveState::default())
            .manage(crate::settings::SettingsState::isolated(
                root.path().join("settings.json"),
            ))
            .invoke_handler(tauri::generate_handler![
                google_drive_available,
                google_drive_connect,
                google_drive_disconnect,
                google_drive_progress,
                google_drive_list,
                google_drive_create_backup,
                google_drive_download,
                google_drive_inspect,
                google_drive_restore,
                crate::recovery::create_backup,
                crate::recovery::restore_backup,
                crate::app_lock::lock_app
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let view = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        // Retain the compatible discovery reservation through snapshot/restart.
        Self {
            root,
            app,
            view,
            state,
            node,
            _discovery_reservation: reservation,
        }
    }
    fn invoke(&self, command: &str, mut body: Value) -> Result<Value, Value> {
        body.as_object_mut()
            .unwrap()
            .entry("connectionId")
            .or_insert(json!(CONNECTION));
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
    fn grant(&self, client: &DriveClient) -> Arc<Connection> {
        let connection = Arc::new(Connection::new(
            self.state.session().clone(),
            self.state.app_lock().clone(),
        ));
        *connection.token.try_lock().unwrap() = Some("fixture-access-token".into());
        let drive = self.app.state::<DriveState>();
        *drive.client_override.lock().unwrap() = Some(client.clone());
        drive
            .connections
            .lock()
            .unwrap()
            .insert(CONNECTION.into(), connection.clone());
        connection
    }
    fn sign_in(&self) -> crate::services::user::User {
        let user = self
            .state
            .auth_service()
            .register("fixture-user".into(), PASSWORD.into(), "fixture".into())
            .unwrap();
        self.state
            .auth_service()
            .login("fixture-user".into(), PASSWORD.into())
            .unwrap();
        let port = self.node.4.lock().unwrap().as_ref().unwrap().1;
        let runtime = tauri::async_runtime::block_on(NodeRuntime::start(
            self.root.path(),
            &user.user_id,
            "Fixture",
            PASSWORD,
            port,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        *self.node.0.blocking_write() = Some(runtime);
        user
    }
    fn wait_for_runtime(&self) {
        tauri::async_runtime::block_on(async {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if self.node.0.read().await.is_some() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.state.session().clear();
        tauri::async_runtime::block_on(async {
            let _permit = self.node.lock_profile_lifecycle().await;
            if let Some(runtime) = self.node.0.write().await.take() {
                runtime.stop().await;
            }
        });
    }
}

#[derive(Default)]
struct ProviderState {
    archive: Vec<u8>,
    complete: bool,
    denied: bool,
    requests: Vec<Vec<u8>>,
}
struct Provider {
    client: DriveClient,
    state: Arc<Mutex<ProviderState>>,
    task: tauri::async_runtime::JoinHandle<()>,
}
impl Provider {
    fn new() -> Self {
        let listener =
            tauri::async_runtime::block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let client = DriveClient::for_test(&origin);
        let state = Arc::new(Mutex::new(ProviderState::default()));
        let server_state = state.clone();
        let task = tauri::async_runtime::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut chunk = [0; 4096];
                    let n = socket.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..n]);
                    if let Some(index) = request.windows(4).position(|p| p == b"\r\n\r\n") {
                        break index + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|n| n.parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let first = headers.lines().next().unwrap().to_owned();
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("authorization: bearer fixture-access-token")
                );
                while request.len() < header_end + length {
                    let mut chunk = [0; 8192];
                    let n = socket.read(&mut chunk).await.unwrap();
                    assert_ne!(n, 0);
                    request.extend_from_slice(&chunk[..n]);
                }
                let (status, extra, body) = {
                    let mut state = server_state.lock().unwrap();
                    state.requests.push(request.clone());
                    let metadata = |state: &ProviderState| json!({"id":"fixture_archive", "name":"fixture.mtbackup", "size":state.archive.len().to_string(), "sha256Checksum":hex::encode(sha2::Sha256::digest(&state.archive)), "appProperties":{"meshTalkBackup":"1","complete":state.complete.to_string()}});
                    if state.denied {
                        (401, String::new(), b"fixture-private-error-body".to_vec())
                    } else if first.starts_with("POST ") {
                        (
                            201,
                            format!(
                                "Location: {origin}/upload/drive/v3/files?upload_id=fixture\r\n"
                            ),
                            b"{}".to_vec(),
                        )
                    } else if first.starts_with("PUT ") {
                        state.archive.extend_from_slice(&request[header_end..]);
                        (
                            200,
                            String::new(),
                            metadata(&state).to_string().into_bytes(),
                        )
                    } else if first.starts_with("PATCH ") {
                        state.complete = true;
                        (200, String::new(), b"{}".to_vec())
                    } else if first.contains("alt=media") {
                        (200, String::new(), state.archive.clone())
                    } else if first.starts_with("GET /drive/v3/files/fixture_archive?") {
                        (
                            200,
                            String::new(),
                            metadata(&state).to_string().into_bytes(),
                        )
                    } else {
                        (200,String::new(),json!({"files":if state.complete {vec![metadata(&state)]} else {vec![]}}).to_string().into_bytes())
                    }
                };
                socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",body.len()).as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        Self {
            client,
            state,
            task,
        }
    }
}
impl Drop for Provider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
use sha2::Digest;

#[test]
fn registered_cloud_commands_round_trip_real_archive_and_restart_runtime() {
    let source = Fixture::new();
    let provider = Provider::new();
    source.sign_in();
    let (account, device, channel, favorite, attachment, media) =
        tauri::async_runtime::block_on(async {
            let node = source.node.node_handle().await.unwrap();
            let channel = node.create_channel("Portable 你好", vec![]).await.unwrap();
            node.send_channel_message(channel, "encrypted cloud fixture 🌍".as_bytes())
                .await
                .unwrap();
            let attachment_path = source.root.path().join("portable-说明.txt");
            std::fs::write(&attachment_path, b"independently retained cloud attachment").unwrap();
            let attachment = node
                .send_file_channel(
                    channel,
                    &attachment_path,
                    mesh_talk_core::file::FileKind::File,
                )
                .await
                .unwrap();
            let media_path = source.root.path().join("portable-image.png");
            std::fs::write(&media_path, b"independently retained cloud media").unwrap();
            let media = node
                .send_file_channel(channel, &media_path, mesh_talk_core::file::FileKind::Media)
                .await
                .unwrap();
            let ids: Vec<_> = node
                .channel_history(channel, 100)
                .iter()
                .map(|entry| entry.id.to_hex())
                .collect();
            let favorite = node
                .save_message_favorite_guarded(
                    mesh_talk_core::node::FavoriteSource {
                        kind: "channel".into(),
                        target: hex::encode(channel.as_bytes()),
                        message_ids: ids.clone(),
                    },
                    mesh_talk_core::node::FavoriteMetadata {
                        title: "云端收藏 🌍".into(),
                        description: "Portable title and description".into(),
                        notes: std::collections::BTreeMap::from([(
                            ids[0].clone(),
                            "保存したメモ ✅".into(),
                        )]),
                    },
                    |commit| commit(),
                )
                .unwrap();
            assert!(node.favorite_file_available(attachment));
            assert!(node.favorite_file_available(media));
            (
                node.account_id(),
                node.user_id(),
                channel,
                favorite,
                attachment,
                media,
            )
        });
    source.grant(&provider.client);
    assert_eq!(
        source.invoke("google_drive_list", json!({})).unwrap(),
        json!([])
    );
    let key = RecoveryKey::generate().display();
    let backup = source
        .invoke("google_drive_create_backup", json!({"recoveryKey":key}))
        .unwrap();
    assert_eq!(backup["preview"]["account_id"], account);
    assert_eq!(backup["preview"]["favorites"], 1);
    assert_eq!(backup["preview"]["favorite_files"], 2);
    assert_eq!(backup["preview"]["media_files"], 1);
    assert_eq!(backup["preview"]["missing_favorite_files"], 0);
    source.wait_for_runtime();
    assert_eq!(
        source.invoke("google_drive_progress", json!({})).unwrap()["phase"],
        "complete"
    );
    let status: BackupStatus = FileManager::new(source.root.path().to_path_buf())
        .read_encrypted_file("fixture-user", "meta/backup_status.enc", PASSWORD)
        .unwrap();
    assert!(status.destination.starts_with("Google Drive / "));
    let listed = source.invoke("google_drive_list", json!({})).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], "fixture_archive");
    source.invoke("google_drive_disconnect", json!({})).unwrap();
    assert!(source.invoke("google_drive_list", json!({})).is_err());

    let target = Fixture::new();
    let connection = target.grant(&provider.client);
    assert!(
        target
            .invoke("google_drive_inspect", json!({"recoveryKey":key}))
            .is_err()
    );
    target
        .invoke("google_drive_download", json!({"fileId":"fixture_archive"}))
        .unwrap();
    let downloaded = connection
        .archive
        .try_lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .path()
        .to_path_buf();
    assert!(downloaded.exists());
    assert_eq!(
        target.invoke("google_drive_progress", json!({})).unwrap()["phase"],
        "downloaded"
    );
    assert_eq!(
        target
            .invoke(
                "google_drive_inspect",
                json!({"recoveryKey":RecoveryKey::generate().display()})
            )
            .unwrap_err()["kind"],
        "crypto"
    );
    let preview = target
        .invoke("google_drive_inspect", json!({"recoveryKey":key}))
        .unwrap();
    assert_eq!(preview["account_id"], account);
    assert!(preview["messages"].as_u64().unwrap() > 0);
    let restored = target
        .invoke(
            "google_drive_restore",
            json!({"recoveryKey":key,"username":"restored-user","newPassword":PASSWORD}),
        )
        .unwrap();
    assert_eq!(restored["preview"]["account_id"], account);
    assert_ne!(restored["device_id"], device);
    let restored_user = target
        .state
        .auth_service()
        .register("occupied-user".into(), PASSWORD.into(), "fixture".into())
        .unwrap();
    assert!(!restored_user.user_id.is_empty());
    assert_eq!(
        target
            .invoke(
                "google_drive_restore",
                json!({"recoveryKey":key,"username":"occupied-user","newPassword":PASSWORD})
            )
            .unwrap_err()["kind"],
        "invalid-input"
    );
    let (user, _) = target
        .state
        .auth_service()
        .login("restored-user".into(), PASSWORD.into())
        .unwrap();
    let reopened = Node::open_recovery_profile(
        &target.root.path().join("accounts").join(user.user_id),
        PASSWORD,
    )
    .unwrap();
    assert_eq!(reopened.account_id(), account);
    assert_eq!(
        reopened.list_message_favorites("", 0, 10).unwrap().items,
        vec![favorite]
    );
    assert_eq!(
        sha2::Sha256::digest(reopened.read_favorite_file(attachment).unwrap()),
        sha2::Sha256::digest(b"independently retained cloud attachment"),
    );
    assert_eq!(
        sha2::Sha256::digest(reopened.read_favorite_file(media).unwrap()),
        sha2::Sha256::digest(b"independently retained cloud media"),
    );
    assert!(
        reopened
            .channel_history(channel, 100)
            .iter()
            .any(|entry| entry.text == "encrypted cloud fixture 🌍".as_bytes())
    );
    drop(reopened);
    target.invoke("google_drive_disconnect", json!({})).unwrap();
    drop(connection);
    assert!(!downloaded.exists());
    let requests = provider.state.lock().unwrap();
    assert!(requests.complete);
    assert!(
        !requests
            .archive
            .windows(key.len())
            .any(|bytes| bytes == key.as_bytes())
    );
}

#[test]
fn registered_cloud_commands_reject_retired_grants_and_admission_failures() {
    let fixture = Fixture::new();
    let provider = Provider::new();
    for (name, args) in [
        ("google_drive_list", json!({})),
        ("google_drive_download", json!({"fileId":"fixture_archive"})),
        ("google_drive_inspect", json!({"recoveryKey":"invalid"})),
        (
            "google_drive_create_backup",
            json!({"recoveryKey":"invalid"}),
        ),
        (
            "google_drive_restore",
            json!({"recoveryKey":"invalid","username":"new-user","newPassword":PASSWORD}),
        ),
    ] {
        assert_eq!(fixture.invoke(name, args).unwrap_err()["kind"], "auth");
    }
    for id in [
        "short".to_owned(),
        "invalid/connection-identifier".to_owned(),
        "a".repeat(129),
    ] {
        assert_eq!(
            fixture
                .invoke("google_drive_connect", json!({"connectionId":id}))
                .unwrap_err()["kind"],
            "invalid-input"
        );
    }
    let connection = fixture.grant(&provider.client);
    assert_eq!(
        fixture.invoke("google_drive_progress", json!({})).unwrap()["phase"],
        "connecting"
    );
    assert_eq!(
        fixture
            .invoke("google_drive_connect", json!({}))
            .unwrap_err()["kind"],
        "invalid-input"
    );
    assert_eq!(
        fixture
            .invoke(
                "google_drive_create_backup",
                json!({"recoveryKey":RecoveryKey::generate().display()})
            )
            .unwrap_err()["kind"],
        "auth"
    );
    let _busy = connection.begin().unwrap();
    assert_eq!(
        fixture
            .invoke("google_drive_download", json!({"fileId":"fixture_archive"}))
            .unwrap_err()["kind"],
        "invalid-input"
    );
    drop(_busy);
    *connection.token.try_lock().unwrap() = None;
    assert_eq!(
        fixture.invoke("google_drive_list", json!({})).unwrap_err()["kind"],
        "auth"
    );
    *connection.token.try_lock().unwrap() = Some("fixture-access-token".into());
    provider.state.lock().unwrap().denied = true;
    let error = fixture.invoke("google_drive_list", json!({})).unwrap_err();
    assert_eq!(error["kind"], "auth");
    assert!(!error.to_string().contains("fixture-private-error-body"));
    fixture.state.session().clear();
    assert_eq!(
        fixture
            .invoke("google_drive_progress", json!({}))
            .unwrap_err()["kind"],
        "authorization"
    );
    fixture
        .invoke("google_drive_disconnect", json!({}))
        .unwrap();
    if !oauth::configured() {
        assert_eq!(
            fixture
                .invoke("google_drive_connect", json!({}))
                .unwrap_err()["kind"],
            "invalid-input"
        );
        assert!(
            fixture
                .app
                .state::<DriveState>()
                .connections
                .lock()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn registered_local_recovery_and_cloud_generation_checks_share_the_same_authority() {
    let fixture = Fixture::new();
    let provider = Provider::new();
    fixture.sign_in();
    fixture.grant(&provider.client);
    assert_eq!(
        fixture
            .invoke(
                "google_drive_create_backup",
                json!({"recoveryKey":"invalid"})
            )
            .unwrap_err()["kind"],
        "invalid-input"
    );
    let key = RecoveryKey::generate().display();
    let archive = fixture.root.path().join("local.mtbackup");
    fixture
        .invoke(
            "create_backup",
            json!({"destination":archive,"recoveryKey":key}),
        )
        .unwrap();
    fixture.wait_for_runtime();
    assert!(archive.exists());
    let status: BackupStatus = FileManager::new(fixture.root.path().to_path_buf())
        .read_encrypted_file("fixture-user", "meta/backup_status.enc", PASSWORD)
        .unwrap();
    assert_eq!(status.destination, archive.display().to_string());
    let state = fixture.state.clone();
    let old = state.session().generation();
    let error = tauri::async_runtime::block_on(recovery::create_backup_with_status(
        fixture.app.handle().clone(),
        &state,
        &fixture.node,
        fixture
            .root
            .path()
            .join("stale.mtbackup")
            .display()
            .to_string(),
        key.clone(),
        false,
        Some(old + 1),
    ))
    .err()
    .expect("stale generation must fail");
    assert!(matches!(error, CommandError::Authorization(_)));
    assert_eq!(fixture.invoke("restore_backup",json!({"path":archive,"recoveryKey":key,"username":"new-user","newPassword":PASSWORD})).unwrap_err()["kind"],"authorization");
    fixture.invoke("lock_app", json!({})).unwrap();
    assert_eq!(
        fixture.invoke("google_drive_list", json!({})).unwrap_err()["kind"],
        "authorization"
    );
}

#[test]
fn legacy_saved_backup_preview_defaults_favorites_counts() {
    let preview: mesh_talk_core::recovery::BackupPreview = serde_json::from_value(json!({
        "account_id": "legacy-account", "created_at_ms": 1, "messages": 2,
        "channels": 0, "file_cards": 0, "media_files": 0, "missing_media": 0,
    }))
    .unwrap();
    assert_eq!(
        (
            preview.favorites,
            preview.favorite_files,
            preview.missing_favorite_files
        ),
        (0, 0, 0)
    );
}

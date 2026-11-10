//! Exercise registered native commands with real encrypted storage and a loopback Drive API.
use super::*;
use mesh_talk_core::identity::manager::IdentityManager;
use mesh_talk_core::node::{Node, NodeRuntime};
use mesh_talk_core::recovery::key::RecoveryKey;
use serde_json::{Value, json};
use std::path::Path;
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
            .manage(crate::reception_host::ReceptionHostState::default())
            .manage(DriveState::default())
            .manage(crate::settings::SettingsState::isolated(
                root.path().join("settings.json"),
            ))
            .invoke_handler(tauri::generate_handler![
                google_drive_available,
                google_drive_connect,
                google_drive_resume,
                google_drive_forget,
                google_drive_disconnect,
                google_drive_progress,
                google_drive_list,
                google_drive_create_backup,
                google_drive_download,
                google_drive_inspect,
                google_drive_restore,
                crate::recovery::create_backup,
                crate::recovery::prepare_backup_key,
                crate::recovery::backup_status,
                crate::recovery::setup::backup_setup,
                crate::recovery::setup::confirm_backup_key,
                crate::recovery::setup::set_backup_destination,
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
            // The runtime prepares the signed channel view in bounded turns.
            // Wait for that selected read before capturing this archive fixture.
            let history = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match node.channel_history(channel, 100) {
                        Ok(history) => break history,
                        Err(mesh_talk_core::node::NodeError::Log(
                            mesh_talk_core::eventlog::LogError::StaleCursor,
                        )) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                        Err(_) => panic!("the selected cloud fixture history must be readable"),
                    }
                }
            })
            .await
            .expect("the live runtime must prepare its selected cloud fixture history");
            let ids: Vec<_> = history.iter().map(|entry| entry.id.to_hex()).collect();
            assert_eq!(
                ids.len(),
                3,
                "the archive must include all three fixture messages"
            );
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
    source.state.app_lock().set_identity_verifier(|| Ok(()));
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
    // This bare offline owner has no worker to prepare channel projections.
    // A second backup must drive its selected favorite sources itself and must
    // never turn pending protection checks into a successful, lossy archive.
    let offline_key = RecoveryKey::generate();
    let mut offline_archive = Vec::new();
    reopened
        .write_encrypted_recovery_archive(&mut offline_archive, &offline_key, 1)
        .unwrap();
    let offline_preview = Node::inspect_encrypted_recovery_archive(
        std::io::Cursor::new(&offline_archive),
        &offline_key,
        target.root.path(),
    )
    .unwrap();
    assert_eq!(offline_preview.favorites, 1);
    assert_eq!(offline_preview.favorite_files, 2);
    assert_eq!(offline_preview.missing_favorite_files, 0);
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
            .unwrap()
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
    fixture.state.app_lock().set_identity_verifier(|| Ok(()));
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
        None,
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

#[test]
fn backup_key_is_reused_when_the_dialog_reopens() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let first = fixture.invoke("prepare_backup_key", json!({})).unwrap();
    let second = fixture.invoke("prepare_backup_key", json!({})).unwrap();
    assert_eq!(
        first, second,
        "reopening backup must not replace its recovery key"
    );
}

#[test]
fn backup_setup_acknowledgement_and_destination_survive_reopen() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let initial = fixture.invoke("backup_setup", json!({})).unwrap();
    assert_eq!(initial["confirmed"], false);
    assert_eq!(initial["destination"], "local");
    let wrong = RecoveryKey::generate().display();
    assert!(
        fixture
            .invoke("confirm_backup_key", json!({"recoveryKey":wrong}))
            .is_err()
    );
    assert_eq!(
        fixture.invoke("backup_setup", json!({})).unwrap()["confirmed"],
        false
    );
    fixture
        .invoke(
            "confirm_backup_key",
            json!({"recoveryKey":initial["recovery_key"]}),
        )
        .unwrap();
    fixture
        .invoke("set_backup_destination", json!({"destination":"drive"}))
        .unwrap();
    let next = fixture.invoke("backup_setup", json!({})).unwrap();
    assert_eq!(next["confirmed"], true);
    assert_eq!(next["destination"], "drive");
    assert_eq!(next["recovery_key"], initial["recovery_key"]);
    assert!(
        fixture
            .invoke(
                "set_backup_destination",
                json!({"destination":"https://other.invalid"})
            )
            .is_err()
    );
    let encrypted = std::fs::read(
        fixture
            .root
            .path()
            .join("users/fixture-user/meta/backup_setup.enc"),
    )
    .unwrap();
    let key = next["recovery_key"].as_str().unwrap();
    assert!(
        !encrypted
            .windows(key.len())
            .any(|bytes| bytes == key.as_bytes())
    );
    fixture.state.session().clear();
    assert!(fixture.invoke("backup_setup", json!({})).is_err());
    fixture
        .state
        .auth_service()
        .login("fixture-user".into(), PASSWORD.into())
        .unwrap();
    assert_eq!(fixture.invoke("backup_setup", json!({})).unwrap(), next);
}

#[test]
fn saved_key_backup_requires_one_time_confirmation_and_remains_readable() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let setup = fixture.invoke("backup_setup", json!({})).unwrap();
    let path = fixture
        .root
        .path()
        .join("first.mtbackup")
        .display()
        .to_string();
    let failure = fixture
        .invoke("create_backup", json!({"destination":path}))
        .unwrap_err();
    assert!(
        failure["message"]
            .as_str()
            .unwrap()
            .contains("Save your recovery key")
    );
    assert!(!Path::new(&path).exists());
    fixture
        .invoke(
            "confirm_backup_key",
            json!({"recoveryKey":setup["recovery_key"]}),
        )
        .unwrap();
    fixture.state.app_lock().set_identity_verifier(|| Ok(()));
    let first = fixture
        .invoke("create_backup", json!({"destination":path}))
        .unwrap();
    fixture.wait_for_runtime();
    fixture.state.app_lock().set_identity_verifier(|| Ok(()));
    let second = fixture
        .invoke(
            "create_backup",
            json!({"destination":fixture.root.path().join("second.mtbackup")}),
        )
        .unwrap();
    fixture.wait_for_runtime();
    assert_eq!(first["recovery_key"], setup["recovery_key"]);
    assert_eq!(second["recovery_key"], setup["recovery_key"]);
    let key = RecoveryKey::parse(setup["recovery_key"].as_str().unwrap()).unwrap();
    let preview = Node::inspect_encrypted_recovery_archive(
        std::fs::File::open(&path).unwrap(),
        &key,
        fixture.root.path(),
    )
    .unwrap();
    assert_eq!(preview.account_id, first["preview"]["account_id"]);
    let encrypted_second = std::fs::read(fixture.root.path().join("second.mtbackup")).unwrap();
    assert_ne!(
        std::fs::read(&path).unwrap(),
        encrypted_second,
        "reused keys must retain fresh archive salts"
    );
}

#[test]
fn corrupt_saved_setup_is_never_silently_replaced() {
    let fixture = Fixture::new();
    fixture.sign_in();
    fixture.invoke("backup_setup", json!({})).unwrap();
    let path = fixture
        .root
        .path()
        .join("users/fixture-user/meta/backup_setup.enc");
    std::fs::write(&path, b"damaged encrypted setup").unwrap();
    assert!(fixture.invoke("backup_setup", json!({})).is_err());
    assert_eq!(std::fs::read(path).unwrap(), b"damaged encrypted setup");
}

struct RefreshProvider {
    endpoint: String,
    requests: Arc<Mutex<Vec<String>>>,
    task: tauri::async_runtime::JoinHandle<()>,
}
impl RefreshProvider {
    fn new(status: &'static str) -> Self {
        let body = if status == "400 Bad Request" {
            r#"{"error":"invalid_grant"}"#
        } else {
            r#"{"access_token":"fixture-access-token","token_type":"Bearer","expires_in":3600}"#
        };
        Self::with_response(status, body)
    }
    fn with_response(status: &'static str, body: &'static str) -> Self {
        let listener =
            tauri::async_runtime::block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let received = requests.clone();
        let task = tauri::async_runtime::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let read = socket.read(&mut chunk).await.unwrap();
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&chunk[..read]);
                    assert!(bytes.len() < 32 * 1024);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|value| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                received
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(bytes).unwrap());
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        Self {
            endpoint,
            requests,
            task,
        }
    }
}
impl Drop for RefreshProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn remember_grant(fixture: &Fixture, token: &str) -> (remembered::Owner, Arc<Connection>) {
    let owner = remembered::Owner::capture(&fixture.state, &fixture.node).unwrap();
    let connection = Arc::new(Connection::new(
        fixture.state.session().clone(),
        fixture.state.app_lock().clone(),
    ));
    tauri::async_runtime::block_on(owner.save(token.into(), connection.clone())).unwrap();
    (owner, connection)
}
fn refresh_provider(fixture: &Fixture, provider: &RefreshProvider) {
    *fixture
        .app
        .state::<DriveState>()
        .refresh_override
        .lock()
        .unwrap() = Some(provider.endpoint.clone());
}

#[test]
fn remembered_grant_resumes_refreshes_and_is_explicitly_forgotten() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let provider = Provider::new();
    *fixture
        .app
        .state::<DriveState>()
        .client_override
        .lock()
        .unwrap() = Some(provider.client.clone());
    let refresh = RefreshProvider::new("200 OK");
    refresh_provider(&fixture, &refresh);
    let (owner, _) = remember_grant(&fixture, "fixture-refresh-token");
    let encrypted = std::fs::read(
        fixture
            .root
            .path()
            .join("users/fixture-user/meta/google_drive.enc"),
    )
    .unwrap();
    assert!(
        !encrypted
            .windows(21)
            .any(|part| part == b"fixture-refresh-token")
    );
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        true
    );
    assert_eq!(
        fixture.invoke("google_drive_list", json!({})).unwrap(),
        json!([])
    );
    let connection = fixture.app.state::<DriveState>().get(CONNECTION).unwrap();
    *connection.token_expires.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(
        fixture.invoke("google_drive_list", json!({})).unwrap(),
        json!([])
    );
    assert_eq!(refresh.requests.lock().unwrap().len(), 2);
    for request in refresh.requests.lock().unwrap().iter() {
        assert!(request.contains("grant_type=refresh_token"));
        assert!(request.contains("refresh_token=fixture-refresh-token"));
        assert!(!request.contains("recovery"));
    }
    fixture
        .invoke("google_drive_disconnect", json!({}))
        .unwrap();
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_some(),
        "closing retains the encrypted grant"
    );
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        true
    );
    fixture.invoke("google_drive_forget", json!({})).unwrap();
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        false
    );
}

#[test]
fn signed_out_restore_and_other_local_users_never_borrow_saved_access_or_keys() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let setup = fixture.invoke("backup_setup", json!({})).unwrap();
    let (old_owner, _) = remember_grant(&fixture, "fixture-refresh-token");
    fixture.state.session().clear();
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        false
    );
    assert!(tauri::async_runtime::block_on(old_owner.load()).is_err());
    fixture
        .state
        .auth_service()
        .register("other-user".into(), PASSWORD.into(), "other".into())
        .unwrap();
    fixture
        .state
        .auth_service()
        .login("other-user".into(), PASSWORD.into())
        .unwrap();
    let other = remembered::Owner::capture(&fixture.state, &fixture.node).unwrap();
    assert!(matches!(
        tauri::async_runtime::block_on(old_owner.load()),
        Err(CommandError::Authorization(_))
    ));
    assert!(
        tauri::async_runtime::block_on(other.load())
            .unwrap()
            .is_none()
    );
    let other_setup = fixture.invoke("backup_setup", json!({})).unwrap();
    assert_ne!(other_setup["recovery_key"], setup["recovery_key"]);
    assert_eq!(other_setup["confirmed"], false);
}

#[test]
fn cancelled_oauth_cannot_restore_a_forgotten_grant() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let (owner, connection) = remember_grant(&fixture, "fixture-refresh-token");
    connection.cancelled.send_replace(true);
    tauri::async_runtime::block_on(owner.forget()).unwrap();
    assert!(
        tauri::async_runtime::block_on(owner.save("late-refresh-token".into(), connection))
            .is_err()
    );
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_none()
    );
}

#[test]
fn rejected_refresh_forgets_only_the_rejected_grant_and_network_errors_retain_it() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let (owner, old_connection) = remember_grant(&fixture, "old-refresh-token");
    remember_grant(&fixture, "new-refresh-token");
    tauri::async_runtime::block_on(
        owner.forget_rejected("old-refresh-token".into(), old_connection),
    )
    .unwrap();
    assert_eq!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .as_deref(),
        Some("new-refresh-token")
    );
    for status in [
        "408 Request Timeout",
        "429 Too Many Requests",
        "401 Unauthorized",
    ] {
        let temporary = RefreshProvider::new(status);
        refresh_provider(&fixture, &temporary);
        assert!(fixture.invoke("google_drive_resume", json!({})).is_err());
        assert!(
            tauri::async_runtime::block_on(owner.load())
                .unwrap()
                .is_some(),
            "a retryable/configuration failure must retain remembered access"
        );
    }
    let unavailable = RefreshProvider::new("503 Service Unavailable");
    refresh_provider(&fixture, &unavailable);
    assert_eq!(
        fixture
            .invoke("google_drive_resume", json!({}))
            .unwrap_err()["kind"],
        "io"
    );
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_some()
    );
    let revoked = RefreshProvider::new("400 Bad Request");
    refresh_provider(&fixture, &revoked);
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        false
    );
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_none()
    );
}

#[test]
fn app_lock_blocks_saved_keys_and_remembered_google_grants() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let (owner, _) = remember_grant(&fixture, "fixture-refresh-token");
    fixture.invoke("lock_app", json!({})).unwrap();
    assert!(fixture.invoke("backup_setup", json!({})).is_err());
    assert!(tauri::async_runtime::block_on(owner.load()).is_err());
    assert!(fixture.invoke("google_drive_resume", json!({})).is_err());
}

#[test]
fn backup_status_is_private_durable_and_reports_corruption_without_changing_the_key() {
    let fixture = Fixture::new();
    fixture.sign_in();
    assert_eq!(
        fixture.invoke("backup_status", json!({})).unwrap(),
        Value::Null
    );
    let setup = fixture.invoke("backup_setup", json!({})).unwrap();
    fixture
        .invoke(
            "confirm_backup_key",
            json!({"recoveryKey":setup["recovery_key"]}),
        )
        .unwrap();
    let path = fixture.root.path().join("status.mtbackup");
    fixture.state.app_lock().set_identity_verifier(|| Ok(()));
    let created = fixture
        .invoke("create_backup", json!({"destination":path}))
        .unwrap();
    fixture.wait_for_runtime();
    let status = fixture.invoke("backup_status", json!({})).unwrap();
    assert_eq!(status["destination"], json!(path));
    assert_eq!(status["preview"], created["preview"]);
    assert_eq!(status["archive_bytes"], created["archive_bytes"]);
    assert!(status.get("recovery_key").is_none());
    let status_path = fixture
        .root
        .path()
        .join("users/fixture-user/meta/backup_status.enc");
    std::fs::write(&status_path, b"damaged status metadata").unwrap();
    assert_eq!(
        fixture.invoke("backup_status", json!({})).unwrap_err()["kind"],
        "io"
    );
    assert_eq!(
        fixture.invoke("backup_setup", json!({})).unwrap()["recovery_key"],
        setup["recovery_key"]
    );
    fixture.state.session().clear();
    assert_eq!(
        fixture.invoke("backup_status", json!({})).unwrap_err()["kind"],
        "auth"
    );
    fixture
        .state
        .auth_service()
        .register("other-user".into(), PASSWORD.into(), "other".into())
        .unwrap();
    fixture
        .state
        .auth_service()
        .login("other-user".into(), PASSWORD.into())
        .unwrap();
    assert_eq!(
        fixture.invoke("backup_status", json!({})).unwrap(),
        Value::Null
    );
}

#[test]
fn malformed_saved_setup_and_confirmation_never_replace_a_recovery_key() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let initial = fixture.invoke("backup_setup", json!({})).unwrap();
    assert_eq!(
        fixture
            .invoke(
                "confirm_backup_key",
                json!({"recoveryKey":"not a recovery key"})
            )
            .unwrap_err()["kind"],
        "invalid-input"
    );
    assert_eq!(fixture.invoke("backup_setup", json!({})).unwrap(), initial);
    let manager = FileManager::new(fixture.root.path().to_path_buf());
    for (key, destination, kind) in [
        ("invalid key".to_string(), "local", "crypto"),
        (
            initial["recovery_key"].as_str().unwrap().to_string(),
            "unsupported",
            "invalid-input",
        ),
    ] {
        let setup = crate::recovery::setup::BackupSetup {
            recovery_key: key,
            confirmed: true,
            destination: destination.into(),
        };
        manager
            .write_encrypted_file("fixture-user", "meta/backup_setup.enc", &setup, PASSWORD)
            .unwrap();
        let path = fixture
            .root
            .path()
            .join("users/fixture-user/meta/backup_setup.enc");
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            fixture.invoke("backup_setup", json!({})).unwrap_err()["kind"],
            kind
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "invalid stored setup must not silently generate another key"
        );
    }
}

#[test]
fn expired_without_refresh_requires_sign_in_without_using_an_expired_access_token() {
    let fixture = Fixture::new();
    let connection = fixture
        .app
        .state::<DriveState>()
        .insert(CONNECTION, &fixture.state, &fixture.node)
        .unwrap();
    *connection.token.try_lock().unwrap() = Some("expired-access-token".into());
    *connection.token_expires.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
    assert!(matches!(
        tauri::async_runtime::block_on(connection.access_token()),
        Err(CommandError::Auth(_))
    ));
}

#[test]
fn rotated_refresh_grants_are_saved_and_used_by_the_next_native_refresh() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let (owner, _) = remember_grant(&fixture, "original-refresh-grant");
    let refresh = RefreshProvider::with_response(
        "200 OK",
        r#"{"access_token":"fixture-access-token","token_type":"Bearer","expires_in":3600,"refresh_token":"rotated-refresh-grant"}"#,
    );
    refresh_provider(&fixture, &refresh);
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        true
    );
    assert_eq!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .as_deref(),
        Some("rotated-refresh-grant")
    );
    let connection = fixture.app.state::<DriveState>().get(CONNECTION).unwrap();
    *connection.token_expires.lock().unwrap() = Some(Instant::now() - Duration::from_secs(1));
    assert_eq!(
        tauri::async_runtime::block_on(connection.access_token()).unwrap(),
        "fixture-access-token"
    );
    let requests = refresh.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("refresh_token=original-refresh-grant"));
    assert!(requests[1].contains("refresh_token=rotated-refresh-grant"));
    drop(requests);
    // A retired lock lifetime cannot replace a saved grant with a late rotation.
    fixture.state.app_lock().clear();
    let late: oauth::TokenResponse = serde_json::from_value(json!({"access_token":"late-access", "token_type":"Bearer", "expires_in":3600, "refresh_token":"late-refresh"})).unwrap();
    assert!(tauri::async_runtime::block_on(connection.install_token(late)).is_err());
    assert_eq!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .as_deref(),
        Some("rotated-refresh-grant")
    );
}

#[test]
fn signed_out_authorization_keeps_its_refresh_grant_in_memory_only() {
    let fixture = Fixture::new();
    fixture.sign_in();
    remember_grant(&fixture, "local-user-refresh");
    fixture.state.session().clear();
    let connection = fixture
        .app
        .state::<DriveState>()
        .insert(CONNECTION, &fixture.state, &fixture.node)
        .unwrap();
    let tokens: oauth::TokenResponse = serde_json::from_value(json!({"access_token":"restore-access", "token_type":"Bearer", "expires_in":3600, "refresh_token":"temporary-restore-refresh"})).unwrap();
    tauri::async_runtime::block_on(connection.install_token(tokens)).unwrap();
    assert_eq!(
        tauri::async_runtime::block_on(connection.access_token()).unwrap(),
        "restore-access"
    );
    assert_eq!(
        connection.refresh_token.try_lock().unwrap().as_deref(),
        Some("temporary-restore-refresh")
    );
    fixture.invoke("google_drive_forget", json!({})).unwrap();
    fixture
        .state
        .auth_service()
        .login("fixture-user".into(), PASSWORD.into())
        .unwrap();
    let owner = remembered::Owner::capture(&fixture.state, &fixture.node).unwrap();
    assert_eq!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .as_deref(),
        Some("local-user-refresh"),
        "signed-out restore must neither replace nor forget another local user's saved grant"
    );
}

#[test]
fn invalid_or_foreign_client_grants_never_reach_the_token_endpoint() {
    #[derive(serde::Serialize)]
    struct StoredGrantFixture {
        client_id: String,
        refresh_token: String,
    }
    let fixture = Fixture::new();
    fixture.sign_in();
    let refresh = RefreshProvider::new("200 OK");
    refresh_provider(&fixture, &refresh);
    let manager = FileManager::new(fixture.root.path().to_path_buf());
    let path = fixture
        .root
        .path()
        .join("users/fixture-user/meta/google_drive.enc");
    manager
        .write_encrypted_file(
            "fixture-user",
            "meta/google_drive.enc",
            &StoredGrantFixture {
                client_id: format!("another-{}", oauth::client_id()),
                refresh_token: "foreign-client-refresh".into(),
            },
            PASSWORD,
        )
        .unwrap();
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        false
    );
    assert!(path.exists());
    for token in [String::new(), "x".repeat(16 * 1024 + 1)] {
        manager
            .write_encrypted_file(
                "fixture-user",
                "meta/google_drive.enc",
                &StoredGrantFixture {
                    client_id: oauth::client_id().into(),
                    refresh_token: token,
                },
                PASSWORD,
            )
            .unwrap();
        assert_eq!(
            fixture
                .invoke("google_drive_resume", json!({}))
                .unwrap_err()["kind"],
            "auth"
        );
    }
    std::fs::write(&path, b"corrupt remembered grant").unwrap();
    assert_eq!(
        fixture
            .invoke("google_drive_resume", json!({}))
            .unwrap_err()["kind"],
        "io"
    );
    assert!(refresh.requests.lock().unwrap().is_empty());
    fixture.invoke("google_drive_forget", json!({})).unwrap();
    assert!(
        !path.exists(),
        "explicit forget remains available when a cached grant is corrupt"
    );
    assert_eq!(
        fixture.invoke("google_drive_resume", json!({})).unwrap(),
        false
    );
}

#[test]
fn failed_first_setup_is_not_acknowledgeable_and_can_be_retried_after_storage_recovers() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let meta = fixture.root.path().join("users/fixture-user/meta");
    let retained = fixture.root.path().join("retained-auth-metadata");
    std::fs::rename(&meta, &retained).unwrap();
    std::fs::write(&meta, b"blocked metadata directory").unwrap();
    let failure = fixture.invoke("backup_setup", json!({})).unwrap_err();
    assert_eq!(failure["kind"], "io");
    assert!(failure.get("recovery_key").is_none());
    assert_eq!(std::fs::read(&meta).unwrap(), b"blocked metadata directory");
    std::fs::remove_file(&meta).unwrap();
    std::fs::rename(retained, meta).unwrap();
    let setup = fixture.invoke("backup_setup", json!({})).unwrap();
    assert_eq!(setup["confirmed"], false);
    assert!(RecoveryKey::parse(setup["recovery_key"].as_str().unwrap()).is_ok());
    assert_eq!(fixture.invoke("backup_setup", json!({})).unwrap(), setup);
}

#[test]
fn cloud_backup_uses_the_acknowledged_native_key_without_a_webview_key_argument() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let provider = Provider::new();
    fixture.grant(&provider.client);
    let setup = fixture.invoke("backup_setup", json!({})).unwrap();
    fixture
        .invoke(
            "confirm_backup_key",
            json!({"recoveryKey":setup["recovery_key"]}),
        )
        .unwrap();
    fixture.state.app_lock().set_identity_verifier(|| Ok(()));
    let backup = fixture
        .invoke("google_drive_create_backup", json!({}))
        .unwrap();
    fixture.wait_for_runtime();
    assert_eq!(backup["recovery_key"], setup["recovery_key"]);
    assert!(provider.state.lock().unwrap().complete);
    let status = fixture.invoke("backup_status", json!({})).unwrap();
    assert!(
        status["destination"]
            .as_str()
            .unwrap()
            .starts_with("Google Drive / ")
    );
    assert_eq!(status["preview"], backup["preview"]);
}

#[test]
fn failed_credential_storage_never_reports_remembered_or_forgotten_access() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let owner = remembered::Owner::capture(&fixture.state, &fixture.node).unwrap();
    let connection = Arc::new(Connection::new(
        fixture.state.session().clone(),
        fixture.state.app_lock().clone(),
    ));
    let grant_path = fixture
        .root
        .path()
        .join("users/fixture-user/meta/google_drive.enc");
    std::fs::create_dir_all(&grant_path).unwrap();
    let sentinel = grant_path.join("keep-existing-data");
    std::fs::write(&sentinel, b"unchanged").unwrap();
    let failure =
        tauri::async_runtime::block_on(owner.save("new-refresh-token".into(), connection.clone()))
            .unwrap_err();
    assert!(matches!(failure, CommandError::Io(_)));
    assert!(!failure.to_string().contains("new-refresh-token"));
    assert!(matches!(
        tauri::async_runtime::block_on(owner.forget()),
        Err(CommandError::Io(_))
    ));
    assert!(matches!(
        tauri::async_runtime::block_on(
            owner.forget_rejected("old-refresh-token".into(), connection)
        ),
        Err(CommandError::Io(_))
    ));
    assert_eq!(std::fs::read(sentinel).unwrap(), b"unchanged");
}

#[test]
fn retired_rejection_cannot_forget_valid_access_and_forgetting_is_idempotent() {
    let fixture = Fixture::new();
    fixture.sign_in();
    let (owner, connection) = remember_grant(&fixture, "current-refresh-token");
    connection.cancelled.send_replace(true);
    tauri::async_runtime::block_on(
        owner.forget_rejected("current-refresh-token".into(), connection),
    )
    .unwrap();
    assert_eq!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .as_deref(),
        Some("current-refresh-token")
    );
    tauri::async_runtime::block_on(owner.forget()).unwrap();
    tauri::async_runtime::block_on(owner.forget()).unwrap();
    let connection = Arc::new(Connection::new(
        fixture.state.session().clone(),
        fixture.state.app_lock().clone(),
    ));
    tauri::async_runtime::block_on(
        owner.forget_rejected("current-refresh-token".into(), connection),
    )
    .unwrap();
    assert!(
        tauri::async_runtime::block_on(owner.load())
            .unwrap()
            .is_none()
    );
}

#[test]
fn denied_sensitive_backup_commands_leave_files_and_runtime_untouched() {
    let fixture = Fixture::new();
    let provider = Provider::new();
    fixture.sign_in();
    fixture.grant(&provider.client);
    let account = fixture
        .node
        .0
        .blocking_read()
        .as_ref()
        .unwrap()
        .account_id()
        .to_owned();
    let destination = fixture.root.path().join("denied.mtbackup");
    for command in ["create_backup", "google_drive_create_backup"] {
        fixture.state.app_lock().set_identity_verifier(|| {
            Err(CommandError::Authentication("Verification canceled".into()))
        });
        let result = fixture.invoke(
            command,
            json!({
                "destination": destination,
                "recoveryKey": RecoveryKey::generate().display(),
            }),
        );
        assert_eq!(result.unwrap_err()["kind"], "auth");
        assert!(!destination.exists());
        assert_eq!(
            fixture
                .node
                .0
                .blocking_read()
                .as_ref()
                .unwrap()
                .account_id(),
            account
        );
        assert!(
            !fixture
                .root
                .path()
                .join("users/fixture-user/meta/backup_status.enc")
                .exists()
        );
    }
}

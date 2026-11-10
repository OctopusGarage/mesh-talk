use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const VERSION: &str = "99.0.0-nightly.42.1";
const PAYLOAD: &[u8] = b"updater regression payload";
const RELEASES: &str = "https://github.com/updater-test/releases";

#[derive(Clone)]
struct Response {
    body: Vec<u8>,
    pause_body: Option<Arc<tokio::sync::Notify>>,
}

struct HttpsRelease {
    http: reqwest::Client,
    routes: Arc<std::sync::Mutex<BTreeMap<String, Response>>>,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}

impl HttpsRelease {
    async fn new() -> Self {
        let mut ca_params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Mesh Talk HTTPS fixture CA");
        ca_params.key_usages = vec![
            rcgen::KeyUsagePurpose::DigitalSignature,
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::CrlSign,
        ];
        let ca_key = rcgen::KeyPair::generate().unwrap();
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = rcgen::Issuer::new(ca_params, ca_key);
        let mut leaf_params = rcgen::CertificateParams::new(vec!["github.com".into()]).unwrap();
        leaf_params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "github.com");
        leaf_params.use_authority_key_identifier_extension = true;
        leaf_params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
        leaf_params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let signing_key = rcgen::KeyPair::generate().unwrap();
        let cert = leaf_params.signed_by(&signing_key, &issuer).unwrap();
        let tls = tokio_rustls::rustls::ServerConfig::builder_with_provider(Arc::new(
            tokio_rustls::rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            // Root trust belongs to the client, not the transmitted server chain.
            vec![cert.der().clone()],
            tokio_rustls::rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der())
                .into(),
        )
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        // Keep canonical HTTPS URLs and hostname verification. Only DNS and the
        // explicitly trusted, throwaway CA differ from the real host. A CA-signed
        // server leaf also works with Windows' native certificate-chain verifier.
        let http = reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_der(ca.der().as_ref()).unwrap())
            .resolve("github.com", listener.local_addr().unwrap())
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let routes = Arc::new(std::sync::Mutex::new(BTreeMap::<String, Response>::new()));
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let server_routes = routes.clone();
        let server_requests = requests.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let acceptor = acceptor.clone();
                        let routes = server_routes.clone();
                        let requests = server_requests.clone();
                        connections.spawn(async move {
                            let Ok(mut stream) = acceptor.accept(socket).await else { return };
                            let mut header = Vec::new();
                            let mut buffer = [0; 1024];
                            while !header.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                                let Ok(count) = stream.read(&mut buffer).await else { return };
                                if count == 0 || header.len() + count > 16 * 1024 { return; }
                                header.extend_from_slice(&buffer[..count]);
                            }
                            let header = String::from_utf8(header).unwrap();
                            let path = header.lines().next().unwrap().split_whitespace().nth(1).unwrap();
                            requests.lock().unwrap().push(path.to_owned());
                            let response = routes.lock().unwrap().get(path).cloned();
                            let (status, body, pause) = match response {
                                Some(response) => ("200 OK", response.body, response.pause_body),
                                None => ("404 Not Found", Vec::new(), None),
                            };
                            let header = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                            if stream.write_all(header.as_bytes()).await.is_err() { return; }
                            if stream.flush().await.is_err() { return; }
                            if let Some(pause) = pause { pause.notified().await; }
                            let _ = stream.write_all(&body).await;
                            let _ = stream.shutdown().await;
                        });
                    },
                    Some(_) = connections.join_next(), if !connections.is_empty() => {},
                }
            }
        });
        Self {
            http,
            routes,
            requests,
            task,
        }
    }

    fn publish(&self, version: &str, payload: &[u8], with_signature: bool) -> String {
        let tag = format!("v{version}");
        let name = payload_name(&tag, std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let asset = |name: &str, bytes: &[u8]| {
            serde_json::json!({
                "name": name,
                "browser_download_url": format!("https://github.com/OctopusGarage/mesh-talk/releases/download/{tag}/{name}"),
                "size": bytes.len(),
                "digest": format!("sha256:{:x}", Sha256::digest(bytes)),
                "state": "uploaded",
            })
        };
        let signature = super::tests::FIXTURE_SIGNATURE.as_bytes();
        let mut assets = vec![asset(&name, payload)];
        if with_signature {
            assets.push(asset(&format!("{name}.sig"), signature));
        }
        let release = serde_json::json!({
            "tag_name": tag, "body": "HTTPS native preparation fixture", "draft": false,
            "prerelease": true, "assets": assets,
        });
        let payload_path = format!("/OctopusGarage/mesh-talk/releases/download/{tag}/{name}");
        let mut routes = self.routes.lock().unwrap();
        for (path, body) in [
            (
                format!("/updater-test/releases/tags/{tag}"),
                serde_json::to_vec(&release).unwrap(),
            ),
            (payload_path.clone(), payload.to_vec()),
            (format!("{payload_path}.sig"), signature.to_vec()),
        ] {
            routes.insert(
                path,
                Response {
                    body,
                    pause_body: None,
                },
            );
        }
        payload_path
    }

    fn host<'a>(
        &'a self,
        app: &'a tauri::AppHandle<tauri::test::MockRuntime>,
        key: &'a str,
    ) -> PreparationHost<'a, tauri::test::MockRuntime> {
        PreparationHost {
            app,
            metadata_http: self.http.clone(),
            signature_http: self.http.clone(),
            payload_http: self.http.clone(),
            releases: RELEASES,
            public_key: key,
            supported: true,
            base_source: delta::BaseSource {
                tag: env!("MESH_TALK_BUILD_TAG"),
                executable: Path::new(""),
            },
            available_space: |path| fs4::available_space(path),
        }
    }
}

impl Drop for HttpsRelease {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct TestApp {
    app: tauri::App<tauri::test::MockRuntime>,
    _profile: tempfile::TempDir,
}

fn test_app() -> TestApp {
    let profile = tempfile::tempdir().unwrap();
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().identifier = "com.mesh-talk.updater-https-test".into();
    context.config_mut().app.app_directories_override = Some(
        tauri::utils::config::AppDirectoriesOverride::Root(profile.path().to_owned()),
    );
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
    context
        .config_mut()
        .plugins
        .0
        .insert("updater".into(), config["plugins"]["updater"].clone());
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .build(context)
        .unwrap();
    TestApp {
        app,
        _profile: profile,
    }
}

fn registration_retired() {
    assert!(DOWNLOAD_CANCEL.get().unwrap().lock().unwrap().is_none());
}

async fn clear_stage() {
    *STAGED
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await = None;
}

#[tokio::test]
async fn https_preparation_stages_verified_bytes_identity_channel_and_progress() {
    let _state = TEST_STATE.lock().await;
    clear_stage().await;
    let app = test_app();
    let server = HttpsRelease::new().await;
    let payload_path = server.publish(VERSION, PAYLOAD, true);
    let progress = Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
    let observed = progress.clone();
    let result = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(move |message| {
            let tauri::ipc::InvokeResponseBody::Json(json) = message else {
                panic!("JSON progress")
            };
            observed
                .lock()
                .unwrap()
                .push(serde_json::from_str(&json).unwrap());
            Ok(())
        }),
    )
    .await
    .unwrap();
    assert!(result.installable);
    assert_eq!(std::fs::read(&result.path).unwrap(), PAYLOAD);
    let stage = STAGED.get().unwrap().lock().await;
    let staged = stage.as_ref().unwrap();
    assert_eq!(staged.update.version, VERSION);
    assert_eq!(staged.channel, UpdateChannel::Nightly);
    assert!(!staged.installed);
    assert_eq!(
        staged.directory.path().join("payload"),
        std::path::PathBuf::from(&result.path)
    );
    assert_eq!(result.file_name, payload_path.rsplit('/').next().unwrap());
    {
        let progress = progress.lock().unwrap();
        assert_eq!(
            progress.first().unwrap(),
            &serde_json::json!({"downloaded": 0, "total": PAYLOAD.len()})
        );
        assert_eq!(
            progress.last().unwrap(),
            &serde_json::json!({"downloaded": PAYLOAD.len(), "total": PAYLOAD.len()})
        );
        let requests = server.requests.lock().unwrap();
        assert_eq!(
            requests.as_slice(),
            [
                format!("/updater-test/releases/tags/v{VERSION}"),
                format!("{payload_path}.sig"),
                payload_path
            ]
        );
    }
    registration_retired();
    drop(stage);
    clear_stage().await;
    assert!(!Path::new(&result.path).exists());
}

#[tokio::test]
async fn https_preparation_rejects_digest_matching_tampering_untrusted_key_and_relabelled_version()
{
    let _state = TEST_STATE.lock().await;
    let app = test_app();
    for (version, bytes, key) in [
        (
            VERSION,
            b"tampered regression payload".as_slice(),
            super::tests::FIXTURE_KEY,
        ),
        (VERSION, PAYLOAD, "invalid-public-key"),
        ("99.0.0-nightly.42.2", PAYLOAD, super::tests::FIXTURE_KEY),
    ] {
        clear_stage().await;
        let server = HttpsRelease::new().await;
        server.publish(version, bytes, true);
        let error = prepare_from(
            server.host(app.app.handle(), key),
            version.into(),
            Some(UpdateChannel::Nightly),
            Channel::new(|_| Ok(())),
        )
        .await
        .err()
        .unwrap();
        assert!(
            error.contains("signature or signed version is invalid"),
            "{error}"
        );
        assert!(STAGED.get().unwrap().lock().await.is_none());
        registration_retired();
    }
}

#[tokio::test]
async fn https_new_preparation_discards_old_stage_before_manual_fallback() {
    let _state = TEST_STATE.lock().await;
    clear_stage().await;
    let app = test_app();
    let server = HttpsRelease::new().await;
    server.publish(VERSION, PAYLOAD, true);
    let prior = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(|_| Ok(())),
    )
    .await
    .unwrap();
    assert!(Path::new(&prior.path).is_file());
    server.publish(VERSION, PAYLOAD, false);
    let error = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(|_| Ok(())),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error, "This release needs a manual installation");
    assert!(!Path::new(&prior.path).exists());
    assert!(STAGED.get().unwrap().lock().await.is_none());
    registration_retired();
}

#[tokio::test]
async fn https_cancel_during_payload_body_retires_registration_and_temporary_stage() {
    let _state = TEST_STATE.lock().await;
    clear_stage().await;
    let app = test_app();
    let server = HttpsRelease::new().await;
    let payload_path = server.publish(VERSION, PAYLOAD, true);
    server
        .routes
        .lock()
        .unwrap()
        .get_mut(&payload_path)
        .unwrap()
        .pause_body = Some(Arc::new(tokio::sync::Notify::new()));
    let progress_started = Arc::new(tokio::sync::Notify::new());
    let observed = progress_started.clone();
    let preparation = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(move |_| {
            observed.notify_one();
            Ok(())
        }),
    );
    tokio::pin!(preparation);
    tokio::select! {
        result = &mut preparation => panic!("payload body must wait for cancellation: {}", result.err().unwrap()),
        ready = tokio::time::timeout(Duration::from_secs(5), progress_started.notified()) => ready.unwrap(),
    }
    super::super::cancel_update_download();
    assert_eq!(preparation.await.err().unwrap(), "Download canceled");
    assert!(STAGED.get().unwrap().lock().await.is_none());
    registration_retired();
}

#[tokio::test]
async fn prepared_install_errors_preserve_the_stage_without_restarting() {
    let _state = TEST_STATE.lock().await;
    clear_stage().await;
    let app = test_app();
    let server = HttpsRelease::new().await;
    server.publish(VERSION, PAYLOAD, true);
    let prepared = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(|_| Ok(())),
    )
    .await
    .unwrap();
    for (version, channel) in [
        ("99.0.0-nightly.42.2", Some(UpdateChannel::Nightly)),
        (VERSION, Some(UpdateChannel::Stable)),
        (VERSION, None),
    ] {
        assert_eq!(
            install_update(
                app.app.handle().clone(),
                version.into(),
                channel,
                Channel::new(|_| Ok(()))
            )
            .await
            .unwrap_err(),
            "Download and verify this update before installing"
        );
    }
    let download_guard = DOWNLOAD_LOCK.get().unwrap().lock().await;
    assert_eq!(
        install_update(
            app.app.handle().clone(),
            VERSION.into(),
            Some(UpdateChannel::Nightly),
            Channel::new(|_| Ok(()))
        )
        .await
        .unwrap_err(),
        "An update is already in progress"
    );
    let error = prepare_from(
        server.host(app.app.handle(), super::tests::FIXTURE_KEY),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(|_| Ok(())),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error, "An update is already in progress");
    drop(download_guard);
    std::fs::remove_file(&prepared.path).unwrap();
    assert_eq!(
        install_update(
            app.app.handle().clone(),
            VERSION.into(),
            Some(UpdateChannel::Nightly),
            Channel::new(|_| Ok(()))
        )
        .await
        .unwrap_err(),
        "The prepared update is unavailable. Download it again"
    );
    std::fs::write(&prepared.path, PAYLOAD).unwrap();
    let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let observed = events.clone();
    // This signed plaintext vector is not ELF, PE/MSI or a gzip archive. Every
    // native installer rejects its format before handoff; never use executable bytes.
    let error = install_update(
        app.app.handle().clone(),
        VERSION.into(),
        Some(UpdateChannel::Nightly),
        Channel::new(move |message| {
            let tauri::ipc::InvokeResponseBody::Json(json) = message else {
                panic!("JSON install progress")
            };
            observed
                .lock()
                .unwrap()
                .push(serde_json::from_str(&json).unwrap());
            Ok(())
        }),
    )
    .await
    .unwrap_err();
    assert!(
        error.starts_with("Could not install the update."),
        "{error}"
    );
    assert_eq!(events.lock().unwrap().as_slice(), ["installing"]);
    let stage = STAGED.get().unwrap().lock().await;
    let staged = stage.as_ref().unwrap();
    assert!(!staged.installed);
    assert!(staged.directory.path().is_dir());
    assert_eq!(std::fs::read(&prepared.path).unwrap(), PAYLOAD);
    staged.update.verify(PAYLOAD).unwrap();
    drop(stage);
    clear_stage().await;
    registration_retired();
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[path = "delta_https_tests.rs"]
mod delta_https_tests;

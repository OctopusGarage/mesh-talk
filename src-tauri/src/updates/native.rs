//! Native updater policy and installation; all paths and trust material come from the host.

use std::{path::Path, sync::OnceLock, time::Duration};

use serde::Serialize;
use tauri::ipc::Channel;
use tauri_plugin_updater::{Update, UpdaterExt};

use super::{
    API_RELEASES, CancelRegistration, DOWNLOAD_CANCEL, DOWNLOAD_LOCK, DownloadProgress,
    DownloadedUpdate, Release, ReleaseAsset, UpdateChannel, client, current_identity,
    download_and_verify, release_or_cancel, requested_release_from, selected_asset,
};

struct StagedUpdate {
    update: Update,
    channel: UpdateChannel,
    directory: tempfile::TempDir,
    installed: bool,
}

static STAGED: OnceLock<tokio::sync::Mutex<Option<StagedUpdate>>> = OnceLock::new();

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallProgress {
    Installing,
    Restarting,
}

pub(super) fn payload_name(tag: &str, os: &str, arch: &str) -> Option<String> {
    let package = super::package_name(tag, os, arch)?;
    let platform = package.strip_prefix("mesh-talk_")?.strip_suffix(".zip")?;
    let extension = match os {
        "macos" => "app.tar.gz",
        "windows" => "nsis.exe",
        "linux" => "AppImage",
        _ => return None,
    };
    Some(format!("mesh-talk-update_{platform}.{extension}"))
}

fn supported_install(
    os: &str,
    executable: &Path,
    has_key: bool,
    development: bool,
    appimage: bool,
) -> bool {
    if !has_key || development {
        return false;
    }
    match os {
        "macos" => {
            !executable.starts_with("/Volumes")
                && !executable
                    .components()
                    .any(|component| component.as_os_str() == "AppTranslocation")
                && executable.parent().is_some_and(|macos| {
                    macos.file_name().is_some_and(|name| name == "MacOS")
                        && macos.parent().is_some_and(|contents| {
                            contents.file_name().is_some_and(|name| name == "Contents")
                                && contents.parent().is_some_and(|bundle| {
                                    bundle
                                        .extension()
                                        .is_some_and(|extension| extension == "app")
                                })
                        })
                })
        }
        "windows" => executable
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")),
        "linux" => appimage,
        _ => false,
    }
}

fn public_key() -> &'static str {
    option_env!("MESH_TALK_UPDATER_PUBLIC_KEY")
        .unwrap_or("")
        .trim()
}

pub(super) fn available() -> bool {
    // Keep package-manager/MSI ownership intact; this release publishes NSIS and AppImage updater payloads.
    #[cfg(target_os = "windows")]
    if tauri::utils::platform::bundle_type() != Some(tauri::utils::config::BundleType::Nsis) {
        return false;
    }
    #[cfg(target_os = "linux")]
    if tauri::utils::platform::bundle_type() != Some(tauri::utils::config::BundleType::AppImage) {
        return false;
    }
    std::env::current_exe().is_ok_and(|executable| {
        supported_install(
            std::env::consts::OS,
            &executable,
            !public_key().is_empty(),
            cfg!(debug_assertions),
            std::env::var_os("APPIMAGE").is_some(),
        )
    })
}

pub(super) fn assets<'a>(
    release: &'a Release,
    os: &str,
    arch: &str,
) -> Option<(&'a ReleaseAsset, &'a ReleaseAsset)> {
    let name = payload_name(&release.tag_name, os, arch)?;
    let payload = selected_asset(release, &name)?;
    let signature = selected_asset(release, &format!("{name}.sig"))?;
    (signature.size <= 16 * 1024).then_some((payload, signature))
}

async fn signature_from(http: &reqwest::Client, asset: &ReleaseAsset) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    if asset.size > 16 * 1024 {
        return Err("The update signature is too large".into());
    }
    let mut response = http
        .get(&asset.browser_download_url)
        .send()
        .await
        .map_err(|_| "Could not download the update signature")?
        .error_for_status()
        .map_err(|_| "Could not download the update signature")?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read the update signature")?
    {
        if bytes.len() as u64 + chunk.len() as u64 > asset.size {
            return Err("The update signature has an unexpected size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != asset.size
        || asset.digest.as_deref()
            != Some(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))).as_str())
    {
        return Err("The update signature failed its integrity check".into());
    }
    String::from_utf8(bytes).map_err(|_| "The update signature is invalid".into())
}

fn signed_update<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    version: &str,
    payload: &ReleaseAsset,
    signature: &str,
    pubkey: &str,
) -> Result<Update, String> {
    let current = current_identity();
    app.updater_builder().pubkey(pubkey).endpoints(vec![API_RELEASES.parse().expect("fixed HTTPS endpoint")]).map_err(|_| "Could not prepare the updater")?
        // The Windows SDK invokes this before ShellExecuteW. Destructive
        // cleanup here would hide the app and retire its IPC even when launch
        // or UAC authorization fails. Successful handoff exits the process, at
        // which point Windows releases the instance mutex and native handles.
        .on_before_exit(|| {})
        .version_comparator(move |_, remote| super::ReleaseIdentity::parse(&format!("v{}", remote.version)).is_ok_and(|candidate| candidate > current))
        .build().map_err(|_| "Could not prepare the native installer")?
        .restore_update(serde_json::json!({ "version": version, "url": payload.browser_download_url, "signature": signature.trim() }))
        .map_err(|_| "The update metadata is invalid")?.ok_or_else(|| "This release is no longer an update".into())
}

#[tauri::command]
pub async fn prepare_update(
    app: tauri::AppHandle,
    version: String,
    update_channel: Option<UpdateChannel>,
    on_progress: Channel<DownloadProgress>,
) -> Result<DownloadedUpdate, String> {
    if !available() {
        return Err("Use the manual installer for this installation".into());
    }
    let _guard = DOWNLOAD_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .try_lock()
        .map_err(|_| "An update is already in progress")?;
    let (sender, cancel) = tokio::sync::watch::channel(false);
    *DOWNLOAD_CANCEL
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sender);
    let _registration = CancelRegistration;
    let channel = update_channel.unwrap_or_default();
    *STAGED
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await = None;
    let release = requested_release_from(
        &client(Duration::from_secs(20))?,
        API_RELEASES,
        &version,
        channel,
        &current_identity(),
        cancel.clone(),
    )
    .await?;
    let (payload, signature) = assets(&release, std::env::consts::OS, std::env::consts::ARCH)
        .ok_or("This release needs a manual installation")?;
    let signature = release_or_cancel(
        signature_from(&client(Duration::from_secs(30))?, signature),
        cancel.clone(),
    )
    .await?;
    let update = signed_update(&app, &version, payload, &signature, public_key())?;
    let directory = tempfile::tempdir().map_err(|_| "Could not prepare the update folder")?;
    let path = directory.path().join("payload");
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|_| "Could not create the update file")?;
    download_and_verify(payload, &mut file, &on_progress, cancel.clone()).await?;
    drop(file);
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| "Could not read the downloaded update")?;
    let verifier = update.clone();
    tokio::task::spawn_blocking(move || verifier.verify(&bytes)).await.map_err(|_| "Update verification was interrupted")?.map_err(|_| "The update signature or signed version is invalid. Use the release page to download manually")?;
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    let result = DownloadedUpdate {
        path: path.to_string_lossy().into_owned(),
        file_name: payload.name.clone(),
        installable: true,
    };
    *STAGED.get().expect("initialized stage").lock().await = Some(StagedUpdate {
        update,
        channel,
        directory,
        installed: false,
    });
    Ok(result)
}

#[tauri::command]
pub async fn install_update<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    version: String,
    update_channel: Option<UpdateChannel>,
    on_progress: Channel<InstallProgress>,
) -> Result<(), String> {
    let _guard = DOWNLOAD_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .try_lock()
        .map_err(|_| "An update is already in progress")?;
    let mut staged = STAGED
        .get_or_init(|| tokio::sync::Mutex::new(None))
        .lock()
        .await;
    let staged = staged
        .as_mut()
        .filter(|staged| {
            staged.update.version == version && staged.channel == update_channel.unwrap_or_default()
        })
        .ok_or("Download and verify this update before installing")?;
    if !staged.installed {
        let _ = on_progress.send(InstallProgress::Installing);
        let bytes = tokio::fs::read(staged.directory.path().join("payload"))
            .await
            .map_err(|_| "The prepared update is unavailable. Download it again")?;
        #[cfg(target_os = "windows")]
        {
            use tauri_plugin_window_state::AppHandleExt;
            app.save_window_state(tauri_plugin_window_state::StateFlags::all())
                .map_err(|_| "Could not save the window state before updating. Retry or use the manual installer")?;
        }
        let update = staged.update.clone();
        tokio::task::spawn_blocking(move || update.install(bytes)).await.map_err(|_| "Update installation was interrupted")?.map_err(|error| {
            log::warn!("native update installation failed: {error}");
            "Could not install the update. Authorization may have been canceled or the installation folder may be read-only. Retry or use the manual installer".to_string()
        })?;
        staged.installed = true;
    }
    let _ = on_progress.send(InstallProgress::Restarting);
    // Tauri relaunches macOS bundles through LaunchServices and emits Exit so
    // single-instance resources and window state are released before reopening.
    app.request_restart();
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
#[path = "windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_payload_names_preserve_platform_and_full_build_identity() {
        assert_eq!(
            payload_name("v0.4.4-nightly.42.1", "macos", "aarch64").as_deref(),
            Some("mesh-talk-update_v0.4.4-nightly.42.1_macos_arm64.app.tar.gz")
        );
        assert_eq!(
            payload_name("v0.4.4", "windows", "x86_64").as_deref(),
            Some("mesh-talk-update_v0.4.4_windows_x86_64.nsis.exe")
        );
        assert_eq!(
            payload_name("v0.4.4", "linux", "aarch64").as_deref(),
            Some("mesh-talk-update_v0.4.4_linux_aarch64.AppImage")
        );
        assert!(payload_name("v0.4.4", "linux", "armv7").is_none());
    }

    #[test]
    fn automatic_install_requires_a_key_and_an_installed_supported_package() {
        let mac = std::path::Path::new("/Applications/mesh-talk.app/Contents/MacOS/mesh-talk");
        assert!(supported_install("macos", mac, true, false, false));
        assert!(!supported_install("macos", mac, false, false, false));
        assert!(!supported_install("macos", mac, true, true, false));
        assert!(!supported_install(
            "macos",
            std::path::Path::new(
                "/private/var/folders/AppTranslocation/fixture/d/mesh-talk.app/Contents/MacOS/mesh-talk"
            ),
            true,
            false,
            false
        ));
        assert!(!supported_install(
            "macos",
            std::path::Path::new("/Volumes/Mesh/mesh-talk.app/Contents/MacOS/mesh-talk"),
            true,
            false,
            false
        ));
        assert!(!supported_install(
            "macos",
            std::path::Path::new("/tmp/mesh-talk"),
            true,
            false,
            false
        ));
        assert!(!supported_install("linux", mac, true, false, false));
        assert!(supported_install(
            "linux",
            std::path::Path::new("/home/alice/mesh-talk.AppImage"),
            true,
            false,
            true
        ));
    }
    // Public verification vectors only. The throwaway private key is not shipped.
    const FIXTURE_KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDEyMzNGRUE1REYzNkIwNEIKUldSTHNEYmZwZjR6RW9hWnNvS2l6THA5WXoyRFhrdk1vOWhaSXVkTDMyeHk4U0ZtRGJMSFVLc3kK";
    const FIXTURE_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSTHNEYmZwZjR6RXU1aWQxNmxaLzBvV3FveHFiY2cwd21KU3FhQXh1cUhYUWZwOHpnMmxleW9GNTdQM1h4N3BsZDZJblNuTEhLUmwrVVVtWTc0QkM2b3lDWDNkY1ZvRGc4PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyODI3NTAzCWZpbGU6dXBkYXRlci10ZXN0LXBheWxvYWQJdmVyc2lvbjo5OS4wLjAtbmlnaHRseS40Mi4xCmF1ZnNZSnkxejR3RDZzdUk5V3dmeDVyVWpGdDJDanNPTzFsOFA5TGhpekI5bjFWQm1DMHZRSWlaem5HNFVwek9NMTYzNHVZdE9RajZVMFRpWUQ4Y0NBPT0K";

    #[test]
    fn signed_updates_reject_tampering_relabelled_versions_and_untrusted_keys() {
        let mut context = tauri::test::mock_context(tauri::test::noop_assets());
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../../tauri.conf.json")).unwrap();
        context
            .config_mut()
            .plugins
            .0
            .insert("updater".into(), config["plugins"]["updater"].clone());
        let app = tauri::test::mock_builder()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .build(context)
            .unwrap();
        let payload = ReleaseAsset { name: "fixture.app.tar.gz".into(), browser_download_url: "https://github.com/OctopusGarage/mesh-talk/releases/download/v99.0.0-nightly.42.1/fixture.app.tar.gz".into(), digest: None, size: 26, state: "uploaded".into() };
        let update = signed_update(
            app.handle(),
            "99.0.0-nightly.42.1",
            &payload,
            FIXTURE_SIGNATURE,
            FIXTURE_KEY,
        )
        .unwrap();
        update.verify(b"updater regression payload").unwrap();
        assert!(update.verify(b"tampered payload").is_err());
        // The same native base version does not authorize another Nightly identity.
        let relabelled = signed_update(
            app.handle(),
            "99.0.0-nightly.42.2",
            &payload,
            FIXTURE_SIGNATURE,
            FIXTURE_KEY,
        )
        .unwrap();
        assert!(relabelled.verify(b"updater regression payload").is_err());
        let untrusted = signed_update(
            app.handle(),
            "99.0.0-nightly.42.1",
            &payload,
            FIXTURE_SIGNATURE,
            "invalid-public-key",
        )
        .unwrap();
        assert!(untrusted.verify(b"updater regression payload").is_err());
        assert!(
            signed_update(
                app.handle(),
                "0.0.1",
                &payload,
                FIXTURE_SIGNATURE,
                FIXTURE_KEY
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn signature_download_is_bounded_and_digest_verified() {
        use sha2::{Digest, Sha256};
        async fn serve(body: &'static str) -> String {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 4096];
                let _ = stream.read(&mut buffer).await;
                let _ = stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            });
            url
        }
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut asset = ReleaseAsset {
            name: "fixture.sig".into(),
            browser_download_url: serve("signature").await,
            digest: Some(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(b"signature"))
            )),
            size: 9,
            state: "uploaded".into(),
        };
        assert_eq!(signature_from(&http, &asset).await.unwrap(), "signature");
        asset.browser_download_url = serve("corrupted").await;
        assert!(signature_from(&http, &asset).await.is_err());
        asset.browser_download_url = serve("oversized signature").await;
        assert!(signature_from(&http, &asset).await.is_err());
        asset.size = 16 * 1024 + 1;
        assert!(signature_from(&http, &asset).await.is_err());
    }
    #[test]
    fn registered_install_rejects_unprepared_renderer_requests() {
        let app = tauri::test::mock_builder()
            .invoke_handler(tauri::generate_handler![install_update])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        let result = tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: "install_update".into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(
                    serde_json::json!({ "version": "99.0.0", "updateChannel": "stable", "onProgress": "__CHANNEL__:2", "path": "/arbitrary/installer" }),
                ),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        );
        assert_eq!(
            result.unwrap_err(),
            serde_json::json!("Download and verify this update before installing")
        );
    }
}

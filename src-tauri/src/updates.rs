//! Explicit, manual release downloads. The renderer cannot choose a URL or local path:
//! each request rechecks GitHub's latest published release and the exact asset for
//! this build's OS, architecture, and edition before writing to Downloads.

use std::{path::PathBuf, sync::OnceLock, time::Duration};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, ipc::Channel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const API_URL: &str = "https://api.github.com/repos/OctopusGarage/mesh-talk/releases/latest";
const RELEASES_URL: &str = "https://github.com/OctopusGarage/mesh-talk/releases";
const MAX_RELEASE_JSON: u64 = 2 * 1024 * 1024;
const MAX_ASSET_SIZE: u64 = 1024 * 1024 * 1024;
static DOWNLOAD_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
static DOWNLOAD_CANCEL: OnceLock<std::sync::Mutex<Option<tokio::sync::watch::Sender<bool>>>> =
    OnceLock::new();

struct CancelRegistration;

impl Drop for CancelRegistration {
    fn drop(&mut self) {
        if let Some(lock) = DOWNLOAD_CANCEL.get() {
            *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        }
    }
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    body: Option<String>,
    draft: bool,
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
    state: String,
}

#[derive(Serialize)]
pub struct UpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub is_newer: bool,
    pub release_url: String,
    pub notes: String,
    pub package_name: Option<String>,
    pub package_size: Option<u64>,
    pub edition: &'static str,
}

#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Serialize)]
pub struct DownloadedUpdate {
    pub path: String,
    pub file_name: String,
}

fn edition() -> &'static str {
    if option_env!("MESH_TALK_VARIANT") == Some("lite") {
        "lite"
    } else {
        "default"
    }
}

fn package_name(tag: &str, os: &str, arch: &str, edition: &str) -> Option<String> {
    let os = match os {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match arch {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        _ => return None,
    };
    let arch = if os == "linux" && arch == "arm64" {
        "aarch64"
    } else {
        arch
    };
    let suffix = if edition == "lite" { "_lite" } else { "" };
    Some(format!("mesh-talk_{tag}_{os}_{arch}{suffix}.zip"))
}

fn release_version(release: &Release) -> Result<semver::Version, String> {
    if release.draft || release.prerelease {
        return Err("The latest release is not a published stable release".into());
    }
    let version = release
        .tag_name
        .strip_prefix('v')
        .ok_or("The release tag is invalid")?;
    let version = semver::Version::parse(version).map_err(|_| "The release tag is invalid")?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err("The release tag is not a stable version".into());
    }
    Ok(version)
}

fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("package version is semver")
}

fn selected_asset<'a>(release: &'a Release, name: &str) -> Option<&'a ReleaseAsset> {
    let asset = release.assets.iter().find(|asset| asset.name == name)?;
    let expected_url = format!(
        "{RELEASES_URL}/download/{}/{}",
        release.tag_name, asset.name
    );
    if asset.state != "uploaded"
        || asset.browser_download_url != expected_url
        || !(1..=MAX_ASSET_SIZE).contains(&asset.size)
    {
        return None;
    }
    let digest = asset.digest.as_deref()?.strip_prefix("sha256:")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(asset)
}

fn describe_release(
    release: &Release,
    os: &str,
    arch: &str,
    edition: &'static str,
) -> Result<UpdateCheck, String> {
    let latest = release_version(release)?;
    let current = current_version();
    let name = package_name(&release.tag_name, os, arch, edition);
    let asset = name
        .as_deref()
        .and_then(|name| selected_asset(release, name));
    let newer = latest > current;
    Ok(UpdateCheck {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        is_newer: newer,
        release_url: format!("{RELEASES_URL}/tag/{}", release.tag_name),
        notes: release
            .body
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(1200)
            .collect(),
        package_name: if newer {
            asset.map(|asset| asset.name.clone())
        } else {
            None
        },
        package_size: if newer {
            asset.map(|asset| asset.size)
        } else {
            None
        },
        edition,
    })
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("mesh-talk/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not prepare the update connection".into())
}

async fn latest_release() -> Result<Release, String> {
    latest_release_from(&client(Duration::from_secs(20))?, API_URL).await
}

async fn latest_release_from(client: &reqwest::Client, url: &str) -> Result<Release, String> {
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Could not reach GitHub. Check your connection and try again")?;
    let mut response = response.error_for_status().map_err(|error| {
        if error.status() == Some(reqwest::StatusCode::FORBIDDEN) {
            "GitHub is temporarily limiting update checks. Try again later".to_string()
        } else {
            "Could not read the latest published release".to_string()
        }
    })?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RELEASE_JSON)
    {
        return Err("The release response is too large".into());
    }
    if response.url().as_str() != url {
        return Err("The release response came from an unexpected address".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Could not read the release response")?
    {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_RELEASE_JSON {
            return Err("The release response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "The release response is invalid".into())
}

async fn release_or_cancel<T, F>(
    release: F,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<T, String>
where
    F: std::future::Future<Output = Result<T, String>>,
{
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    tokio::select! {
        result = release => result,
        _ = cancel.changed() => Err("Download canceled".into()),
    }
}

#[tauri::command]
pub async fn check_for_update() -> Result<UpdateCheck, String> {
    let release = latest_release().await?;
    describe_release(
        &release,
        std::env::consts::OS,
        std::env::consts::ARCH,
        edition(),
    )
}

fn destination(downloads: &std::path::Path, name: &str) -> PathBuf {
    let base = name.strip_suffix(".zip").expect("validated asset name");
    for index in 0..100 {
        let candidate = if index == 0 {
            downloads.join(name)
        } else {
            downloads.join(format!("{base} ({index}).zip"))
        };
        if matches!(
            std::fs::symlink_metadata(&candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound
        ) {
            return candidate;
        }
    }
    downloads.join(format!("{base} ({}).zip", std::process::id()))
}

#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    version: String,
    on_progress: Channel<DownloadProgress>,
) -> Result<DownloadedUpdate, String> {
    let _guard = DOWNLOAD_LOCK
        .get_or_init(|| tokio::sync::Mutex::new(()))
        .try_lock()
        .map_err(|_| "An update download is already in progress")?;
    let (cancel_sender, cancel_receiver) = tokio::sync::watch::channel(false);
    *DOWNLOAD_CANCEL
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(cancel_sender);
    let _cancel_registration = CancelRegistration;
    let release = release_or_cancel(latest_release(), cancel_receiver.clone()).await?;
    let latest = release_version(&release)?;
    if latest.to_string() != version || latest <= current_version() {
        return Err("The available release changed. Check for updates again".into());
    }
    let name = package_name(
        &release.tag_name,
        std::env::consts::OS,
        std::env::consts::ARCH,
        edition(),
    )
    .ok_or("There is no installer for this platform")?;
    let asset = selected_asset(&release, &name)
        .ok_or("The verified installer for this platform is not available")?;
    let downloads = app
        .path()
        .download_dir()
        .ok()
        .or_else(|| crate::user_home_dir().map(|home| home.join("Downloads")))
        .ok_or("Could not find the Downloads folder")?;
    tokio::fs::create_dir_all(&downloads)
        .await
        .map_err(|_| "Could not create the Downloads folder")?;
    if let Some(existing) =
        find_verified_existing(&downloads, &name, asset, &cancel_receiver).await?
    {
        return Ok(DownloadedUpdate {
            path: existing.to_string_lossy().into_owned(),
            file_name: existing
                .file_name()
                .expect("verified package has a name")
                .to_string_lossy()
                .into_owned(),
        });
    }
    let final_path = destination(&downloads, &name);
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "Could not prepare the temporary download file")?
        .as_nanos();
    let temporary = downloads.join(format!(".{name}.{}.{}.part", std::process::id(), nonce));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .await
        .map_err(|_| "Could not create the temporary download file")?;

    let publish_cancel = cancel_receiver.clone();
    let result = download_and_verify(asset, &mut file, &on_progress, cancel_receiver).await;
    drop(file);
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    if let Err(error) = publish_verified_or_cancel(&temporary, &final_path, &publish_cancel).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    Ok(DownloadedUpdate {
        path: final_path.to_string_lossy().into_owned(),
        file_name: final_path
            .file_name()
            .expect("selected download path has a name")
            .to_string_lossy()
            .into_owned(),
    })
}

async fn find_verified_existing(
    downloads: &std::path::Path,
    name: &str,
    asset: &ReleaseAsset,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<Option<PathBuf>, String> {
    let existing = downloads.join(name);
    if verified_existing(&existing, asset, cancel).await? {
        return Ok(Some(existing));
    }
    let base = name.strip_suffix(".zip").expect("validated asset name");
    let prefix = format!("{base} (");
    let mut entries = tokio::fs::read_dir(downloads)
        .await
        .map_err(|_| "Could not inspect the Downloads folder")?;
    let mut numbered = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|_| "Could not inspect the Downloads folder")?
    {
        let file_name = entry.file_name();
        let Some(digits) = file_name
            .to_str()
            .and_then(|file_name| file_name.strip_prefix(&prefix))
            .and_then(|remainder| remainder.strip_suffix(").zip"))
        else {
            continue;
        };
        if !digits.is_empty() && digits.bytes().all(|digit| digit.is_ascii_digit()) {
            numbered.push(entry.path());
        }
    }
    numbered.sort();
    for path in numbered {
        if verified_existing(&path, asset, cancel).await? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

async fn publish_verified_or_cancel(
    temporary: &std::path::Path,
    final_path: &std::path::Path,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    publish_verified(temporary, final_path)
        .await
        .map_err(|error| {
            String::from(if error.kind() == std::io::ErrorKind::AlreadyExists {
                "The download destination changed. Try again"
            } else {
                "Could not move the verified update into Downloads"
            })
        })?;
    if *cancel.borrow() {
        tokio::fs::remove_file(final_path)
            .await
            .map_err(|_| "Could not remove the canceled update")?;
        return Err("Download canceled".into());
    }
    Ok(())
}

async fn publish_verified(
    temporary: &std::path::Path,
    final_path: &std::path::Path,
) -> std::io::Result<()> {
    // Both paths are in Downloads. A hard link atomically refuses to replace an
    // existing name; an exists() check followed by rename() cannot do that.
    tokio::fs::hard_link(temporary, final_path).await?;
    tokio::fs::remove_file(temporary).await
}

async fn verified_existing(
    path: &std::path::Path,
    asset: &ReleaseAsset,
    cancel: &tokio::sync::watch::Receiver<bool>,
) -> Result<bool, String> {
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    let Ok(metadata) = tokio::fs::symlink_metadata(path).await else {
        return Ok(false);
    };
    if !metadata.file_type().is_file() || metadata.len() != asset.size {
        return Ok(false);
    }
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return Ok(false);
    };
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 8 * 1024];
    loop {
        if *cancel.borrow() {
            return Err("Download canceled".into());
        }
        let Ok(read) = file.read(&mut buffer).await else {
            return Ok(false);
        };
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    let Some(expected) = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
    else {
        return Ok(false);
    };
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    Ok(hex::encode(hash.finalize()) == expected.to_ascii_lowercase())
}

#[tauri::command]
pub fn cancel_update_download() {
    if let Some(lock) = DOWNLOAD_CANCEL.get()
        && let Some(sender) = lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
    {
        let _ = sender.send(true);
    }
}

async fn download_and_verify(
    asset: &ReleaseAsset,
    file: &mut tokio::fs::File,
    on_progress: &Channel<DownloadProgress>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    download_and_verify_with_client(
        &client(Duration::from_secs(20 * 60))?,
        asset,
        file,
        on_progress,
        cancel,
    )
    .await
}

async fn download_and_verify_with_client(
    client: &reqwest::Client,
    asset: &ReleaseAsset,
    file: &mut tokio::fs::File,
    on_progress: &Channel<DownloadProgress>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    if *cancel.borrow() {
        return Err("Download canceled".into());
    }
    let request = client.get(&asset.browser_download_url).send();
    let mut response = tokio::select! {
        result = request => result.map_err(|_| "Could not start the download")?,
        _ = cancel.changed() => return Err("Download canceled".into()),
    }
    .error_for_status()
    .map_err(|_| "GitHub could not provide the update package")?;
    if response
        .content_length()
        .is_some_and(|length| length != asset.size)
    {
        return Err("The download size does not match the published release".into());
    }
    let mut hash = Sha256::new();
    let mut downloaded = 0u64;
    let mut last_reported = std::time::Instant::now();
    let _ = on_progress.send(DownloadProgress {
        downloaded: 0,
        total: asset.size,
    });
    loop {
        if *cancel.borrow() {
            return Err("Download canceled".into());
        }
        let chunk = tokio::select! {
            result = response.chunk() => result.map_err(|_| "The download was interrupted")?,
            _ = cancel.changed() => return Err("Download canceled".into()),
        };
        let Some(chunk) = chunk else { break };
        downloaded += chunk.len() as u64;
        if downloaded > asset.size {
            return Err("The download exceeded the published size".into());
        }
        file.write_all(&chunk)
            .await
            .map_err(|_| "Could not write the update to Downloads")?;
        hash.update(&chunk);
        if last_reported.elapsed() >= Duration::from_millis(200) || downloaded == asset.size {
            let _ = on_progress.send(DownloadProgress {
                downloaded,
                total: asset.size,
            });
            last_reported = std::time::Instant::now();
        }
    }
    file.flush()
        .await
        .map_err(|_| "Could not finish writing the update")?;
    file.sync_all()
        .await
        .map_err(|_| "Could not save the update to disk")?;
    if downloaded != asset.size {
        return Err("The download is incomplete".into());
    }
    let expected = asset
        .digest
        .as_deref()
        .and_then(|value| value.strip_prefix("sha256:"))
        .expect("selected asset has a SHA-256 digest");
    if hex::encode(hash.finalize()) != expected.to_ascii_lowercase() {
        return Err("The downloaded package failed its SHA-256 check".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next_release_tag() -> String {
        let installed = current_version();
        format!(
            "v{}",
            semver::Version::new(installed.major, installed.minor, installed.patch + 1)
        )
    }

    #[test]
    fn package_names_match_all_release_targets_and_editions() {
        assert_eq!(
            package_name("v0.4.0", "macos", "aarch64", "default").unwrap(),
            "mesh-talk_v0.4.0_macos_arm64.zip"
        );
        assert_eq!(
            package_name("v0.4.0", "windows", "x86_64", "lite").unwrap(),
            "mesh-talk_v0.4.0_windows_x86_64_lite.zip"
        );
        assert_eq!(
            package_name("v0.4.0", "linux", "aarch64", "default").unwrap(),
            "mesh-talk_v0.4.0_linux_aarch64.zip"
        );
        assert!(package_name("v0.4.0", "freebsd", "x86_64", "default").is_none());
        assert!(package_name("v0.4.0", "linux", "riscv64", "default").is_none());
    }

    #[test]
    fn rejects_unverified_or_mismatched_release_assets() {
        let tag = next_release_tag();
        let name = format!("mesh-talk_{tag}_macos_arm64.zip");
        let mut release: Release = serde_json::from_value(serde_json::json!({
            "tag_name": tag, "body": "Changes", "draft": false, "prerelease": false,
            "assets": [{
                "name": name, "state": "uploaded", "size": 42,
                "browser_download_url": format!("{RELEASES_URL}/download/{tag}/{name}"),
                "digest": format!("sha256:{}", "a".repeat(64))
            }]
        }))
        .unwrap();
        assert!(
            describe_release(&release, "macos", "aarch64", "default")
                .unwrap()
                .package_name
                .is_some()
        );
        release.assets[0].digest = None;
        assert!(
            describe_release(&release, "macos", "aarch64", "default")
                .unwrap()
                .package_name
                .is_none()
        );
        release.assets[0].digest = Some(format!("sha256:{}", "a".repeat(64)));
        release.assets[0].browser_download_url = "https://example.com/fake.zip".into();
        assert!(
            describe_release(&release, "macos", "aarch64", "default")
                .unwrap()
                .package_name
                .is_none()
        );
        release.prerelease = true;
        assert!(describe_release(&release, "macos", "aarch64", "default").is_err());
    }

    #[tokio::test]
    async fn an_existing_package_is_reused_only_when_its_bytes_match() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("package.zip");
        tokio::fs::write(&path, b"package").await.unwrap();
        let asset = ReleaseAsset {
            name: "package.zip".into(),
            browser_download_url: String::new(),
            digest: Some(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(b"package"))
            )),
            size: 7,
            state: "uploaded".into(),
        };
        let (_, cancel) = tokio::sync::watch::channel(false);
        assert!(verified_existing(&path, &asset, &cancel).await.unwrap());
        tokio::fs::write(&path, b"tampered").await.unwrap();
        assert!(!verified_existing(&path, &asset, &cancel).await.unwrap());
    }

    #[tokio::test]
    async fn numbered_verified_package_is_reused_when_canonical_name_is_occupied() {
        let directory = tempfile::tempdir().unwrap();
        tokio::fs::write(directory.path().join("package.zip"), b"invalid")
            .await
            .unwrap();
        let numbered = directory.path().join("package (1).zip");
        tokio::fs::write(&numbered, b"package").await.unwrap();
        let asset = ReleaseAsset {
            name: "package.zip".into(),
            browser_download_url: String::new(),
            digest: Some(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(b"package"))
            )),
            size: 7,
            state: "uploaded".into(),
        };
        let (_, cancel) = tokio::sync::watch::channel(false);

        assert_eq!(
            find_verified_existing(directory.path(), "package.zip", &asset, &cancel)
                .await
                .unwrap(),
            Some(numbered)
        );
    }

    #[tokio::test]
    async fn cancellation_before_publish_keeps_verified_bytes_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("verified.part");
        let final_path = directory.path().join("update.zip");
        tokio::fs::write(&temporary, b"verified package")
            .await
            .unwrap();
        let (sender, cancel) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();

        assert_eq!(
            publish_verified_or_cancel(&temporary, &final_path, &cancel)
                .await
                .unwrap_err(),
            "Download canceled"
        );
        assert!(!final_path.exists());
        assert_eq!(
            tokio::fs::read(&temporary).await.unwrap(),
            b"verified package"
        );
    }

    #[tokio::test]
    async fn cancel_prevents_reusing_an_existing_verified_package() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("package.zip");
        tokio::fs::write(&path, b"package").await.unwrap();
        let asset = ReleaseAsset {
            name: "package.zip".into(),
            browser_download_url: String::new(),
            digest: Some(format!(
                "sha256:{}",
                hex::encode(Sha256::digest(b"package"))
            )),
            size: 7,
            state: "uploaded".into(),
        };
        let (sender, cancel) = tokio::sync::watch::channel(false);
        sender.send(true).unwrap();

        assert_eq!(
            verified_existing(&path, &asset, &cancel).await.unwrap_err(),
            "Download canceled"
        );
    }

    #[tokio::test]
    async fn cancel_interrupts_a_pending_release_recheck() {
        let (sender, cancel) = tokio::sync::watch::channel(false);
        let (started, ready) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(release_or_cancel(
            async move {
                let _ = started.send(());
                std::future::pending::<Result<(), String>>().await
            },
            cancel,
        ));
        ready.await.unwrap();
        sender.send(true).unwrap();

        assert_eq!(
            tokio::time::timeout(Duration::from_millis(200), waiting)
                .await
                .expect("cancellation should interrupt the pending release check")
                .unwrap()
                .unwrap_err(),
            "Download canceled"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publishing_never_replaces_an_existing_directory_entry() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("verified.part");
        let final_path = directory.path().join("update.zip");
        tokio::fs::write(&temporary, b"verified package")
            .await
            .unwrap();
        std::os::unix::fs::symlink("other-file.zip", &final_path).unwrap();

        assert_eq!(
            publish_verified(&temporary, &final_path)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            tokio::fs::read_link(&final_path).await.unwrap(),
            std::path::Path::new("other-file.zip")
        );
        assert_eq!(
            tokio::fs::read(&temporary).await.unwrap(),
            b"verified package"
        );
    }

    #[tokio::test]
    async fn publishing_moves_verified_bytes_and_removes_the_temporary_name() {
        let directory = tempfile::tempdir().unwrap();
        let temporary = directory.path().join("verified.part");
        let final_path = directory.path().join("update.zip");
        tokio::fs::write(&temporary, b"verified package")
            .await
            .unwrap();

        publish_verified(&temporary, &final_path).await.unwrap();

        assert_eq!(
            tokio::fs::read(&final_path).await.unwrap(),
            b"verified package"
        );
        assert!(!temporary.exists());
    }

    #[cfg(unix)]
    #[test]
    fn destination_skips_a_broken_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let name = "mesh-talk_v0.4.0_macos_arm64.zip";
        std::os::unix::fs::symlink("missing.zip", directory.path().join(name)).unwrap();

        assert_eq!(
            destination(directory.path(), name),
            directory
                .path()
                .join("mesh-talk_v0.4.0_macos_arm64 (1).zip")
        );
    }

    #[test]
    fn release_description_distinguishes_current_and_unavailable_packages() {
        let installed = current_version();
        let mut release: Release = serde_json::from_value(serde_json::json!({
            "tag_name": format!("v{installed}"), "body": "x".repeat(1300),
            "draft": false, "prerelease": false, "assets": []
        }))
        .unwrap();
        let current = describe_release(&release, "macos", "aarch64", "default").unwrap();
        assert!(!current.is_newer);
        assert_eq!(current.current_version, installed.to_string());
        assert_eq!(current.latest_version, installed.to_string());
        assert_eq!(current.notes.chars().count(), 1200);
        assert!(current.package_name.is_none());

        release.tag_name = next_release_tag();
        let unavailable = describe_release(&release, "linux", "x86_64", "lite").unwrap();
        assert!(unavailable.is_newer);
        assert!(unavailable.package_name.is_none());
        assert_eq!(unavailable.edition, "lite");

        release.draft = true;
        assert!(release_version(&release).is_err());
        release.draft = false;
        release.tag_name = "0.4.0".into();
        assert!(release_version(&release).is_err());
        release.tag_name = "v0.4.0-beta.1".into();
        assert!(release_version(&release).is_err());
    }

    #[tokio::test]
    async fn loopback_release_lookup_handles_success_limits_and_bad_responses() {
        async fn serve(status: &str, body: &'static str, declared_size: usize) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/latest", listener.local_addr().unwrap());
            let status = status.to_owned();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await.unwrap();
                let headers = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {declared_size}\r\nConnection: close\r\n\r\n"
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(body.as_bytes()).await.unwrap();
            });
            url
        }

        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let valid =
            r#"{"tag_name":"v0.4.0","body":"Notes","draft":false,"prerelease":false,"assets":[]}"#;
        let url = serve("200 OK", valid, valid.len()).await;
        let release = latest_release_from(&client, &url).await.unwrap();
        assert_eq!(release.tag_name, "v0.4.0");

        let url = serve("403 Forbidden", "", 0).await;
        assert_eq!(
            latest_release_from(&client, &url).await.err().unwrap(),
            "GitHub is temporarily limiting update checks. Try again later"
        );
        let url = serve("200 OK", "not JSON", 8).await;
        assert_eq!(
            latest_release_from(&client, &url).await.err().unwrap(),
            "The release response is invalid"
        );
        let url = serve("200 OK", "", (MAX_RELEASE_JSON + 1) as usize).await;
        assert_eq!(
            latest_release_from(&client, &url).await.err().unwrap(),
            "The release response is too large"
        );
    }

    #[tokio::test]
    async fn mismatched_existing_files_are_never_reused_or_overwritten() {
        let directory = tempfile::tempdir().unwrap();
        let name = "package.zip";
        let canonical = directory.path().join(name);
        tokio::fs::write(&canonical, b"wrong bytes").await.unwrap();
        tokio::fs::write(directory.path().join("package (2).zip"), b"wrong too")
            .await
            .unwrap();
        let asset = ReleaseAsset {
            name: name.into(),
            browser_download_url: String::new(),
            digest: Some(format!("sha256:{}", hex::encode(Sha256::digest(b"valid")))),
            size: 5,
            state: "uploaded".into(),
        };
        let (_, cancel) = tokio::sync::watch::channel(false);
        assert_eq!(
            find_verified_existing(directory.path(), name, &asset, &cancel)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            destination(directory.path(), name),
            directory.path().join("package (1).zip")
        );
        assert_eq!(tokio::fs::read(canonical).await.unwrap(), b"wrong bytes");
    }

    #[tokio::test]
    async fn loopback_download_checks_length_digest_and_reports_progress() {
        use std::sync::{Arc, Mutex};

        async fn serve(bytes: &'static [u8]) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/package.zip", listener.local_addr().unwrap());
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let _ = stream.read(&mut request).await.unwrap();
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    bytes.len()
                );
                stream.write_all(headers.as_bytes()).await.unwrap();
                stream.write_all(bytes).await.unwrap();
            });
            url
        }

        let directory = tempfile::tempdir().unwrap();
        let loopback_client = reqwest::Client::builder().no_proxy().build().unwrap();
        let digest = format!(
            "sha256:{}",
            hex::encode(Sha256::digest(b"verified package"))
        );
        let progress = Arc::new(Mutex::new(Vec::new()));
        let reports = progress.clone();
        let channel = Channel::new(move |message| {
            reports.lock().unwrap().push(message);
            Ok(())
        });
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let mut asset = ReleaseAsset {
            name: "package.zip".into(),
            browser_download_url: serve(b"verified package").await,
            digest: Some(digest),
            size: 16,
            state: "uploaded".into(),
        };
        let mut file = tokio::fs::File::create(directory.path().join("valid.part"))
            .await
            .unwrap();
        download_and_verify_with_client(
            &loopback_client,
            &asset,
            &mut file,
            &channel,
            cancel.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            tokio::fs::read(directory.path().join("valid.part"))
                .await
                .unwrap(),
            b"verified package"
        );
        assert!(progress.lock().unwrap().len() >= 2);

        asset.browser_download_url = serve(b"short").await;
        let mut file = tokio::fs::File::create(directory.path().join("short.part"))
            .await
            .unwrap();
        assert_eq!(
            download_and_verify_with_client(
                &loopback_client,
                &asset,
                &mut file,
                &channel,
                cancel.clone()
            )
            .await
            .unwrap_err(),
            "The download size does not match the published release"
        );

        asset.browser_download_url = serve(b"tampered package").await;
        let mut file = tokio::fs::File::create(directory.path().join("tampered.part"))
            .await
            .unwrap();
        assert_eq!(
            download_and_verify_with_client(&loopback_client, &asset, &mut file, &channel, cancel)
                .await
                .unwrap_err(),
            "The downloaded package failed its SHA-256 check"
        );
    }
}

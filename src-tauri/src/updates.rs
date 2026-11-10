//! Explicit release downloads and signed native update handoff. The renderer cannot choose a URL or local path:
//! each request rechecks the exact published release and matching asset for
//! this build's OS and architecture before writing to Downloads.

mod channel;
mod delta_catalog;
pub(crate) mod delta_codec;
pub mod native;

use std::{path::PathBuf, sync::OnceLock, time::Duration};

use channel::ReleaseIdentity;
pub use channel::UpdateChannel;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{Manager, ipc::Channel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const API_RELEASES: &str = "https://api.github.com/repos/OctopusGarage/mesh-talk/releases";
const API_URL: &str = "https://api.github.com/repos/OctopusGarage/mesh-talk/releases/latest";
const MAX_RELEASE_PAGES: u32 = 3;
// Each release contains the complete six-platform archive matrix,
// signatures and relay assets. Small pages fit the bounded JSON response budget.
const RELEASE_PAGE_SIZE: usize = 10;
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
    pub automatic_install: bool,
    pub current_version: String,
    pub latest_version: String,
    pub is_newer: bool,
    pub release_url: String,
    pub notes: String,
    pub package_name: Option<String>,
    pub package_size: Option<u64>,
    // Retained for IPC compatibility; desktop packages have one canonical edition.
    pub edition: &'static str,
    pub channel: UpdateChannel,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadPhase {
    Preparing,
    Fallback,
}

#[derive(Clone, Serialize)]
pub struct DownloadProgress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<DownloadPhase>,
    pub downloaded: u64,
    pub total: u64,
}

#[derive(Serialize)]
pub struct DownloadedUpdate {
    pub installable: bool,
    pub path: String,
    pub file_name: String,
}

fn package_name(tag: &str, os: &str, arch: &str) -> Option<String> {
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
    Some(format!("mesh-talk_{tag}_{os}_{arch}.zip"))
}

fn release_version(release: &Release, channel: UpdateChannel) -> Result<ReleaseIdentity, String> {
    ReleaseIdentity::published(
        &release.tag_name,
        release.prerelease,
        release.draft,
        channel,
    )
}

pub(crate) fn current_build_version() -> &'static str {
    env!("MESH_TALK_BUILD_VERSION")
}

fn current_identity() -> ReleaseIdentity {
    ReleaseIdentity::parse(env!("MESH_TALK_BUILD_TAG")).expect("validated build identity")
}

#[cfg(test)]
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
    channel: UpdateChannel,
) -> Result<UpdateCheck, String> {
    let latest = release_version(release, channel)?;
    let current = current_identity();
    let name = package_name(&release.tag_name, os, arch);
    let asset = name
        .as_deref()
        .and_then(|name| selected_asset(release, name));
    let newer = latest > current;
    let automatic_install =
        newer && native::available() && native::assets(release, os, arch).is_some();
    let asset = if automatic_install {
        native::assets(release, os, arch).map(|(payload, _)| payload)
    } else {
        asset
    };
    Ok(UpdateCheck {
        automatic_install,
        current_version: current_build_version().to_string(),
        latest_version: release.tag_name[1..].to_string(),
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
        edition: "default",
        channel,
    })
}

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("mesh-talk/{}", current_build_version()))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .map_err(|_| "Could not prepare the update connection".into())
}

/// Stable keeps GitHub's full-release endpoint. Nightly explicitly discovers
/// recognized immutable builds and newer stable versions; other prereleases
/// cannot enter either feed. Discover recent Nightlies with a bounded scan,
/// while looking up Stable separately so old stable releases remain eligible.
async fn latest_release(channel: UpdateChannel) -> Result<Release, String> {
    let client = client(Duration::from_secs(20))?;
    if channel == UpdateChannel::Stable {
        return latest_release_from(&client, API_URL).await;
    }
    nightly_release_from(&client, API_RELEASES, API_URL).await
}

async fn nightly_release_from(
    client: &reqwest::Client,
    releases_url: &str,
    stable_url: &str,
) -> Result<Release, String> {
    let channel = UpdateChannel::Nightly;
    let mut candidates: Vec<Release> = optional_release_json_from(client, stable_url)
        .await?
        .into_iter()
        .collect();
    for page in 1..=MAX_RELEASE_PAGES {
        let url = format!("{releases_url}?per_page={RELEASE_PAGE_SIZE}&page={page}");
        let releases: Vec<Release> = release_json_from(client, &url).await?;
        let complete = releases.len() < RELEASE_PAGE_SIZE;
        let found_nightly = releases.iter().any(|release| {
            release_version(release, channel).is_ok_and(|identity| identity.is_nightly())
        });
        candidates.extend(releases);
        if complete || found_nightly {
            return select_release(candidates, channel);
        }
    }
    Err("The release catalog is too large. Open the release page to choose a build".into())
}

fn select_release(releases: Vec<Release>, channel: UpdateChannel) -> Result<Release, String> {
    releases
        .into_iter()
        .filter_map(|release| {
            release_version(&release, channel)
                .ok()
                .map(|identity| (identity, release))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|(_, release)| release)
        .ok_or_else(|| "No published release is available for this update channel".into())
}

async fn latest_release_from(client: &reqwest::Client, url: &str) -> Result<Release, String> {
    release_json_from(client, url).await
}

async fn release_json_from<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, String> {
    optional_release_json_from(client, url)
        .await?
        .ok_or_else(|| "Could not read the latest published release".into())
}

async fn optional_release_json_from<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<Option<T>, String> {
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| "Could not reach GitHub. Check your connection and try again")?;
    if response.url().as_str() != url {
        return Err("The release response came from an unexpected address".into());
    }
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
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
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| "The release response is invalid".into())
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
pub async fn check_for_update(
    update_channel: Option<UpdateChannel>,
) -> Result<UpdateCheck, String> {
    let channel = update_channel.unwrap_or_default();
    let release = latest_release(channel).await?;
    describe_release(
        &release,
        std::env::consts::OS,
        std::env::consts::ARCH,
        channel,
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

/// Revalidate the exact immutable build selected by the user, independently of
/// destination paths and native window state. Newer publications do not change it.
async fn requested_release_from(
    client: &reqwest::Client,
    releases_url: &str,
    version: &str,
    channel: UpdateChannel,
    current: &ReleaseIdentity,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Release, String> {
    let tag = format!("v{version}");
    let identity = ReleaseIdentity::published(&tag, version.contains("-nightly."), false, channel)?;
    if identity <= *current {
        return Err("The selected build is not a newer update for this channel".into());
    }
    let url = format!("{releases_url}/tags/{tag}");
    let release = release_or_cancel(latest_release_from(client, &url), cancel).await?;
    if release.tag_name != tag || release_version(&release, channel)? != identity {
        return Err("The available release changed. Check for updates again".into());
    }
    Ok(release)
}

#[tauri::command]
pub async fn download_update(
    app: tauri::AppHandle,
    version: String,
    update_channel: Option<UpdateChannel>,
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
    let channel = update_channel.unwrap_or_default();
    let release = requested_release_from(
        &client(Duration::from_secs(20))?,
        API_RELEASES,
        &version,
        channel,
        &current_identity(),
        cancel_receiver.clone(),
    )
    .await?;
    let name = package_name(
        &release.tag_name,
        std::env::consts::OS,
        std::env::consts::ARCH,
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
            installable: false,
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
        installable: false,
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
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    download_in_phase(client, asset, file, on_progress, cancel, None).await
}

async fn download_in_phase(
    client: &reqwest::Client,
    asset: &ReleaseAsset,
    file: &mut tokio::fs::File,
    on_progress: &Channel<DownloadProgress>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
    phase: Option<DownloadPhase>,
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
        phase,
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
            .map_err(|_| "Could not write the update file")?;
        hash.update(&chunk);
        if last_reported.elapsed() >= Duration::from_millis(200) || downloaded == asset.size {
            let _ = on_progress.send(DownloadProgress {
                phase,
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

    fn catalog_release(tag: &str, prerelease: bool, draft: bool) -> Release {
        serde_json::from_value(serde_json::json!({
            "tag_name": tag, "body": "Notes", "draft": draft,
            "prerelease": prerelease, "assets": []
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn nightly_discovery_works_before_the_first_stable_release() {
        use tokio::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            for (path, status, body) in [
                ("/latest", "404 Not Found", "{}".to_string()),
                (
                    "/releases?per_page=10&page=1",
                    "200 OK",
                    serde_json::json!([{
                        "tag_name": "v0.4.3-nightly.123.1", "body": "Try it",
                        "draft": false, "prerelease": true, "assets": []
                    }])
                    .to_string(),
                ),
            ] {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).await.unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..read]).starts_with(&format!("GET {path} "))
                );
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let result = nightly_release_from(
            &client,
            &format!("{url}/releases"),
            &format!("{url}/latest"),
        )
        .await;
        assert!(
            result.is_ok(),
            "A Stable release is not a prerequisite for Nightly: {:?}",
            result.err()
        );
        server.await.unwrap();
    }

    async fn serve_release_responses(
        responses: Vec<(String, &'static str, String)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for (path, status, body) in responses {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 2048];
                let read = stream.read(&mut request).await.unwrap();
                assert!(
                    String::from_utf8_lossy(&request[..read]).starts_with(&format!("GET {path} "))
                );
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, task)
    }

    fn release_body(tag: &str, prerelease: bool, draft: bool) -> serde_json::Value {
        serde_json::json!({"tag_name":tag,"body":"Notes","prerelease":prerelease,"draft":draft,"assets":[]})
    }

    #[tokio::test]
    async fn download_lookup_keeps_exact_identity_and_rejects_changed_publication() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let current = ReleaseIdentity::parse("v0.4.3").unwrap();
        let tag = "v0.4.4-nightly.123.1";
        for (returned_tag, prerelease, draft, succeeds) in [
            (tag, true, false, true),
            ("v0.4.4-nightly.124.1", true, false, false),
            (tag, false, false, false),
            (tag, true, true, false),
        ] {
            let (url, server) = serve_release_responses(vec![(
                format!("/tags/{tag}"),
                "200 OK",
                release_body(returned_tag, prerelease, draft).to_string(),
            )])
            .await;
            let (_cancel_sender, cancel) = tokio::sync::watch::channel(false);
            let result = requested_release_from(
                &client,
                &url,
                &tag[1..],
                UpdateChannel::Nightly,
                &current,
                cancel,
            )
            .await;
            assert_eq!(
                result.is_ok(),
                succeeds,
                "{returned_tag} prerelease={prerelease} draft={draft}"
            );
            server.await.unwrap();
        }
        // Reject channel mismatch, development identities, traversal and
        // downgrades before making any HTTP request.
        for (version, channel) in [
            ("0.4.4-nightly.123.1", UpdateChannel::Stable),
            ("0.4.4-dev", UpdateChannel::Nightly),
            ("0.4.3", UpdateChannel::Stable),
            ("0.4.2", UpdateChannel::Nightly),
            ("0.4.4/../../other", UpdateChannel::Nightly),
        ] {
            let (_cancel_sender, cancel) = tokio::sync::watch::channel(false);
            let error = requested_release_from(
                &client,
                "http://127.0.0.1:1",
                version,
                channel,
                &current,
                cancel,
            )
            .await
            .err()
            .unwrap();
            assert!(
                !error.contains("reach GitHub"),
                "must reject before the connection: {version}"
            );
        }
    }

    #[tokio::test]
    async fn nightly_discovery_stops_at_its_bound_and_reports_no_public_builds() {
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let foreign = vec![release_body("v99.0.0-beta.1", true, false); RELEASE_PAGE_SIZE];
        let mut responses = vec![("/latest".to_string(), "404 Not Found", "{}".to_string())];
        for page in 1..=MAX_RELEASE_PAGES {
            responses.push((
                format!("/releases?per_page={RELEASE_PAGE_SIZE}&page={page}"),
                "200 OK",
                serde_json::to_string(&foreign).unwrap(),
            ));
        }
        let (url, server) = serve_release_responses(responses).await;
        let error = nightly_release_from(
            &client,
            &format!("{url}/releases"),
            &format!("{url}/latest"),
        )
        .await
        .err()
        .unwrap();
        assert!(error.contains("catalog is too large"));
        server.await.unwrap();

        for stable in [None, Some("v0.4.3")] {
            let response = stable
                .map(|tag| ("200 OK", release_body(tag, false, false).to_string()))
                .unwrap_or(("404 Not Found", "{}".into()));
            let (url, server) = serve_release_responses(vec![
                ("/latest".into(), response.0, response.1),
                ("/releases?per_page=10&page=1".into(), "200 OK", "[]".into()),
            ])
            .await;
            let result = nightly_release_from(
                &client,
                &format!("{url}/releases"),
                &format!("{url}/latest"),
            )
            .await;
            if let Some(tag) = stable {
                assert_eq!(result.unwrap().tag_name, tag);
            } else {
                assert!(result.err().unwrap().contains("No published release"));
            }
            server.await.unwrap();
        }
    }

    #[test]
    fn channel_selection_ignores_drafts_and_foreign_prereleases() {
        let releases = || {
            vec![
                catalog_release("v0.4.4-nightly.9.1", true, false),
                catalog_release("v0.4.4-nightly.10.1", true, false),
                catalog_release("v0.4.5-beta.999", true, false),
                catalog_release("v0.4.5-nightly.20.1", true, true),
                catalog_release("v0.4.3", false, false),
            ]
        };
        assert_eq!(
            select_release(releases(), UpdateChannel::Stable)
                .unwrap()
                .tag_name,
            "v0.4.3"
        );
        assert_eq!(
            select_release(releases(), UpdateChannel::Nightly)
                .unwrap()
                .tag_name,
            "v0.4.4-nightly.10.1"
        );
        assert_eq!(
            select_release(
                vec![
                    catalog_release("v0.4.5", false, false),
                    catalog_release("v0.4.4-nightly.10.1", true, false)
                ],
                UpdateChannel::Nightly
            )
            .unwrap()
            .tag_name,
            "v0.4.5"
        );
        assert!(
            select_release(
                vec![catalog_release("v0.4.5-beta.1", true, false)],
                UpdateChannel::Nightly
            )
            .is_err()
        );
    }

    #[test]
    fn nightly_descriptions_do_not_fallback_to_an_older_package() {
        let base = current_version();
        let tag = format!("v{base}-nightly.999999999999.1");
        let name = package_name(&tag, "windows", "aarch64").unwrap();
        let mut release = catalog_release(&tag, true, false);
        release.assets.push(ReleaseAsset {
            name: name.clone(),
            state: "uploaded".into(),
            size: 42,
            browser_download_url: format!("{RELEASES_URL}/download/{tag}/{name}"),
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        });
        let check =
            describe_release(&release, "windows", "aarch64", UpdateChannel::Nightly).unwrap();
        assert!(check.is_newer);
        assert_eq!(check.latest_version, tag[1..]);
        assert_eq!(check.channel, UpdateChannel::Nightly);
        assert_eq!(check.package_name.as_deref(), Some(name.as_str()));
        // A legacy suffixed asset cannot substitute for the canonical desktop package.
        release.assets[0].name = name.replace(".zip", "_lite.zip");
        let unavailable =
            describe_release(&release, "windows", "aarch64", UpdateChannel::Nightly).unwrap();
        assert!(unavailable.package_name.is_none());
        assert!(describe_release(&release, "windows", "aarch64", UpdateChannel::Stable).is_err());
    }

    #[test]
    fn package_names_match_all_release_targets() {
        assert_eq!(
            package_name("v0.4.0", "macos", "aarch64").unwrap(),
            "mesh-talk_v0.4.0_macos_arm64.zip"
        );
        assert_eq!(
            package_name("v0.4.0", "windows", "x86_64").unwrap(),
            "mesh-talk_v0.4.0_windows_x86_64.zip"
        );
        assert_eq!(
            package_name("v0.4.0", "linux", "aarch64").unwrap(),
            "mesh-talk_v0.4.0_linux_aarch64.zip"
        );
        assert!(package_name("v0.4.0", "freebsd", "x86_64").is_none());
        assert!(package_name("v0.4.0", "linux", "riscv64").is_none());
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
            describe_release(&release, "macos", "aarch64", UpdateChannel::Stable)
                .unwrap()
                .package_name
                .is_some()
        );
        release.assets[0].digest = None;
        assert!(
            describe_release(&release, "macos", "aarch64", UpdateChannel::Stable)
                .unwrap()
                .package_name
                .is_none()
        );
        release.assets[0].digest = Some(format!("sha256:{}", "a".repeat(64)));
        release.assets[0].browser_download_url = "https://example.com/fake.zip".into();
        assert!(
            describe_release(&release, "macos", "aarch64", UpdateChannel::Stable)
                .unwrap()
                .package_name
                .is_none()
        );
        release.prerelease = true;
        assert!(describe_release(&release, "macos", "aarch64", UpdateChannel::Stable).is_err());
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
        let current =
            describe_release(&release, "macos", "aarch64", UpdateChannel::Stable).unwrap();
        assert_eq!(
            current.is_newer,
            release_version(&release, UpdateChannel::Stable).unwrap() > current_identity()
        );
        assert_eq!(current.current_version, env!("MESH_TALK_BUILD_TAG")[1..]);
        assert_eq!(current.latest_version, installed.to_string());
        assert_eq!(current.notes.chars().count(), 1200);
        assert!(current.package_name.is_none());

        release.tag_name = next_release_tag();
        let unavailable =
            describe_release(&release, "linux", "x86_64", UpdateChannel::Stable).unwrap();
        assert!(unavailable.is_newer);
        assert!(unavailable.package_name.is_none());
        assert_eq!(unavailable.edition, "default");

        release.draft = true;
        assert!(release_version(&release, UpdateChannel::Stable).is_err());
        release.draft = false;
        release.tag_name = "0.4.0".into();
        assert!(release_version(&release, UpdateChannel::Stable).is_err());
        release.tag_name = "v0.4.0-beta.1".into();
        assert!(release_version(&release, UpdateChannel::Stable).is_err());
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

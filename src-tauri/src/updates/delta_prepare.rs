//! Optional delta transport; native installation and trust stay in the existing owner.
use super::*;
use crate::updates::{DownloadPhase, delta_catalog, delta_codec};
use sha2::{Digest, Sha256};

#[path = "delta_metadata.rs"]
mod metadata;

pub(super) struct BaseSource<'a> {
    pub tag: &'a str,
    pub executable: &'a Path,
}

pub(super) enum Preparation {
    Missing,
    Fallback,
    Ready(Box<Update>, String),
}

fn canceled(cancel: &tokio::sync::watch::Receiver<bool>) -> Result<(), String> {
    if *cancel.borrow() {
        Err("Download canceled".into())
    } else {
        Ok(())
    }
}

fn phase(progress: &Channel<DownloadProgress>, value: DownloadPhase) {
    let _ = progress.send(DownloadProgress {
        downloaded: 0,
        total: 0,
        phase: Some(value),
    });
}

async fn download(
    host: &PreparationHost<'_, impl tauri::Runtime>,
    asset: &ReleaseAsset,
    destination: &Path,
    progress: &Channel<DownloadProgress>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Vec<u8>, String> {
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|_| "Could not stage the delta update")?;
    crate::updates::download_and_verify_with_client(
        &host.payload_http,
        asset,
        &mut file,
        progress,
        cancel,
    )
    .await?;
    drop(file);
    tokio::fs::read(destination)
        .await
        .map_err(|_| "Could not read the delta update".into())
}

pub(super) async fn prepare<R: tauri::Runtime>(
    host: &PreparationHost<'_, R>,
    release: &Release,
    canonical: &ReleaseAsset,
    directory: &Path,
    source: &BaseSource<'_>,
    on_progress: &Channel<DownloadProgress>,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Preparation, String> {
    canceled(&cancel)?;
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    if !matches!(os, "macos" | "windows") {
        return Ok(Preparation::Missing);
    }
    let name = delta_catalog::catalog_name(&release.tag_name, os, arch)
        .ok_or("Unsupported delta platform")?;
    let Some(catalog_asset) = selected_asset(release, &name) else {
        return Ok(Preparation::Missing);
    };
    let Some(catalog_signature_asset) = selected_asset(release, &format!("{name}.sig")) else {
        return Ok(Preparation::Missing);
    };
    if catalog_asset.size > delta_catalog::MAX_CATALOG_SIZE {
        return Ok(Preparation::Fallback);
    }
    let scratch = tempfile::tempdir_in(directory).map_err(|_| "Could not prepare delta staging")?;
    // Do not expose metadata transfer as package progress, or leave a misleading 100%.
    phase(on_progress, DownloadPhase::Preparing);
    let Some(bytes) = metadata::fetch(
        &host.metadata_http,
        catalog_asset,
        delta_catalog::MAX_CATALOG_SIZE,
        cancel.clone(),
    )
    .await?
    else {
        return Ok(Preparation::Fallback);
    };
    let Some(signature) = metadata::signature(
        &host.signature_http,
        catalog_signature_asset,
        cancel.clone(),
    )
    .await?
    else {
        return Ok(Preparation::Fallback);
    };
    let version = release
        .tag_name
        .strip_prefix('v')
        .ok_or("Invalid release identity")?;
    let verifier = signed_update(
        host.app,
        version,
        catalog_asset,
        &signature,
        host.public_key,
    )?;
    let authenticated = tokio::task::spawn_blocking(move || {
        verifier
            .verify(&bytes)
            .map_err(|_| "The delta catalog signature or signed version is invalid")?;
        Ok::<_, String>(bytes)
    })
    .await
    .map_err(|_| "Delta catalog verification was interrupted")??;
    canceled(&cancel)?;
    let catalog = delta_catalog::Catalog::parse(&authenticated, &release.tag_name, os, arch)?;
    if !catalog.fallback.matches(canonical) {
        return Err("The signed delta catalog contradicts the full update".into());
    }
    let Some(target) = selected_asset(release, &catalog.target.name) else {
        return Ok(Preparation::Fallback);
    };
    if !catalog.target.matches(target) {
        return Err("The signed delta target contradicts this release".into());
    }
    let Some(target_signature) = selected_asset(release, &format!("{}.sig", target.name)) else {
        return Ok(Preparation::Fallback);
    };
    if target_signature.size > 16 * 1024 {
        return Err("The delta target signature exceeds its limit".into());
    }
    // Unknown codecs are an optional transport capability, never executable code.
    if catalog.target.size > delta_codec::MAX_OUTPUT_SIZE || catalog.algorithm != "zstd-prefix-v1" {
        return Ok(Preparation::Fallback);
    }
    if !catalog
        .bases
        .iter()
        .any(|base| base.tag == source.tag && base.size <= delta_codec::MAX_BASE_SIZE as u64)
    {
        return Ok(Preparation::Fallback);
    }
    let base_path = source.executable.to_owned();
    let target_size = target.size;
    let budget = catalog
        .bases
        .iter()
        .map(|base| base.patch.size)
        .max()
        .unwrap_or(0)
        .saturating_add(target_size)
        .saturating_add(delta_codec::MAX_BASE_SIZE as u64)
        .saturating_add(16 * 1024 * 1024);
    match (host.available_space)(directory) {
        Ok(available) if available >= budget => {}
        _ => return Ok(Preparation::Fallback),
    }
    let snapshot_cancel = cancel.clone();
    let base = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let file = std::fs::File::open(base_path).map_err(|_| "Executable base unavailable")?;
        if file
            .metadata()
            .map_err(|_| "Executable base unavailable")?
            .len()
            > delta_codec::MAX_BASE_SIZE as u64
        {
            return Err("Executable base exceeds its limit");
        }
        let mut base = Vec::new();
        let mut input = file.take(delta_codec::MAX_BASE_SIZE as u64 + 1);
        let mut buffer = [0; 64 * 1024];
        loop {
            if *snapshot_cancel.borrow() {
                return Err("Download canceled");
            }
            let count = input
                .read(&mut buffer)
                .map_err(|_| "Executable base unavailable")?;
            if count == 0 {
                break;
            }
            base.extend_from_slice(&buffer[..count]);
        }
        if base.len() > delta_codec::MAX_BASE_SIZE {
            return Err("Executable base exceeds its limit");
        }
        Ok(base)
    })
    .await
    .map_err(|_| "Executable snapshot was interrupted")?;
    canceled(&cancel)?;
    let Ok(base) = base else {
        return Ok(Preparation::Fallback);
    };
    let hash = hex::encode(Sha256::digest(&base));
    let Some(candidate) = catalog.matching_base(source.tag, base.len() as u64, &hash) else {
        return Ok(Preparation::Fallback);
    };
    let overhead = catalog_asset
        .size
        .saturating_add(catalog_signature_asset.size)
        .saturating_add(target_signature.size);
    if candidate.patch.size > delta_codec::MAX_OUTPUT_SIZE
        || !delta_catalog::worthwhile(canonical.size, candidate.patch.size, overhead)
    {
        return Ok(Preparation::Fallback);
    }
    let Some(patch_asset) = selected_asset(release, &candidate.patch.name) else {
        return Ok(Preparation::Fallback);
    };
    if !candidate.patch.matches(patch_asset) {
        return Err("The signed delta patch contradicts this release".into());
    }
    let patch = download(
        host,
        patch_asset,
        &scratch.path().join("patch"),
        on_progress,
        cancel.clone(),
    )
    .await;
    canceled(&cancel)?;
    let Ok(patch) = patch else {
        return Ok(Preparation::Fallback);
    };
    phase(on_progress, DownloadPhase::Preparing);
    let output_path = scratch.path().join("reconstructed");
    let output = std::fs::File::create(&output_path)
        .map_err(|_| "Could not stage the reconstructed update")?;
    let hash = catalog.target.sha256.clone();
    let decode_cancel = cancel.clone();
    let reconstructed = tokio::task::spawn_blocking(move || {
        let mut output = output;
        delta_codec::reconstruct(&base, &patch, &mut output, target_size, &hash, || {
            *decode_cancel.borrow()
        })?;
        output.sync_all().map_err(delta_codec::DeltaError::Io)
    })
    .await
    .map_err(|_| "Delta reconstruction was interrupted")?;
    canceled(&cancel)?;
    if let Err(error) = reconstructed {
        if matches!(error, delta_codec::DeltaError::Canceled) {
            return Err("Download canceled".into());
        }
        log::info!("Delta preparation unavailable; using the signed full update: {error}");
        return Ok(Preparation::Fallback);
    }
    let Some(signature) =
        metadata::signature(&host.signature_http, target_signature, cancel.clone()).await?
    else {
        return Ok(Preparation::Fallback);
    };
    let update = signed_update(host.app, version, target, &signature, host.public_key)?;
    let bytes = tokio::fs::read(&output_path)
        .await
        .map_err(|_| "Could not read the reconstructed update")?;
    let verifier = update.clone();
    tokio::task::spawn_blocking(move || verifier.verify(&bytes))
        .await
        .map_err(|_| "Delta verification was interrupted")?
        .map_err(|_| "The reconstructed update signature or signed version is invalid")?;
    canceled(&cancel)?;
    tokio::fs::rename(output_path, directory.join("payload"))
        .await
        .map_err(|_| "Could not stage the verified update")?;
    log::info!(
        "Prepared signed delta update: {} patch bytes, {} canonical full bytes",
        patch_asset.size,
        canonical.size
    );
    Ok(Preparation::Ready(Box::new(update), target.name.clone()))
}

//! Optional bounded metadata transport. Network unavailability is distinct from
//! cancellation or received bytes that contradict the selected immutable asset.
use super::*;

/// None means an ordinary network failure; integrity and cancellation are errors.
pub(super) async fn fetch(
    http: &reqwest::Client,
    asset: &ReleaseAsset,
    maximum: u64,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Option<Vec<u8>>, String> {
    canceled(&cancel)?;
    if asset.size > maximum {
        return Err("The delta metadata exceeds its size limit".into());
    }
    let request = http.get(&asset.browser_download_url).send();
    let response = tokio::select! {
        response = request => response,
        _ = cancel.changed() => return Err("Download canceled".into()),
    };
    canceled(&cancel)?;
    let mut response = match response.and_then(reqwest::Response::error_for_status) {
        Ok(response) => response,
        Err(_) => return Ok(None),
    };
    if response
        .content_length()
        .is_some_and(|size| size != asset.size)
    {
        return Err("The delta metadata has an unexpected size".into());
    }
    let mut bytes = Vec::new();
    loop {
        canceled(&cancel)?;
        let chunk = tokio::select! {
            result = response.chunk() => result,
            _ = cancel.changed() => return Err("Download canceled".into()),
        };
        canceled(&cancel)?;
        let chunk = match chunk {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return Ok(None),
        };
        if bytes.len() as u64 + chunk.len() as u64 > asset.size {
            return Err("The delta metadata exceeded its declared size".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() as u64 != asset.size
        || asset.digest.as_deref() != Some(format!("sha256:{:x}", Sha256::digest(&bytes)).as_str())
    {
        return Err("The delta metadata failed its integrity check".into());
    }
    Ok(Some(bytes))
}

pub(super) async fn signature(
    http: &reqwest::Client,
    asset: &ReleaseAsset,
    cancel: tokio::sync::watch::Receiver<bool>,
) -> Result<Option<String>, String> {
    fetch(http, asset, 16 * 1024, cancel)
        .await?
        .map(|bytes| String::from_utf8(bytes).map_err(|_| "The delta signature is invalid".into()))
        .transpose()
}

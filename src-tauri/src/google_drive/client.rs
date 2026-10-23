//! Bounded Drive API transport. Only complete, checksum-verified archives are listed.
use crate::commands::CommandError;
use reqwest::{Client, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Core accepts 8 GiB plaintext plus authenticated archive framing.
const MAX_ARCHIVE_BYTES: u64 = 9 * 1024 * 1024 * 1024;
const CHUNK_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 1024 * 1024;
const FILE_FIELDS: &str = "id,name,size,sha256Checksum,appProperties";
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct DriveArchive {
    pub id: String,
    pub name: String,
    pub size: String,
    #[serde(rename(deserialize = "sha256Checksum", serialize = "checksum"))]
    pub checksum: String,
    #[serde(rename = "appProperties", default, skip_serializing)]
    properties: HashMap<String, String>,
}
impl DriveArchive {
    fn checked_size(&self) -> Result<u64, CommandError> {
        let size = self.size.parse::<u64>().map_err(|_| invalid_archive())?;
        if size == 0
            || size > MAX_ARCHIVE_BYTES
            || self.checksum.len() != 64
            || hex::decode(&self.checksum).is_err()
            || !valid_id(&self.id)
        {
            return Err(invalid_archive());
        }
        Ok(size)
    }
    fn is_complete(&self) -> bool {
        self.properties
            .get("meshTalkBackup")
            .is_some_and(|v| v == "1")
            && self.properties.get("complete").is_some_and(|v| v == "true")
    }
}
fn invalid_archive() -> CommandError {
    CommandError::InvalidInput("Google Drive backup is incomplete, damaged or too large".into())
}
fn transfer_error() -> CommandError {
    CommandError::Io(
        "Google Drive transfer failed. Check your connection and available storage, then retry."
            .into(),
    )
}
pub(super) fn http_client() -> Result<Client, CommandError> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|_| transfer_error())
}
async fn checked(response: Response) -> Result<Response, CommandError> {
    match response.status() {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(CommandError::Authentication("Google Drive access expired or was denied. Reconnect Google Drive and check storage permissions.".into())),
        status if status.is_success() => Ok(response),
        _ => Err(transfer_error()),
    }
}
pub(super) async fn json_response<T: serde::de::DeserializeOwned>(
    mut response: Response,
) -> Result<T, CommandError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| transfer_error())? {
        if bytes.len() + chunk.len() > MAX_JSON_BYTES {
            return Err(transfer_error());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| transfer_error())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
#[derive(Clone)]
pub(super) struct DriveClient {
    http: Client,
    api: String,
    upload_api: String,
    #[cfg(test)]
    test_origin: Option<String>,
}
impl DriveClient {
    pub fn new() -> Result<Self, CommandError> {
        Ok(Self {
            http: http_client()?,
            api: "https://www.googleapis.com/drive/v3".into(),
            upload_api: "https://www.googleapis.com/upload/drive/v3".into(),
            #[cfg(test)]
            test_origin: None,
        })
    }
    fn file_url(&self, id: &str) -> Result<String, CommandError> {
        if !valid_id(id) {
            return Err(invalid_archive());
        }
        Ok(format!("{}/files/{id}", self.api))
    }
    fn validate_upload_url(&self, location: &str) -> Result<Url, CommandError> {
        let url = Url::parse(location).map_err(|_| transfer_error())?;
        let allowed = url.scheme() == "https"
            && url.host_str() == Some("www.googleapis.com")
            && url.port_or_known_default() == Some(443);
        #[cfg(test)]
        let allowed = allowed
            || self
                .test_origin
                .as_ref()
                .is_some_and(|origin| url.origin().ascii_serialization() == *origin);
        if !allowed
            || url.path() != "/upload/drive/v3/files"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(transfer_error());
        }
        Ok(url)
    }
    pub async fn list(&self, token: &str) -> Result<Vec<DriveArchive>, CommandError> {
        #[derive(Deserialize)]
        struct Page {
            #[serde(default)]
            files: Vec<DriveArchive>,
            #[serde(rename = "nextPageToken")]
            next: Option<String>,
        }
        let mut results = Vec::new();
        let mut page = String::new();
        for _ in 0..100 {
            let mut request = self.http.get(format!("{}/files", self.api)).bearer_auth(token).query(&[("spaces", "appDataFolder"), ("q", "trashed = false and appProperties has { key='meshTalkBackup' and value='1' } and appProperties has { key='complete' and value='true' }"), ("fields", &format!("nextPageToken,files({FILE_FIELDS})")), ("orderBy", "createdTime desc"), ("pageSize", "100")]);
            if !page.is_empty() {
                request = request.query(&[("pageToken", &page)]);
            }
            let response = checked(request.send().await.map_err(|_| transfer_error())?).await?;
            let parsed: Page = json_response(response).await?;
            for item in parsed.files {
                if item.is_complete() && item.checked_size().is_ok() {
                    results.push(item);
                }
            }
            match parsed.next {
                Some(next) if !next.is_empty() && next != page => page = next,
                None => return Ok(results),
                _ => return Err(transfer_error()),
            }
        }
        Err(CommandError::Io("Too many Google Drive backups to list. Remove older app data in Google Drive settings.".into()))
    }
    pub async fn upload(
        &self,
        token: &str,
        source: &Path,
        name: &str,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<DriveArchive, CommandError> {
        let mut file = tokio::fs::File::open(source)
            .await
            .map_err(|_| transfer_error())?;
        let total = file.metadata().await.map_err(|_| transfer_error())?.len();
        if total == 0 || total > MAX_ARCHIVE_BYTES {
            return Err(invalid_archive());
        }
        let response = checked(self.http.post(format!("{}/files", self.upload_api)).bearer_auth(token).query(&[("uploadType","resumable"),("fields",FILE_FIELDS)]).header("X-Upload-Content-Type", "application/octet-stream").header("X-Upload-Content-Length", total).json(&serde_json::json!({"name":name,"parents":["appDataFolder"],"appProperties":{"meshTalkBackup":"1","complete":"false"}})).send().await.map_err(|_| transfer_error())?).await?;
        let location = self.validate_upload_url(
            response
                .headers()
                .get("location")
                .and_then(|h| h.to_str().ok())
                .ok_or_else(transfer_error)?,
        )?;
        let mut sent = 0;
        let mut hash = Sha256::new();
        progress(0, total);
        loop {
            let count = (total - sent).min(CHUNK_BYTES as u64) as usize;
            let mut bytes = vec![0; count];
            file.read_exact(&mut bytes)
                .await
                .map_err(|_| transfer_error())?;
            hash.update(&bytes);
            let end = sent + count as u64;
            let response = self
                .http
                .put(location.clone())
                .bearer_auth(token)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Range", format!("bytes {sent}-{}/{total}", end - 1))
                .body(bytes)
                .send()
                .await
                .map_err(|_| transfer_error())?;
            if end < total {
                if response.status().as_u16() != 308
                    || response
                        .headers()
                        .get("range")
                        .and_then(|h| h.to_str().ok())
                        != Some(format!("bytes=0-{}", end - 1).as_str())
                {
                    return Err(transfer_error());
                }
                sent = end;
                progress(sent, total);
                continue;
            }
            let archive: DriveArchive = json_response(checked(response).await?).await?;
            if archive.checked_size()? != total || archive.checksum != hex::encode(hash.finalize())
            {
                return Err(invalid_archive());
            }
            // Never reuse an existing file ID or replace a previous backup.
            checked(
                self.http
                    .patch(self.file_url(&archive.id)?)
                    .bearer_auth(token)
                    .json(&serde_json::json!({"appProperties":{"meshTalkBackup":"1","complete":"true"}}))
                    .send()
                    .await
                    .map_err(|_| transfer_error())?,
            )
            .await?;
            progress(total, total);
            return Ok(archive);
        }
    }
    pub async fn download(
        &self,
        token: &str,
        id: &str,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<tempfile::NamedTempFile, CommandError> {
        let url = self.file_url(id)?;
        let metadata: DriveArchive = json_response(
            checked(
                self.http
                    .get(&url)
                    .bearer_auth(token)
                    .query(&[("fields", FILE_FIELDS)])
                    .send()
                    .await
                    .map_err(|_| transfer_error())?,
            )
            .await?,
        )
        .await?;
        let total = metadata.checked_size()?;
        progress(0, total);
        if metadata.id != id || !metadata.is_complete() {
            return Err(invalid_archive());
        }
        let mut response = checked(
            self.http
                .get(url)
                .bearer_auth(token)
                .query(&[("alt", "media")])
                .send()
                .await
                .map_err(|_| transfer_error())?,
        )
        .await?;
        if response
            .content_length()
            .is_some_and(|length| length != total)
        {
            return Err(invalid_archive());
        }
        let file = tempfile::NamedTempFile::new().map_err(|_| transfer_error())?;
        let mut writer = tokio::fs::File::from_std(file.reopen().map_err(|_| transfer_error())?);
        let mut received = 0u64;
        let mut hash = Sha256::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| transfer_error())? {
            received += chunk.len() as u64;
            if received > total {
                return Err(invalid_archive());
            }
            hash.update(&chunk);
            progress(received, total);
            writer
                .write_all(&chunk)
                .await
                .map_err(|_| transfer_error())?;
        }
        if received != total || hex::encode(hash.finalize()) != metadata.checksum {
            return Err(invalid_archive());
        }
        writer.sync_all().await.map_err(|_| transfer_error())?;
        drop(writer);
        Ok(file)
    }
}
#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

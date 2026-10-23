use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

async fn server(
    replies: Vec<(u16, String)>,
) -> (DriveClient, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let client = DriveClient {
        http: Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .unwrap(),
        api: format!("{base}/drive/v3"),
        upload_api: format!("{base}/upload/drive/v3"),
        test_origin: Some(base.clone()),
    };
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                    .await
                    .unwrap()
                    .unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let header = String::from_utf8_lossy(&bytes[..header_end]);
            let length = header
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .and_then(|n| n.parse::<usize>().ok())
                })
                .unwrap_or(0);
            while bytes.len() < header_end + length {
                let mut chunk = [0; 4096];
                let n = socket.read(&mut chunk).await.unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&chunk[..n]);
            }
            requests.push(String::from_utf8_lossy(&bytes).to_string());
            let body = body.replace("$ORIGIN", &base);
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\nContent-Type: application/json\r\n{}\r\n{body}", body.len(), if status == 201 { format!("Location: {base}/upload/drive/v3/files?upload_id=test\r\n") } else if status == 308 { format!("Range: bytes=0-{}\r\n",CHUNK_BYTES-1) } else { String::new() }).as_bytes()).await.unwrap();
        }
        requests
    });
    (client, task)
}
fn metadata(bytes: &[u8]) -> String {
    serde_json::json!({"id":"archive_1", "name":"backup.mtbackup", "size":bytes.len().to_string(), "sha256Checksum":hex::encode(Sha256::digest(bytes)), "appProperties":{"meshTalkBackup":"1", "complete":"true"}}).to_string()
}
#[tokio::test]
async fn download_checks_size_and_digest_before_returning_a_file() {
    let bytes = b"encrypted archive";
    let (client, task) = server(vec![
        (200, metadata(bytes)),
        (200, String::from_utf8(bytes.to_vec()).unwrap()),
    ])
    .await;
    let file = client
        .download("token", "archive_1", |_, _| {})
        .await
        .unwrap();
    assert_eq!(std::fs::read(file.path()).unwrap(), bytes);
    let requests = task.await.unwrap();
    assert!(requests[1].contains("alt=media"));
    assert!(requests.iter().all(|r| {
        r.to_ascii_lowercase()
            .contains("authorization: bearer token")
    }));
}
#[tokio::test]
async fn corrupted_download_is_rejected() {
    let (client, task) = server(vec![(200, metadata(b"good")), (200, "evil".into())]).await;
    assert!(
        client
            .download("token", "archive_1", |_, _| {})
            .await
            .is_err()
    );
    task.await.unwrap();
}
#[tokio::test]
async fn expired_authorization_has_an_actionable_error_without_body_leakage() {
    let (client, task) = server(vec![(401, "secret-token-body".into())]).await;
    let error = client.list("token").await.unwrap_err().to_string();
    assert!(error.contains("Reconnect"));
    assert!(!error.contains("secret-token"));
    task.await.unwrap();
}
#[tokio::test]
async fn listing_follows_pages_and_uses_only_completed_app_archives() {
    let item: serde_json::Value = serde_json::from_str(&metadata(b"a")).unwrap();
    let (client, task) = server(vec![
        (
            200,
            serde_json::json!({"files":[],"nextPageToken":"next"}).to_string(),
        ),
        (200, serde_json::json!({"files":[item]}).to_string()),
    ])
    .await;
    assert_eq!(client.list("token").await.unwrap().len(), 1);
    let requests = task.await.unwrap();
    assert!(requests[0].contains("spaces=appDataFolder"));
    assert!(requests[1].contains("pageToken=next"));
}
#[tokio::test]
async fn upload_publishes_complete_only_after_matching_remote_checksum() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"sealed").unwrap();
    let (client, task) = server(vec![
        (201, "{}".into()),
        (200, metadata(b"sealed")),
        (200, "{}".into()),
    ])
    .await;
    assert_eq!(
        client
            .upload("token", file.path(), "backup.mtbackup", |_, _| {})
            .await
            .unwrap()
            .id,
        "archive_1"
    );
    let requests = task.await.unwrap();
    assert!(requests[0].contains("\"complete\":\"false\""));
    assert!(
        requests[1].contains("Content-Range: bytes 0-5/6")
            || requests[1].contains("content-range: bytes 0-5/6")
    );
    assert!(requests[1].ends_with("sealed"));
    assert!(requests[2].contains("\"complete\":\"true\""));
}
#[tokio::test]
async fn checksum_mismatch_never_marks_an_upload_complete() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"sealed").unwrap();
    let (client, task) = server(vec![(201, "{}".into()), (200, metadata(b"wrong!"))]).await;
    assert!(
        client
            .upload("token", file.path(), "backup.mtbackup", |_, _| {})
            .await
            .is_err()
    );
    assert_eq!(task.await.unwrap().len(), 2);
}
#[test]
fn resumable_location_cannot_send_credentials_to_another_host() {
    let client = DriveClient::new().unwrap();
    assert!(
        client
            .validate_upload_url("https://www.googleapis.com/upload/drive/v3/files?upload_id=x")
            .is_ok()
    );
    for value in [
        "https://evil.example/upload/drive/v3/files",
        "http://www.googleapis.com/upload/drive/v3/files",
        "https://www.googleapis.com/other",
        "https://user@www.googleapis.com/upload/drive/v3/files",
    ] {
        assert!(client.validate_upload_url(value).is_err());
    }
}

#[tokio::test]
async fn oversized_or_incomplete_metadata_never_starts_media_download() {
    for invalid in [
        serde_json::json!({"id":"archive_1","name":"large","size":(MAX_ARCHIVE_BYTES+1).to_string(),"sha256Checksum":"a".repeat(64),"appProperties":{"meshTalkBackup":"1","complete":"true"}}),
        serde_json::json!({"id":"archive_1","name":"partial","size":"4","sha256Checksum":"a".repeat(64),"appProperties":{"meshTalkBackup":"1","complete":"false"}}),
    ] {
        let (client, task) = server(vec![(200, invalid.to_string())]).await;
        assert!(
            client
                .download("token", "archive_1", |_, _| {})
                .await
                .is_err()
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn short_media_response_is_rejected_and_never_returned() {
    let (client, task) = server(vec![
        (200, metadata(b"long archive")),
        (200, "short".into()),
    ])
    .await;
    assert!(
        client
            .download("token", "archive_1", |_, _| {})
            .await
            .is_err()
    );
    task.await.unwrap();
}

#[tokio::test]
async fn failed_upload_does_not_publish_a_complete_archive() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"sealed").unwrap();
    let (client, task) = server(vec![
        (201, "{}".into()),
        (503, "provider-internal-secret".into()),
    ])
    .await;
    let error = client
        .upload("token", file.path(), "backup.mtbackup", |_, _| {})
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("provider-internal-secret"));
    assert_eq!(task.await.unwrap().len(), 2);
}

#[tokio::test]
async fn successful_download_file_is_removed_when_its_owner_retires() {
    let (client, task) = server(vec![(200, metadata(b"sealed")), (200, "sealed".into())]).await;
    let file = client
        .download("token", "archive_1", |_, _| {})
        .await
        .unwrap();
    let path = file.path().to_path_buf();
    assert!(path.exists());
    drop(file);
    assert!(!path.exists());
    task.await.unwrap();
}

#[tokio::test]
async fn large_upload_advances_only_after_a_resumable_chunk_acknowledgement() {
    let bytes = vec![b'x'; CHUNK_BYTES + 5];
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), &bytes).unwrap();
    let (client, task) = server(vec![
        (201, "{}".into()),
        (308, String::new()),
        (200, metadata(&bytes)),
        (200, "{}".into()),
    ])
    .await;
    let mut progress = Vec::new();
    client
        .upload("token", file.path(), "backup.mtbackup", |sent, total| {
            progress.push((sent, total))
        })
        .await
        .unwrap();
    let requests = task.await.unwrap();
    assert!(requests[2].to_ascii_lowercase().contains(&format!(
        "content-range: bytes {}-{}/{}",
        CHUNK_BYTES,
        CHUNK_BYTES + 4,
        CHUNK_BYTES + 5
    )));
    assert_eq!(
        progress,
        vec![
            (0, bytes.len() as u64),
            (CHUNK_BYTES as u64, bytes.len() as u64),
            (bytes.len() as u64, bytes.len() as u64)
        ]
    );
}

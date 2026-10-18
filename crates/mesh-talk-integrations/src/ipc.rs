//! Bounded, authenticated local IPC. No network listener is opened.
use crate::{IntegrationError, IntegrationService};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    task::JoinSet,
};

pub(crate) const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_FRAME: usize = 64 * 1024;
const MAX_CONNECTIONS: usize = 16;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub version: u32,
    pub token: String,
    pub method: String,
    pub params: Value,
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum Response {
    Result { result: Value },
    Error { error: IntegrationError },
}
pub(crate) fn io_error() -> IntegrationError {
    IntegrationError::new("local_transport", "Local integration connection failed")
}
pub(crate) fn timeout_error() -> IntegrationError {
    IntegrationError::new(
        "timeout",
        "Local integration request timed out; query the request receipt before retrying",
    )
}
fn frame_error() -> IntegrationError {
    IntegrationError::new(
        "invalid_frame",
        "Invalid or oversized local integration frame",
    )
}
pub(crate) async fn read_frame<S: AsyncRead + Unpin, T: DeserializeOwned>(
    stream: &mut S,
) -> Result<T, IntegrationError> {
    let length = stream.read_u32().await.map_err(|_| io_error())? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(frame_error());
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| io_error())?;
    serde_json::from_slice(&bytes).map_err(|_| frame_error())
}
pub(crate) async fn write_frame<S: AsyncWrite + Unpin, T: Serialize>(
    stream: &mut S,
    value: &T,
) -> Result<(), IntegrationError> {
    let bytes = serde_json::to_vec(value).map_err(|_| frame_error())?;
    if bytes.len() > MAX_FRAME {
        return Err(frame_error());
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| io_error())?;
    stream.write_all(&bytes).await.map_err(|_| io_error())?;
    stream.flush().await.map_err(|_| io_error())
}

pub struct LocalServer {
    #[cfg(unix)]
    listener: tokio::net::UnixListener,
    #[cfg(windows)]
    listener: tokio::net::windows::named_pipe::NamedPipeServer,
    endpoint: String,
    #[cfg(unix)]
    socket_path: std::path::PathBuf,
}
impl LocalServer {
    pub async fn bind(directory: &Path) -> Result<Self, IntegrationError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
            if !directory.exists() {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(directory)
                    .map_err(|_| io_error())?;
            }
            let metadata = std::fs::symlink_metadata(directory).map_err(|_| io_error())?;
            // SAFETY: geteuid has no preconditions.
            if !metadata.is_dir()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.mode() & 0o777 != 0o700
            {
                return Err(io_error());
            }
            let socket_path = directory.join(format!("i-{}.sock", uuid::Uuid::new_v4().simple()));
            let listener = tokio::net::UnixListener::bind(&socket_path).map_err(|_| io_error())?;
            if std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600))
                .is_err()
            {
                let _ = std::fs::remove_file(&socket_path);
                return Err(io_error());
            }
            let endpoint = socket_path.to_str().ok_or_else(io_error)?.to_owned();
            Ok(Self {
                listener,
                endpoint,
                socket_path,
            })
        }
        #[cfg(windows)]
        {
            let _ = directory;
            let endpoint = format!(r"\\.\pipe\mesh-talk-{}", uuid::Uuid::new_v4().simple());
            let listener = create_pipe(&endpoint, true)?;
            Ok(Self { listener, endpoint })
        }
    }
    #[cfg(unix)]
    async fn accept(&mut self) -> Result<tokio::net::UnixStream, IntegrationError> {
        self.listener
            .accept()
            .await
            .map(|(stream, _)| stream)
            .map_err(|_| io_error())
    }
    #[cfg(windows)]
    async fn accept(
        &mut self,
    ) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, IntegrationError> {
        self.listener.connect().await.map_err(|_| io_error())?;
        let replacement = create_pipe(&self.endpoint, false)?;
        Ok(std::mem::replace(&mut self.listener, replacement))
    }
    pub fn endpoint(&self) -> String {
        self.endpoint.clone()
    }
    pub async fn run(self, service: Arc<IntegrationService>) -> Result<(), IntegrationError> {
        self.run_until(service, std::future::pending()).await
    }
    /// Shutdown waits for all request tasks to finish cancellation before returning.
    pub async fn run_until(
        mut self,
        service: Arc<IntegrationService>,
        shutdown: impl std::future::Future<Output = ()>,
    ) -> Result<(), IntegrationError> {
        let mut requests = JoinSet::new();
        tokio::pin!(shutdown);
        let result = loop {
            tokio::select! {
                _ = &mut shutdown => break Ok(()),
                _ = requests.join_next(), if !requests.is_empty() => {},
                accepted = self.accept(), if requests.len() < MAX_CONNECTIONS => {
                    let stream = match accepted { Ok(stream) => stream, Err(error) => break Err(error) };
                    let service = service.clone();
                    requests.spawn(async move { let _ = tokio::time::timeout(IO_TIMEOUT, serve(stream, service)).await; });
                }
            }
        };
        requests.abort_all();
        while requests.join_next().await.is_some() {}
        result
    }
}
#[cfg(unix)]
impl Drop for LocalServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    service: Arc<IntegrationService>,
) -> Result<(), IntegrationError> {
    let request: Request = read_frame(&mut stream).await?;
    let result = if request.version != 1 {
        Err(IntegrationError::new(
            "unsupported_version",
            "Unsupported local integration protocol version",
        ))
    } else {
        service
            .handle(&request.token, &request.method, request.params)
            .await
    };
    let response = match result {
        Ok(result) => Response::Result { result },
        Err(error) => Response::Error { error },
    };
    write_frame(&mut stream, &response).await
}
#[cfg(windows)]
fn create_pipe(
    endpoint: &str,
    first: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer, IntegrationError> {
    let security = crate::private_files::windows_security::Security::new()?;
    let mut attributes = security.attributes();
    // SAFETY: descriptor and attributes remain live throughout synchronous pipe creation.
    unsafe {
        tokio::net::windows::named_pipe::ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                endpoint,
                (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES).cast(),
            )
    }
    .map_err(|_| io_error())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn frames_round_trip_and_reject_untrusted_lengths() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        write_frame(&mut writer, &json!({"message":"hello"}))
            .await
            .unwrap();
        assert_eq!(
            read_frame::<_, Value>(&mut reader).await.unwrap(),
            json!({"message":"hello"})
        );
        writer.write_u32((MAX_FRAME + 1) as u32).await.unwrap();
        assert_eq!(
            read_frame::<_, Value>(&mut reader).await.unwrap_err().code,
            "invalid_frame"
        );
    }

    #[tokio::test]
    async fn oversized_output_is_rejected_before_writing() {
        let (mut writer, _) = tokio::io::duplex(64);
        assert_eq!(
            write_frame(&mut writer, &"x".repeat(MAX_FRAME))
                .await
                .unwrap_err()
                .code,
            "invalid_frame"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn socket_is_private_and_removed_on_drop() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let server = LocalServer::bind(directory.path()).await.unwrap();
        let socket = server.endpoint();
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(server);
        assert!(!Path::new(&socket).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_public_socket_directories() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(LocalServer::bind(directory.path()).await.is_err());
    }
}

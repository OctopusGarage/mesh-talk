//! One-request local client; credentials never enter diagnostic messages.
use crate::{
    IntegrationError,
    ipc::{IO_TIMEOUT, Request, Response, read_frame, write_frame},
    private_files::read_private_json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    pub endpoint: String,
    pub token: String,
}

/// A protected, replaceable locator for one running profile's IPC listener.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndpointManifest {
    pub endpoint: String,
}

pub fn endpoint_reference(path: &Path) -> Result<String, IntegrationError> {
    if !path.is_absolute() {
        return Err(crate::ipc::io_error());
    }
    Ok(format!(
        "file:{}",
        path.to_str().ok_or_else(crate::ipc::io_error)?
    ))
}
fn resolve_endpoint(reference: &str) -> Result<String, IntegrationError> {
    if let Some(path) = reference.strip_prefix("file:") {
        let path = Path::new(path);
        if !path.is_absolute() {
            return Err(crate::ipc::io_error());
        }
        let manifest: EndpointManifest = read_private_json(path)?;
        if manifest.endpoint.starts_with("file:") || manifest.endpoint.is_empty() {
            return Err(crate::ipc::io_error());
        }
        Ok(manifest.endpoint)
    } else {
        Ok(reference.to_owned())
    }
}

pub struct LocalClient {
    config: ClientConfig,
}
impl LocalClient {
    pub fn new(config: ClientConfig) -> Self {
        Self { config }
    }
    pub fn from_file(path: &Path) -> Result<Self, IntegrationError> {
        Ok(Self::new(read_private_json(path)?))
    }
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, IntegrationError> {
        tokio::time::timeout(IO_TIMEOUT, async {
            let endpoint = resolve_endpoint(&self.config.endpoint)?;
            let mut stream = connect(&endpoint).await?;
            write_frame(
                &mut stream,
                &Request {
                    version: 1,
                    token: self.config.token.clone(),
                    method: method.into(),
                    params,
                },
            )
            .await?;
            match read_frame::<_, Response>(&mut stream).await? {
                Response::Result { result } => Ok(result),
                Response::Error { error } => Err(error),
            }
        })
        .await
        .map_err(|_| crate::ipc::timeout_error())?
    }
}
#[cfg(unix)]
async fn connect(endpoint: &str) -> Result<tokio::net::UnixStream, IntegrationError> {
    tokio::net::UnixStream::connect(endpoint)
        .await
        .map_err(|_| crate::ipc::io_error())
}
#[cfg(windows)]
async fn connect(
    endpoint: &str,
) -> Result<tokio::net::windows::named_pipe::NamedPipeClient, IntegrationError> {
    // Only local pipes are valid, even when reading a malformed client config.
    if !endpoint.starts_with(r"\\.\pipe\mesh-talk-") {
        return Err(crate::ipc::io_error());
    }
    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint) {
            Ok(client) => return Ok(client),
            Err(error) if error.raw_os_error() == Some(231) => {
                tokio::time::sleep(std::time::Duration::from_millis(20)).await
            }
            Err(_) => return Err(crate::ipc::io_error()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn client_transmits_credentials_and_preserves_structured_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = read_frame(&mut stream).await.unwrap();
            assert_eq!(request.token, "secret");
            assert_eq!(request.method, "messages.send");
            assert_eq!(request.params, json!({"body":"hello"}));
            write_frame(
                &mut stream,
                &Response::Error {
                    error: IntegrationError::new("forbidden", "Not permitted"),
                },
            )
            .await
            .unwrap();
        });
        let client = LocalClient::new(ClientConfig {
            endpoint: path.to_str().unwrap().into(),
            token: "secret".into(),
        });
        assert_eq!(
            client
                .call("messages.send", json!({"body":"hello"}))
                .await
                .unwrap_err()
                .code,
            "forbidden"
        );
        server.await.unwrap();
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn protected_pipe_round_trips_structured_response() {
        use tokio::net::windows::named_pipe::ServerOptions;
        let endpoint = format!(r"\\.\pipe\mesh-talk-test-{}", uuid::Uuid::new_v4());
        let security = crate::private_files::windows_security::Security::new().unwrap();
        let mut attributes = security.attributes();
        // SAFETY: both security allocations live through synchronous creation.
        let mut pipe = unsafe {
            ServerOptions::new()
                .first_pipe_instance(true)
                .reject_remote_clients(true)
                .create_with_security_attributes_raw(
                    &endpoint,
                    (&mut attributes as *mut windows_sys::Win32::Security::SECURITY_ATTRIBUTES)
                        .cast(),
                )
        }
        .unwrap();
        let server = tokio::spawn(async move {
            pipe.connect().await.unwrap();
            let request: Request = read_frame(&mut pipe).await.unwrap();
            assert_eq!(request.token, "secret");
            write_frame(
                &mut pipe,
                &Response::Result {
                    result: json!({"ok":true}),
                },
            )
            .await
            .unwrap();
        });
        let client = LocalClient::new(ClientConfig {
            endpoint,
            token: "secret".into(),
        });
        assert_eq!(
            client.call("capabilities", json!({})).await.unwrap(),
            json!({"ok":true})
        );
        server.await.unwrap();
    }
}

#[cfg(all(test, unix))]
mod manifest_tests {
    use super::*;
    use crate::private_files::replace_private_json;
    use serde_json::json;

    async fn responder(path: &Path, value: Value) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(path).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request: Request = read_frame(&mut stream).await.unwrap();
            assert_eq!(request.token, "persistent-token");
            write_frame(&mut stream, &Response::Result { result: value })
                .await
                .unwrap();
        })
    }
    #[tokio::test]
    async fn unchanged_config_resolves_replaced_endpoint_each_call() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.sock");
        let second = dir.path().join("second.sock");
        let manifest = dir.path().join("endpoint.json");
        let first_server = responder(&first, json!(1)).await;
        replace_private_json(
            &manifest,
            &EndpointManifest {
                endpoint: first.to_str().unwrap().into(),
            },
        )
        .unwrap();
        let client = LocalClient::new(ClientConfig {
            endpoint: endpoint_reference(&manifest).unwrap(),
            token: "persistent-token".into(),
        });
        assert_eq!(client.call("context", json!({})).await.unwrap(), json!(1));
        first_server.await.unwrap();
        let second_server = responder(&second, json!(2)).await;
        replace_private_json(
            &manifest,
            &EndpointManifest {
                endpoint: second.to_str().unwrap().into(),
            },
        )
        .unwrap();
        assert_eq!(client.call("context", json!({})).await.unwrap(), json!(2));
        second_server.await.unwrap();
    }
    #[tokio::test]
    async fn rejects_nested_endpoint_manifests() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("endpoint.json");
        replace_private_json(
            &manifest,
            &EndpointManifest {
                endpoint: "file:/nested.json".into(),
            },
        )
        .unwrap();
        let client = LocalClient::new(ClientConfig {
            endpoint: endpoint_reference(&manifest).unwrap(),
            token: "secret".into(),
        });
        assert_eq!(
            client.call("context", json!({})).await.unwrap_err().code,
            "local_transport"
        );
    }
}

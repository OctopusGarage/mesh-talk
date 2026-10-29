//! Fixed-route, loopback-only HTTP adaptation of scoped integration operations.
use crate::{IntegrationError, IntegrationService};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use serde_json::{Value, json};
use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::JoinSet,
};

const MAX_BODY_BYTES: usize = 64 * 1024;
const MAX_REQUESTS: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// One owner for connection, handler and retirement deadlines. Production defaults
// remain fixed; in-module fixtures can exercise the same lifecycle promptly.
#[derive(Clone, Copy)]
struct HttpDeadlines {
    request: Duration,
}

impl Default for HttpDeadlines {
    fn default() -> Self {
        Self {
            request: REQUEST_TIMEOUT,
        }
    }
}

impl HttpDeadlines {
    fn drain(self) -> Duration {
        self.request + Duration::from_secs(1)
    }
}

/// A numeric-loopback listener with four fixed, scoped operations and no admin routes.
pub struct LocalHttpServer {
    listener: TcpListener,
    host: String,
    pending: Arc<Semaphore>,
    connections: Arc<Semaphore>,
    client_config: Option<PathBuf>,
    deadlines: HttpDeadlines,
}

#[derive(Clone)]
struct HttpState {
    service: Arc<IntegrationService>,
    host: String,
    pending: Arc<Semaphore>,
    client_config: Option<PathBuf>,
    deadlines: HttpDeadlines,
}

impl LocalHttpServer {
    pub async fn bind() -> Result<Self, IntegrationError> {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| local_error())?;
        let host = listener
            .local_addr()
            .map_err(|_| local_error())?
            .to_string();
        Ok(Self {
            listener,
            host,
            pending: Arc::new(Semaphore::new(MAX_REQUESTS)),
            connections: Arc::new(Semaphore::new(MAX_REQUESTS)),
            client_config: None,
            deadlines: HttpDeadlines::default(),
        })
    }

    /// Publish only the credential file location in public discovery, never its contents.
    pub fn with_client_config(mut self, path: PathBuf) -> Self {
        self.client_config = Some(path);
        self
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}", self.host)
    }

    /// Stop accepting connections, then drain requests bounded by their existing deadlines.
    pub async fn run_until(
        self,
        service: Arc<IntegrationService>,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> Result<(), IntegrationError> {
        if self
            .client_config
            .as_ref()
            .is_some_and(|path| !path.is_absolute() || path.to_str().is_none())
        {
            return Err(local_error());
        }
        let state = HttpState {
            service,
            host: self.host,
            pending: self.pending,
            client_config: self.client_config,
            deadlines: self.deadlines,
        };
        let deadlines = self.deadlines;
        let app = Router::new().fallback(handle).with_state(state);
        let (stopping, signal) = watch::channel(false);
        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => break,
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = self.listener.accept() => {
                    let (stream, _) = accepted.map_err(|_| local_error())?;
                    let Ok(permit) = self.connections.clone().try_acquire_owned() else {
                        reject_over_capacity(stream).await;
                        continue;
                    };
                    let app = app.clone();
                    let mut signal = signal.clone();
                    tasks.spawn(async move {
                        let _permit = permit;
                        let mut builder = http1::Builder::new();
                        builder.timer(TokioTimer::new())
                            .header_read_timeout(deadlines.request)
                            .keep_alive(false)
                            .max_buf_size(MAX_BODY_BYTES);
                        let connection = builder.serve_connection(TokioIo::new(stream), TowerToHyperService::new(app));
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = &mut connection => {},
                            _ = signal.changed() => {
                                connection.as_mut().graceful_shutdown();
                                // The outer owner enforces the common drain deadline.
                                let _ = connection.await;
                            }
                        }
                    });
                }
            }
        }
        drop(self.listener);
        let _ = stopping.send(true);
        // Request deadlines have one second to finish their structured timeout response.
        let _ = tokio::time::timeout(deadlines.drain(), async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}

/// Reject excess connections before HTTP parsing without creating another task.
/// The only manually emitted response is this fixed, credential-free admission error.
async fn reject_over_capacity(mut stream: TcpStream) {
    let body = r#"{"error":{"code":"CAPACITY_EXCEEDED","message":"Local agent API connection capacity reached","retryable":false}}"#;
    let response = format!(
        "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = tokio::time::timeout(Duration::from_millis(100), async {
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await?;
        // Drain already-sent input so closing an overloaded socket does not reset
        // an otherwise complete HTTP response. Never parse or retain that input.
        stream.readable().await?;
        let mut discarded = [0_u8; 4096];
        let mut remaining = MAX_BODY_BYTES;
        while remaining > 0 {
            match stream.try_read(&mut discarded) {
                Ok(0) | Err(_) => break,
                Ok(count) => remaining = remaining.saturating_sub(count),
            }
        }
        Ok::<(), std::io::Error>(())
    })
    .await;
}

fn local_error() -> IntegrationError {
    IntegrationError::new("LOCAL_HTTP", "Local agent API transport failed")
}

fn envelope(status: StatusCode, value: Value) -> Response {
    let mut response = (status, Json(value)).into_response();
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    envelope(
        status,
        json!({"error": IntegrationError::new(code, message)}),
    )
}

fn single_header(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

async fn handle(State(state): State<HttpState>, request: Request) -> Response {
    let Ok(_permit) = state.pending.try_acquire() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "CAPACITY_EXCEEDED",
            "Local agent API request capacity reached",
        );
    };
    match tokio::time::timeout(state.deadlines.request, dispatch(&state, request)).await {
        Ok(response) => response,
        Err(_) => error(
            StatusCode::GATEWAY_TIMEOUT,
            "TIMEOUT",
            "Local request timed out; query the send result before retrying",
        ),
    }
}

async fn dispatch(state: &HttpState, request: Request) -> Response {
    let headers = request.headers();
    if single_header(headers, header::HOST) != Some(state.host.as_str())
        || headers.contains_key(header::ORIGIN)
        || headers
            .keys()
            .any(|name| name.as_str().starts_with("sec-fetch-"))
    {
        return error(
            StatusCode::FORBIDDEN,
            "LOCAL_CLIENT_REQUIRED",
            "Use a non-browser client with the exact numeric loopback host",
        );
    }
    // Match the complete path and query; arbitrary RPC forwarding is never available.
    let method = match request.uri().path_and_query().map(|value| value.as_str()) {
        Some("/v1/help") => "help",
        Some("/v1/context") => "context",
        Some("/v1/conversations") => "list_conversations",
        Some("/v1/messages") => "send_message",
        Some("/v1/send-result") => "get_send_result",
        _ => {
            return error(
                StatusCode::NOT_FOUND,
                "NOT_FOUND",
                "Unknown local agent API route",
            );
        }
    };
    let expected_method = if method == "help" {
        Method::GET
    } else {
        Method::POST
    };
    if request.method() != expected_method {
        return error(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "Use the HTTP method declared for this route",
        );
    }
    if method == "help" {
        return envelope(
            StatusCode::OK,
            json!({"result": catalog(state.client_config.as_deref(), state.deadlines)}),
        );
    }
    let Some(token) = single_header(headers, header::AUTHORIZATION)
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "A scoped bearer credential is required",
        );
    };
    if token.len() != 64 || !token.bytes().all(|value| value.is_ascii_hexdigit()) {
        return error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHENTICATED",
            "Invalid scoped bearer credential",
        );
    }
    let content_type =
        single_header(headers, header::CONTENT_TYPE).and_then(|value| value.split(';').next());
    if !content_type.is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
        || headers.contains_key(header::CONTENT_ENCODING)
    {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Content-Type must be application/json without content encoding",
        );
    }
    let token = token.to_owned();
    let bytes = match to_bytes(request.into_body(), MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                StatusCode::PAYLOAD_TOO_LARGE,
                "PAYLOAD_TOO_LARGE",
                "Local request body could not be read within the 65536-byte limit",
            );
        }
    };
    let params: Value = match serde_json::from_slice(&bytes) {
        Ok(Value::Object(object)) => Value::Object(object),
        _ => {
            return error(
                StatusCode::BAD_REQUEST,
                "INVALID_JSON",
                "Request body must be a JSON object",
            );
        }
    };
    match state.service.handle(&token, method, params).await {
        Ok(value) => envelope(StatusCode::OK, json!({"result": value})),
        Err(failure) => {
            let status = match failure.code.as_str() {
                "UNAUTHENTICATED" => StatusCode::UNAUTHORIZED,
                "SCOPE_DENIED" => StatusCode::FORBIDDEN,
                "CAPACITY_EXCEEDED" => StatusCode::SERVICE_UNAVAILABLE,
                "RATE_LIMITED" => StatusCode::TOO_MANY_REQUESTS,
                "PAYLOAD_TOO_LARGE" => StatusCode::PAYLOAD_TOO_LARGE,
                "IDEMPOTENCY_CONFLICT" => StatusCode::CONFLICT,
                "STORAGE_FAILURE" => StatusCode::INTERNAL_SERVER_ERROR,
                _ => StatusCode::BAD_REQUEST,
            };
            envelope(status, json!({"error": failure}))
        }
    }
}

#[derive(schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EmptyParams {}

fn catalog(client_config: Option<&std::path::Path>, deadlines: HttpDeadlines) -> Value {
    let empty_schema = schemars::schema_for!(EmptyParams);
    json!({
        "api_version": 1,
        "authentication": {"scheme":"Bearer", "credential_file":client_config, "instructions":"Read the local credential file privately for endpoint and token. Send Authorization: Bearer <token> on operation requests. Never print, paste, log or disclose the token. Read context first for current granted destinations and runtime limits."},
        "operations": [
            {"name":"context", "method":"POST", "path":"/v1/context", "description":"Read the current account principal, granted destinations and runtime limits; no message history.", "input_schema":empty_schema, "example":{}},
            {"name":"list_conversations", "method":"POST", "path":"/v1/conversations", "description":"Describe only granted conversations when context.can_describe is true.", "input_schema":empty_schema, "example":{}},
            {"name":"send_message", "method":"POST", "path":"/v1/messages", "description":"Submit user-requested text to one exact destination from authenticated context with a stable retry key. Acceptance does not prove recipient delivery.", "input_schema":schemars::schema_for!(crate::SendMessage), "example":{"destination":{"kind":"account","id":"<exact granted account ID from context>"},"text":"Requested update","idempotency_key":"unique-intent-key"}},
            {"name":"get_send_result", "method":"POST", "path":"/v1/send-result", "description":"Check a prior send by its original idempotency key before retrying an uncertain submission.", "input_schema":schemars::schema_for!(crate::model::StatusRequest), "example":{"idempotency_key":"unique-intent-key"}}
        ],
        "limits": {"max_request_bytes":MAX_BODY_BYTES,"max_concurrent_requests":MAX_REQUESTS,"max_connections":MAX_REQUESTS,"header_timeout_ms":deadlines.request.as_millis(),"keep_alive":false,"request_timeout_ms":deadlines.request.as_millis(),"max_text_bytes":crate::MAX_TEXT_BYTES,"content_types":["text"],"read_access":false,"parameters":"POST bodies must be JSON objects with Content-Type: application/json."},
        "retry_semantics":"Use one stable idempotency_key per intended message. On timeout or uncertain submission, query get_send_result first; retries must keep the original key, destination and content. Authenticated context defines the retry window. Accepted submission does not prove delivery.",
        "lifecycle":"The desktop must be running and unlocked with access enabled. Scope, expiry and host session are checked again at admission; revocation takes effect immediately. This public catalog grants no authority.",
        "response_envelope":{"success":{"result":"operation result"},"failure":{"error":{"code":"stable code","message":"safe explanation","retryable":false}}},
        "errors": {
            "UNAUTHENTICATED":"Missing, malformed, expired or revoked credential.",
            "SCOPE_DENIED":"The current grant, destination or host session does not allow this operation.",
            "INVALID_INPUT":"Parameters violate the declared shape or runtime constraints; exact lowercase destination IDs and nonblank text are required.",
            "INVALID_JSON":"The request body must be a JSON object.",
            "IDEMPOTENCY_CONFLICT":"The key was already used for different content; query its result and preserve original retry content.",
            "PAYLOAD_TOO_LARGE":"The request or text exceeds its runtime limit.",
            "CAPACITY_EXCEEDED":"The bounded request capacity is full.",
            "RATE_LIMITED":"The send rate is temporarily exhausted; preserve the original key and retry after the server-specified delay.",
            "TIMEOUT":"Outcome may be uncertain; query the original key before retrying.",
            "LOCAL_CLIENT_REQUIRED":"Use a non-browser client and the exact numeric loopback Host; Origin and all Sec-Fetch-* headers are rejected.",
            "UNSUPPORTED_MEDIA_TYPE":"POST requests require application/json without content encoding.",
            "NOT_FOUND":"This API has only the advertised routes.",
            "METHOD_NOT_ALLOWED":"Use the advertised HTTP method.",
            "DESTINATION_UNAVAILABLE":"The granted destination is not currently available.",
            "STORAGE_FAILURE":"The local operation could not be completed; query status before retrying."
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Destination,
        host::HeadlessHost,
        policy::{GrantRegistry, new_token, now_ms},
    };
    use mesh_talk_core::{discovery::Roster, identity::device::DeviceIdentity, node::Node};
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct Fixture {
        _directory: tempfile::TempDir,
        endpoint: String,
        host: String,
        token: String,
        admin: String,
        destination: Destination,
        account_id: String,
        pending: Arc<Semaphore>,
        connections: Arc<Semaphore>,
        shutdown: tokio::sync::oneshot::Sender<()>,
        task: tokio::task::JoinHandle<Result<(), IntegrationError>>,
    }

    #[test]
    fn production_http_deadlines_remain_thirty_seconds() {
        let policy = HttpDeadlines::default();
        assert_eq!(policy.request, Duration::from_secs(30));
        assert_eq!(policy.drain(), Duration::from_secs(31));
        let help = catalog(None, policy);
        assert_eq!(help["limits"]["header_timeout_ms"], 30_000);
        assert_eq!(help["limits"]["request_timeout_ms"], 30_000);
    }

    async fn fixture() -> Fixture {
        fixture_with_deadline(REQUEST_TIMEOUT).await
    }

    async fn fixture_with_deadline(timeout: Duration) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &directory.path().join("events"),
            &directory.path().join("sent"),
            "pw",
        )
        .unwrap();
        let channel = node.create_channel("Alerts", vec![]).await.unwrap();
        let destination = Destination::Channel {
            id: hex::encode(channel.as_bytes()),
        };
        let registry =
            GrantRegistry::open(&directory.path().join("grants"), "pw", &node.account_id())
                .unwrap();
        let admin = new_token();
        let account_id = node.account_id();
        let service = Arc::new(IntegrationService::new(
            Arc::new(HeadlessHost::new(node)),
            registry,
            &admin,
        ));
        let grant = service.handle(&admin, "grant_create", json!({"client_id":"http-test", "destinations":[destination], "describe":true,"expires_at_ms":now_ms()+60000})).await.unwrap();
        let token = grant["token"].as_str().unwrap().to_owned();
        let mut server = LocalHttpServer::bind()
            .await
            .unwrap()
            .with_client_config(directory.path().join("agent-access.json"));
        server.deadlines = HttpDeadlines { request: timeout };
        let pending = server.pending.clone();
        let connections = server.connections.clone();
        let endpoint = server.endpoint();
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        let host = endpoint.strip_prefix("http://").unwrap().to_owned();
        let (shutdown, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(service, async {
            let _ = stopped.await;
        }));
        Fixture {
            _directory: directory,
            endpoint,
            host,
            token,
            admin,
            destination,
            account_id,
            pending,
            connections,
            shutdown,
            task,
        }
    }

    async fn request(
        fixture: &Fixture,
        method: &str,
        path: &str,
        host: &str,
        token: &str,
        extra: &str,
        body: &str,
    ) -> (u16, Value) {
        let mut stream =
            tokio::net::TcpStream::connect(fixture.endpoint.strip_prefix("http://").unwrap())
                .await
                .unwrap();
        let authorization = if token.is_empty() {
            String::new()
        } else {
            format!("Authorization: Bearer {token}\r\n")
        };
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{authorization}Content-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("cache-control: no-store")
        );
        let status = headers.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap())
    }

    async fn stop(fixture: Fixture) {
        let address = fixture.host.clone();
        fixture.shutdown.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), fixture.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test]
    async fn public_help_discovers_real_schemas_without_private_scope() {
        let f = fixture().await;
        let (status, help) = request(&f, "GET", "/v1/help", &f.host, "", "", "").await;
        assert_eq!(status, 200);
        let catalog = &help["result"];
        assert_eq!(catalog["api_version"], 1);
        assert!(catalog.get("context").is_none());
        assert_eq!(
            catalog["authentication"]["credential_file"],
            json!(f._directory.path().join("agent-access.json"))
        );
        let destination_id = match &f.destination {
            Destination::Account { id } | Destination::Channel { id } => id,
        };
        assert!(!help.to_string().contains(destination_id));
        assert!(!help.to_string().contains(&f.account_id));
        assert_eq!(catalog["operations"].as_array().unwrap().len(), 4);
        let operations = catalog["operations"].as_array().unwrap();
        let send = operations
            .iter()
            .find(|op| op["name"] == "send_message")
            .unwrap();
        assert_eq!(send["method"], "POST");
        assert_eq!(send["path"], "/v1/messages");
        assert_eq!(send["input_schema"]["additionalProperties"], false);
        assert!(
            send["input_schema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("idempotency_key"))
        );
        let context = operations
            .iter()
            .find(|op| op["name"] == "context")
            .unwrap();
        assert_eq!(context["input_schema"]["additionalProperties"], false);
        assert!(!help.to_string().contains(&f.token));
        assert!(!help.to_string().contains(&f.admin));
        assert_eq!(
            request(&f, "GET", "/v1/help", &f.host, &f.admin, "", "")
                .await
                .0,
            200
        );
        assert_eq!(
            request(&f, "GET", "/v1/help", &f.host, "bad", "", "")
                .await
                .0,
            200
        );
        assert_eq!(
            request(
                &f,
                "GET",
                "/v1/help",
                &f.host,
                &f.token,
                "Origin: http://localhost\r\n",
                ""
            )
            .await
            .0,
            403
        );
        stop(f).await;
    }

    #[tokio::test]
    async fn http_routes_delegate_context_conversations_send_and_status() {
        let f = fixture().await;
        let (status, context) =
            request(&f, "POST", "/v1/context", &f.host, &f.token, "", "{}").await;
        assert_eq!(status, 200);
        assert_eq!(context["result"]["destinations"], json!([f.destination]));
        assert_eq!(context["result"]["read_access"], false);
        let (status, conversations) =
            request(&f, "POST", "/v1/conversations", &f.host, &f.token, "", "{}").await;
        assert_eq!(status, 200);
        assert_eq!(
            conversations["result"]["conversations"][0]["name"],
            "Alerts"
        );
        let body =
            json!({"destination":f.destination,"text":"hello HTTP","idempotency_key":"http-once"})
                .to_string();
        let (status, sent) =
            request(&f, "POST", "/v1/messages", &f.host, &f.token, "", &body).await;
        assert_eq!(status, 200);
        assert_eq!(sent["result"]["acceptance"], "accepted");
        let (_, duplicate) =
            request(&f, "POST", "/v1/messages", &f.host, &f.token, "", &body).await;
        assert_eq!(duplicate["result"]["duplicate"], true);
        let (status, receipt) = request(
            &f,
            "POST",
            "/v1/send-result",
            &f.host,
            &f.token,
            "",
            r#"{"idempotency_key":"http-once"}"#,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(receipt["result"]["acceptance"], "accepted");
        stop(f).await;
    }

    #[tokio::test]
    async fn http_rejects_browser_dns_rebinding_admin_and_non_post_requests() {
        let f = fixture().await;
        for (host, extra) in [
            ("localhost:1234", ""),
            (f.host.as_str(), "Origin: http://localhost\r\n"),
            (f.host.as_str(), "Sec-Fetch-Site: none\r\n"),
            (f.host.as_str(), "Sec-Fetch-Mode: navigate\r\n"),
            (f.host.as_str(), "Sec-Fetch-Dest: document\r\n"),
            (f.host.as_str(), "Sec-Fetch-User: ?1\r\n"),
        ] {
            let (status, value) =
                request(&f, "POST", "/v1/context", host, &f.token, extra, "{}").await;
            assert_eq!(status, 403);
            assert!(value.get("error").is_some());
        }
        for path in [
            "/v1/grant_create",
            "/v1/admin_context",
            "/rpc",
            "/v1/context?method=grant_create",
        ] {
            assert_eq!(
                request(&f, "POST", path, &f.host, &f.admin, "", "{}")
                    .await
                    .0,
                404
            );
        }
        assert_eq!(
            request(&f, "GET", "/v1/context", &f.host, &f.token, "", "{}")
                .await
                .0,
            405
        );
        assert_eq!(
            request(&f, "POST", "/v1/context", &f.host, "bad", "", "{}")
                .await
                .0,
            401
        );
        assert!(
            request(&f, "POST", "/v1/context", &f.host, &f.admin, "", "{}")
                .await
                .1
                .get("error")
                .is_some()
        );
        stop(f).await;
    }

    #[tokio::test]
    async fn dropping_the_listener_owner_aborts_its_partial_header_connections() {
        let f = fixture().await;
        let mut stream = tokio::net::TcpStream::connect(&f.host).await.unwrap();
        stream
            .write_all(b"POST /v1/context HTTP/1.1\r\nHost: ")
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.connections.available_permits() != MAX_REQUESTS - 1 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        f.task.abort();
        assert!(f.task.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.connections.available_permits() != MAX_REQUESTS {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), stream.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.is_empty());
        assert!(tokio::net::TcpStream::connect(&f.host).await.is_err());
    }

    #[tokio::test]
    async fn partial_headers_are_bounded_at_admission_and_expire_without_handler_work() {
        // Leave headroom above the existing two-second admission watchdog;
        // exercise expiry without waiting for the production thirty seconds.
        let f = fixture_with_deadline(Duration::from_secs(5)).await;
        let mut streams = Vec::new();
        for _ in 0..MAX_REQUESTS {
            let mut stream = tokio::net::TcpStream::connect(&f.host).await.unwrap();
            stream
                .write_all(b"POST /v1/context HTTP/1.1\r\nHost: ")
                .await
                .unwrap();
            streams.push(stream);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.connections.available_permits() != 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(f.pending.available_permits(), MAX_REQUESTS);
        let (status, response) =
            request(&f, "POST", "/v1/context", &f.host, &f.token, "", "{}").await;
        assert_eq!(status, 503);
        assert_eq!(response["error"]["code"], "CAPACITY_EXCEEDED");
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(8), streams[0].read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(&f.token));
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.connections.available_permits() != MAX_REQUESTS {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            request(&f, "POST", "/v1/context", &f.host, &f.token, "", "{}")
                .await
                .0,
            200
        );
        drop(streams);
        stop(f).await;
    }

    #[tokio::test]
    async fn shutdown_drains_an_outstanding_request_until_its_structured_deadline() {
        // Leave headroom above the existing two-second admission watchdog;
        // exercise expiry without waiting for the production thirty seconds.
        let f = fixture_with_deadline(Duration::from_secs(5)).await;
        let mut stream = tokio::net::TcpStream::connect(&f.host).await.unwrap();
        let headers = format!(
            "POST /v1/context HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{",
            f.host, f.token
        );
        stream.write_all(headers.as_bytes()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.pending.available_permits() != MAX_REQUESTS - 1 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        f.shutdown.send(()).unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(8), stream.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 504 "));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("cache-control: no-store")
        );
        let value: Value = serde_json::from_str(body).unwrap();
        assert_eq!(value["error"]["code"], "TIMEOUT");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("query the send result")
        );
        assert!(!response.contains(&f.token));
        tokio::time::timeout(Duration::from_secs(2), f.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(f.pending.available_permits(), MAX_REQUESTS);
        assert!(tokio::net::TcpStream::connect(&f.host).await.is_err());
    }

    #[tokio::test]
    async fn slow_bodies_consume_bounded_slots_and_excess_requests_fail_structurally() {
        let f = fixture().await;
        let mut connections = Vec::new();
        for _ in 0..MAX_REQUESTS {
            let mut stream = tokio::net::TcpStream::connect(&f.host).await.unwrap();
            let headers = format!(
                "POST /v1/context HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{",
                f.host, f.token
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            connections.push(stream);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while f.pending.available_permits() != 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        let (status, response) =
            request(&f, "POST", "/v1/context", &f.host, &f.token, "", "{}").await;
        assert_eq!(status, 503);
        assert_eq!(response["error"]["code"], "CAPACITY_EXCEEDED");
        drop(connections);
        stop(f).await;
    }

    #[tokio::test]
    async fn http_limits_json_and_preserves_service_errors() {
        let f = fixture().await;
        assert_eq!(
            request(
                &f,
                "POST",
                "/v1/context",
                &f.host,
                &f.token,
                "Content-Type: text/plain\r\n",
                "{}"
            )
            .await
            .0,
            415
        );
        assert_eq!(
            request(
                &f,
                "POST",
                "/v1/context",
                &f.host,
                &f.token,
                "Content-Encoding: gzip\r\n",
                "{}"
            )
            .await
            .0,
            415
        );
        let (status, _) = request(&f, "POST", "/v1/context", &f.host, &f.token, "", "{").await;
        assert_eq!(status, 400);
        let (status, _) = request(
            &f,
            "POST",
            "/v1/context",
            &f.host,
            &f.token,
            "",
            &"x".repeat(65537),
        )
        .await;
        assert_eq!(status, 413);
        let (status, value) =
            request(&f, "POST", "/v1/messages", &f.host, &f.token, "", "{}").await;
        assert_eq!(status, 400);
        assert_eq!(value["error"]["code"], "INVALID_INPUT");
        stop(f).await;
    }
}

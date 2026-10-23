use super::*;
use std::{collections::HashMap, future::Future};
use tokio::{net::TcpStream, sync::oneshot};

const CLIENT_ID: &str = "fake-desktop.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "fake-oauth-client-secret-never-real";
const CODE: &str = "fake-authorization-code+with/slash=";
const TOKEN: &str = "fake-oauth-access-token-never-real";

struct TokenRequest {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    form: HashMap<String, String>,
}

fn query(url: &Url) -> HashMap<String, String> {
    url.query_pairs().into_owned().collect()
}

fn test_client() -> Result<reqwest::Client, CommandError> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap())
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("OAuth fixture did not finish within its bounded deadline")
}

async fn token_reply(listener: TcpListener, status: &str, body: &str) -> TokenRequest {
    let (mut stream, _) = listener.accept().await.unwrap();
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "token request ended before its headers");
        bytes.extend_from_slice(&chunk[..read]);
        assert!(bytes.len() < 16 * 1024, "unbounded token request");
        if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let mut lines = headers.lines();
    let mut request_line = lines.next().unwrap().split_whitespace();
    let method = request_line.next().unwrap().to_string();
    let target = request_line.next().unwrap().to_string();
    let headers: HashMap<_, _> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let content_length: usize = headers["content-length"].parse().unwrap();
    while bytes.len() - header_end < content_length {
        let mut chunk = [0; 1024];
        let read = stream.read(&mut chunk).await.unwrap();
        assert_ne!(read, 0, "token request ended before its form");
        bytes.extend_from_slice(&chunk[..read]);
    }
    let body_form = std::str::from_utf8(&bytes[header_end..header_end + content_length]).unwrap();
    let pairs: Vec<_> = Url::parse(&format!("http://fixture.invalid/?{body_form}"))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    let form: HashMap<_, _> = pairs.iter().cloned().collect();
    assert_eq!(pairs.len(), form.len(), "duplicate token form fields");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await.unwrap();
    TokenRequest {
        method,
        target,
        headers,
        form,
    }
}

async fn callback_bytes(authorization: &Url, bytes: &[u8]) -> String {
    let redirect = Url::parse(&query(authorization)["redirect_uri"]).unwrap();
    let mut stream = TcpStream::connect((redirect.host_str().unwrap(), redirect.port().unwrap()))
        .await
        .unwrap();
    stream.write_all(bytes).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8(response).unwrap()
}

async fn callback_get(authorization: &Url, target: &str) -> String {
    callback_bytes(
        authorization,
        format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes(),
    )
    .await
}

fn valid_target(authorization: &Url) -> String {
    let parameters = query(authorization);
    let mut callback = Url::parse(&parameters["redirect_uri"]).unwrap();
    callback
        .query_pairs_mut()
        .extend_pairs([("state", parameters["state"].as_str()), ("code", CODE)]);
    format!("{}?{}", callback.path(), callback.query().unwrap())
}

async fn exchange(
    status: &str,
    body: &str,
    secret: Option<&str>,
) -> (Result<String, CommandError>, TokenRequest, Url, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let (opened, browser) = oneshot::channel();
    let authorization = authorize_with(
        CLIENT_ID,
        secret,
        test_client,
        &endpoint,
        Duration::from_secs(3),
        |url| {
            opened.send(url.clone()).unwrap();
            Ok(())
        },
    );
    let browser = async {
        let url = browser.await.unwrap();
        let response = callback_get(&url, &valid_target(&url)).await;
        (url, response)
    };
    let (result, request, (url, response)) = bounded(async {
        tokio::join!(authorization, token_reply(listener, status, body), browser)
    })
    .await;
    (result, request, url, response)
}

#[tokio::test]
async fn authorize_binds_pkce_and_redirect_to_the_scoped_token_exchange() {
    let body = format!(r#"{{"access_token":"{TOKEN}","token_type":"bEaReR","scope":"{SCOPE}"}}"#);
    let (result, request, url, callback_response) =
        exchange("200 OK", &body, Some(CLIENT_SECRET)).await;
    assert_eq!(result.unwrap(), TOKEN);
    assert_eq!(
        url.origin().ascii_serialization(),
        "https://accounts.google.com"
    );
    assert_eq!(url.path(), "/o/oauth2/v2/auth");
    let parameters = query(&url);
    assert_eq!(parameters.len(), 8);
    assert_eq!(parameters["client_id"], CLIENT_ID);
    assert_eq!(parameters["scope"], SCOPE);
    assert_eq!(parameters["response_type"], "code");
    assert_eq!(parameters["prompt"], "select_account");
    assert_eq!(parameters["code_challenge_method"], "S256");
    assert!(!parameters.contains_key("access_type"));
    let redirect = Url::parse(&parameters["redirect_uri"]).unwrap();
    assert_eq!(redirect.scheme(), "http");
    assert_eq!(redirect.host_str(), Some("127.0.0.1"));
    assert_eq!(redirect.path(), "/oauth/callback");
    assert!(redirect.port().is_some_and(|port| port != 0));
    assert_eq!(parameters["state"].len(), 43);
    assert_eq!(request.method, "POST");
    assert_eq!(request.target, "/token");
    assert_eq!(
        request.headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    assert!(!request.headers.contains_key("authorization"));
    assert_eq!(request.form.len(), 6);
    assert_eq!(request.form["client_id"], CLIENT_ID);
    assert_eq!(request.form["client_secret"], CLIENT_SECRET);
    assert_eq!(request.form["code"], CODE);
    assert_eq!(request.form["redirect_uri"], parameters["redirect_uri"]);
    assert_eq!(request.form["grant_type"], "authorization_code");
    assert_eq!(request.form["code_verifier"].len(), 43);
    assert_ne!(parameters["state"], request.form["code_verifier"]);
    assert_eq!(
        parameters["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(request.form["code_verifier"].as_bytes()))
    );
    assert!(callback_response.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(callback_response.contains("Cache-Control: no-store\r\n"));
    assert!(callback_response.contains("Content-Security-Policy: default-src 'none'\r\n"));
    assert!(callback_response.ends_with("Google Drive connected. You can return to Mesh-Talk."));
    assert!(!callback_response.contains(CODE));
    assert!(!callback_response.contains(TOKEN));
}

#[tokio::test]
async fn authorize_supports_public_clients_and_an_omitted_token_scope() {
    let body = format!(r#"{{"access_token":"{TOKEN}","token_type":"Bearer"}}"#);
    let (result, request, _, _) = exchange("200 OK", &body, None).await;
    assert_eq!(result.unwrap(), TOKEN);
    assert_eq!(request.form.len(), 5);
    assert!(!request.form.contains_key("client_secret"));
}

#[tokio::test]
async fn authorize_ignores_stale_unrelated_and_unreadable_callbacks() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let body =
        format!(r#"{{"access_token":"{TOKEN}","token_type":"Bearer","scope":"openid {SCOPE}"}}"#);
    let (opened, browser) = oneshot::channel();
    let authorization = authorize_with(
        CLIENT_ID,
        None,
        test_client,
        &endpoint,
        Duration::from_secs(3),
        |url| {
            opened.send(url.clone()).unwrap();
            Ok(())
        },
    );
    let browser = async {
        let url = browser.await.unwrap();
        for target in [
            "/favicon.ico",
            "/oauth/callback?state=fake-stale-state&code=fake-stale-code",
        ] {
            let response = callback_get(&url, target).await;
            assert!(
                response.ends_with("Sign-in was not accepted. Return to Mesh-Talk and try again.")
            );
        }
        let post = format!("POST {} HTTP/1.1\r\n\r\n", valid_target(&url));
        assert!(callback_bytes(&url, post.as_bytes()).await.is_empty());
        assert!(callback_bytes(&url, b"\xff\r\n\r\n").await.is_empty());
        assert!(callback_bytes(&url, b"").await.is_empty());
        let oversized = vec![b'x'; 8193];
        assert!(callback_bytes(&url, &oversized).await.is_empty());
        assert!(
            callback_get(&url, &valid_target(&url))
                .await
                .contains("Google Drive connected.")
        );
    };
    let (result, request, ()) = bounded(async {
        tokio::join!(
            authorization,
            token_reply(listener, "200 OK", &body),
            browser
        )
    })
    .await;
    assert_eq!(result.unwrap(), TOKEN);
    assert_eq!(request.form["code"], CODE);
}

#[tokio::test]
async fn authorize_rejects_denial_or_ambiguous_matching_callbacks_without_exchange() {
    for suffix in [
        "&error=access_denied&error_description=fake-provider-secret",
        "&code=fake-one&code=fake-two",
        "&code=",
        "&code=fake-one&state=fake-extra-state",
        "&code=fake-one#fake-fragment",
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let (opened, browser) = oneshot::channel();
        let authorization = authorize_with(
            CLIENT_ID,
            Some(CLIENT_SECRET),
            test_client,
            &endpoint,
            Duration::from_secs(3),
            |url| {
                opened.send(url.clone()).unwrap();
                Ok(())
            },
        );
        let browser = async {
            let url = browser.await.unwrap();
            let target = format!("/oauth/callback?state={}{}", query(&url)["state"], suffix);
            let reply = callback_get(&url, &target).await;
            assert!(
                reply.ends_with("Sign-in was not accepted. Return to Mesh-Talk and try again.")
            );
        };
        let (result, ()) = bounded(async { tokio::join!(authorization, browser) }).await;
        let error = result.unwrap_err();
        assert!(matches!(error, CommandError::Auth(_)));
        assert_eq!(error.to_string(), auth_error().to_string());
        assert!(!error.to_string().contains("fake-"));
        assert!(
            tokio::time::timeout(Duration::from_millis(10), listener.accept())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn authorize_rejects_invalid_token_grants_without_exposing_credentials() {
    let cases = [
        (
            "200 OK",
            r#"{"access_token":"","token_type":"Bearer"}"#,
            true,
        ),
        (
            "200 OK",
            r#"{"access_token":"fake-leaked-token","token_type":"Basic"}"#,
            true,
        ),
        (
            "200 OK",
            r#"{"access_token":"fake-leaked-token","token_type":"Bearer","scope":"openid"}"#,
            true,
        ),
        (
            "200 OK",
            r#"{"access_token":"fake-leaked-token","token_type":"Bearer","scope":""}"#,
            true,
        ),
        ("200 OK", r#"{"access_token":"fake-leaked-token"}"#, false),
        ("200 OK", "fake-invalid-json-with-provider-secret", false),
        (
            "400 Bad Request",
            r#"{"error":"fake-provider-secret","access_token":"fake-leaked-token"}"#,
            true,
        ),
        ("302 Found", r#"{"error":"fake-provider-secret"}"#, true),
    ];
    for (status, body, authentication_error) in cases {
        let (result, request, _, _) = exchange(status, body, Some(CLIENT_SECRET)).await;
        assert_eq!(request.form["client_secret"], CLIENT_SECRET);
        let error = result.unwrap_err();
        assert_eq!(matches!(error, CommandError::Auth(_)), authentication_error);
        assert!(!error.to_string().contains("fake-"), "{error}");
        assert!(!format!("{error:?}").contains(CLIENT_SECRET));
    }
}

#[tokio::test]
async fn authorize_maps_failed_token_connections_to_a_safe_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    drop(listener);
    let (opened, browser) = oneshot::channel();
    let authorization = authorize_with(
        CLIENT_ID,
        Some(CLIENT_SECRET),
        test_client,
        &endpoint,
        Duration::from_secs(3),
        |url| {
            opened.send(url.clone()).unwrap();
            Ok(())
        },
    );
    let browser = async {
        let url = browser.await.unwrap();
        callback_get(&url, &valid_target(&url)).await;
    };
    let (result, ()) = bounded(async { tokio::join!(authorization, browser) }).await;
    let error = result.unwrap_err();
    assert!(matches!(error, CommandError::Auth(_)));
    assert_eq!(error.to_string(), auth_error().to_string());
}

#[tokio::test]
async fn authorize_releases_the_callback_listener_when_browser_opening_fails() {
    let mut opened = None;
    let result = bounded(authorize_with(
        CLIENT_ID,
        Some(CLIENT_SECRET),
        || panic!("browser failure must not construct a token client"),
        "http://127.0.0.1:1/fake-unused-token-endpoint",
        Duration::from_secs(3),
        |url| {
            opened = Some(url.clone());
            Err(CommandError::Io(
                "Could not open browser for Google sign-in".into(),
            ))
        },
    ))
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, CommandError::Io(_)));
    assert_eq!(
        error.to_string(),
        "Could not open browser for Google sign-in"
    );
    let url = opened.unwrap();
    let redirect = Url::parse(&query(&url)["redirect_uri"]).unwrap();
    assert!(
        TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn authorize_bounds_waiting_for_a_callback() {
    let result = bounded(authorize_with(
        CLIENT_ID,
        None,
        || panic!("callback timeout must not construct a token client"),
        "http://127.0.0.1:1/fake-unused-token-endpoint",
        Duration::from_millis(20),
        |_| Ok(()),
    ))
    .await;
    let error = result.unwrap_err();
    assert!(matches!(error, CommandError::Auth(_)));
    assert_eq!(
        error.to_string(),
        "Google sign-in timed out. Connect Google Drive again."
    );
}

#[tokio::test]
async fn authorize_bounds_an_incomplete_callback_request() {
    let (opened, browser) = oneshot::channel();
    let authorization = authorize_with(
        CLIENT_ID,
        None,
        || panic!("incomplete callback must not construct a token client"),
        "http://127.0.0.1:1/fake-unused-token-endpoint",
        Duration::from_millis(250),
        |url| {
            opened.send(url.clone()).unwrap();
            Ok(())
        },
    );
    let browser = async {
        let url = browser.await.unwrap();
        let redirect = Url::parse(&query(&url)["redirect_uri"]).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", redirect.port().unwrap()))
            .await
            .unwrap();
        stream
            .write_all(b"GET /oauth/callback HTTP/1.1\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.is_empty());
    };
    let (result, ()) = bounded(async { tokio::join!(authorization, browser) }).await;
    assert_eq!(
        result.unwrap_err().to_string(),
        "Google sign-in timed out. Connect Google Drive again."
    );
}

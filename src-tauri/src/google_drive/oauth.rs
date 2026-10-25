//! Browser-based desktop OAuth and refresh; credentials never enter the webview.
use super::client::http_client;
use crate::commands::CommandError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tauri_plugin_opener::OpenerExt;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const SCOPE: &str = "https://www.googleapis.com/auth/drive.appdata";
pub(super) fn random_id() -> String {
    URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>())
}
pub(super) fn configured() -> bool {
    option_env!("MESH_TALK_GOOGLE_CLIENT_ID")
        .is_some_and(|id| id.ends_with(".apps.googleusercontent.com"))
}
fn auth_error() -> CommandError {
    CommandError::Authentication(
        "Google authorization failed or was cancelled. Connect Google Drive again.".into(),
    )
}
fn callback_code(target: &str, state: &str) -> Result<String, CommandError> {
    if !target.starts_with("/oauth/callback?") {
        return Err(auth_error());
    }
    let url = Url::parse(&format!("http://127.0.0.1{target}")).map_err(|_| auth_error())?;
    if url.path() != "/oauth/callback" || url.fragment().is_some() {
        return Err(auth_error());
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs.iter().filter(|(k, _)| k == "state").collect();
    let codes: Vec<_> = pairs.iter().filter(|(k, _)| k == "code").collect();
    if states.len() != 1
        || states[0].1 != state
        || codes.len() != 1
        || codes[0].1.is_empty()
        || pairs.iter().any(|(k, _)| k == "error")
    {
        return Err(auth_error());
    }
    Ok(codes[0].1.to_string())
}
fn authorization_url(client_id: &str, redirect: &str, state: &str, verifier: &str) -> Url {
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut url = Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
    url.query_pairs_mut().extend_pairs([
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("prompt", "consent select_account"),
        ("access_type", "offline"),
    ]);
    url
}
#[derive(serde::Deserialize)]
pub(super) struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: u64,
    token_type: String,
    scope: Option<String>,
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GoogleOAuthTokens([redacted])")
    }
}

fn checked_token(response: TokenResponse) -> Result<TokenResponse, CommandError> {
    if response.access_token.is_empty()
        || response.access_token.len() > 16 * 1024
        || response.expires_in == 0
        || response.expires_in > 86400
        || response
            .refresh_token
            .as_ref()
            .is_some_and(|token| token.is_empty() || token.len() > 16 * 1024)
        || !response.token_type.eq_ignore_ascii_case("bearer")
        || response
            .scope
            .as_ref()
            .is_some_and(|scope| !scope.split_whitespace().any(|s| s == SCOPE))
    {
        return Err(auth_error());
    }
    Ok(response)
}

pub(super) fn client_id() -> &'static str {
    option_env!("MESH_TALK_GOOGLE_CLIENT_ID").unwrap_or_default()
}

pub(super) async fn refresh(refresh_token: &str) -> Result<TokenResponse, CommandError> {
    if !configured() {
        return Err(CommandError::InvalidInput(
            "Google Drive is not configured in this build".into(),
        ));
    }
    refresh_with(
        http_client()?,
        "https://oauth2.googleapis.com/token",
        client_id(),
        option_env!("MESH_TALK_GOOGLE_CLIENT_SECRET"),
        refresh_token,
    )
    .await
}

async fn refresh_with(
    http: reqwest::Client,
    endpoint: &str,
    client_id: &str,
    secret: Option<&str>,
    refresh_token: &str,
) -> Result<TokenResponse, CommandError> {
    let mut form = vec![
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
    ];
    if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
        form.push(("client_secret", secret));
    }
    let response = http.post(endpoint).form(&form).send().await.map_err(|_| {
        CommandError::Io(
            "Could not reconnect to Google Drive. Check your connection and retry.".into(),
        )
    })?;
    if !response.status().is_success() {
        // Only Google's explicit invalid_grant means the saved authorization
        // must be forgotten. Rate limits, timeouts and client configuration
        // errors must not destroy a valid remembered connection.
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            #[derive(serde::Deserialize)]
            struct OAuthFailure {
                error: Option<String>,
            }
            let failure: OAuthFailure = super::client::json_response(response).await?;
            if failure.error.as_deref() == Some("invalid_grant") {
                return Err(CommandError::Authentication(
                    "Google Drive access expired or was revoked. Connect Google Drive again."
                        .into(),
                ));
            }
            return Err(CommandError::InvalidInput(
                "Google Drive could not reconnect. Check this build's Google configuration and retry.".into(),
            ));
        }
        return Err(CommandError::Io(
            "Google Drive is temporarily unavailable. Try again.".into(),
        ));
    }
    checked_token(super::client::json_response(response).await?).map_err(|_| {
        CommandError::Io("Google Drive returned an invalid connection response. Try again.".into())
    })
}

pub(super) async fn authorize<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<TokenResponse, CommandError> {
    let client_id = option_env!("MESH_TALK_GOOGLE_CLIENT_ID")
        .filter(|_| configured())
        .ok_or_else(|| {
            CommandError::InvalidInput("Google Drive is not configured in this build".into())
        })?;
    authorize_with(
        client_id,
        option_env!("MESH_TALK_GOOGLE_CLIENT_SECRET").filter(|value| !value.is_empty()),
        http_client,
        "https://oauth2.googleapis.com/token",
        Duration::from_secs(180),
        |url| {
            app.opener()
                .open_url(url.as_str(), None::<&str>)
                .map_err(|_| CommandError::Io("Could not open browser for Google sign-in".into()))
        },
    )
    .await
}

async fn authorize_with(
    client_id: &str,
    client_secret: Option<&str>,
    make_client: impl FnOnce() -> Result<reqwest::Client, CommandError>,
    token_endpoint: &str,
    callback_timeout: Duration,
    open_browser: impl FnOnce(&Url) -> Result<(), CommandError>,
) -> Result<TokenResponse, CommandError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| CommandError::Io("Could not open Google sign-in callback".into()))?;
    let address = listener.local_addr().map_err(|_| auth_error())?;
    let redirect = format!("http://{address}/oauth/callback");
    let state = random_id();
    let verifier = random_id();
    let url = authorization_url(client_id, &redirect, &state, &verifier);
    open_browser(&url)?;
    let code = tokio::time::timeout(callback_timeout, async {
        loop {
            let (mut stream, peer) = listener.accept().await.map_err(|_| auth_error())?;
            if !peer.ip().is_loopback() { continue; }
            let request = tokio::time::timeout(Duration::from_secs(5), async {
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0; 1024];
                    let n = stream.read(&mut chunk).await.map_err(|_| auth_error())?;
                    if n == 0 { return Err(auth_error()); }
                    bytes.extend_from_slice(&chunk[..n]);
                    if bytes.len() > 8192 { return Err(auth_error()); }
                    if bytes.windows(4).any(|part| part == b"\r\n\r\n") { break; }
                }
                String::from_utf8(bytes).map_err(|_| auth_error())
            }).await;
            let Ok(Ok(request)) = request else { continue; };
            let mut parts = request.lines().next().unwrap_or_default().split_whitespace();
            if parts.next() != Some("GET") { continue; }
            let target = parts.next().unwrap_or_default();
            let result = callback_code(target, &state);
            // Favicon/unrelated or stale callbacks do not consume this attempt.
            let state_matches = Url::parse(&format!("http://127.0.0.1{target}")).is_ok_and(|url| url.query_pairs().any(|(k,v)| k == "state" && v == state));
            let body = if result.is_ok() { "Google Drive connected. You can return to Mesh-Talk." } else { "Sign-in was not accepted. Return to Mesh-Talk and try again." };
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nContent-Security-Policy: default-src 'none'\r\n\r\n{body}", body.len());
            let _ = tokio::time::timeout(Duration::from_secs(2), stream.write_all(reply.as_bytes())).await;
            if state_matches { return result; }
        }
    }).await.map_err(|_| CommandError::Authentication("Google sign-in timed out. Connect Google Drive again.".into()))??;
    let mut form = vec![
        ("client_id", client_id),
        ("code", &code),
        ("code_verifier", &verifier),
        ("redirect_uri", &redirect),
        ("grant_type", "authorization_code"),
    ];
    if let Some(secret) = client_secret {
        form.push(("client_secret", secret));
    }
    let response = make_client()?
        .post(token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|_| auth_error())?;
    if !response.status().is_success() {
        return Err(auth_error());
    }
    checked_token(super::client::json_response(response).await?)
}

#[cfg(test)]
pub(super) async fn refresh_for_test(
    endpoint: &str,
    token: &str,
) -> Result<TokenResponse, CommandError> {
    refresh_with(
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap(),
        endpoint,
        client_id(),
        None,
        token,
    )
    .await
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod flow_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_requires_exact_path_and_state_and_one_code() {
        assert_eq!(
            callback_code("/oauth/callback?state=expected&code=ok", "expected").unwrap(),
            "ok"
        );
        for path in [
            "/other?state=expected&code=ok",
            "/oauth/callback?state=wrong&code=ok",
            "/oauth/callback?code=ok",
            "/oauth/callback?state=expected&code=one&code=two",
            "/oauth/callback?state=expected&error=access_denied",
            "/oauth/callback?state=expected&code=",
        ] {
            assert!(callback_code(path, "expected").is_err(), "{path}");
        }
    }

    #[test]
    fn authorization_requests_only_appdata_with_pkce() {
        let url = authorization_url(
            "desktop.apps.googleusercontent.com",
            "http://127.0.0.1:1234/oauth/callback",
            "state",
            "verifier",
        );
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs["scope"],
            "https://www.googleapis.com/auth/drive.appdata"
        );
        assert_eq!(pairs["code_challenge_method"], "S256");
        assert_eq!(
            pairs["code_challenge"],
            "iMnq5o6zALKXGivsnlom_0F5_WYda32GHkxlV7mq7hQ"
        );
        assert_eq!(pairs["access_type"], "offline");
    }
}

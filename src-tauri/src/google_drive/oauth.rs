//! Browser-based desktop OAuth. Tokens stay in native memory only.
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
        ("prompt", "select_account"),
    ]);
    url
}
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    scope: Option<String>,
}

pub(super) async fn authorize<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> Result<String, CommandError> {
    let client_id = option_env!("MESH_TALK_GOOGLE_CLIENT_ID")
        .filter(|_| configured())
        .ok_or_else(|| {
            CommandError::InvalidInput("Google Drive is not configured in this build".into())
        })?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| CommandError::Io("Could not open Google sign-in callback".into()))?;
    let address = listener.local_addr().map_err(|_| auth_error())?;
    let redirect = format!("http://{address}/oauth/callback");
    let state = random_id();
    let verifier = random_id();
    let url = authorization_url(client_id, &redirect, &state, &verifier);
    app.opener()
        .open_url(url.as_str(), None::<&str>)
        .map_err(|_| CommandError::Io("Could not open browser for Google sign-in".into()))?;
    let code = tokio::time::timeout(Duration::from_secs(180), async {
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
    if let Some(secret) =
        option_env!("MESH_TALK_GOOGLE_CLIENT_SECRET").filter(|value| !value.is_empty())
    {
        form.push(("client_secret", secret));
    }
    let response = http_client()?
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|_| auth_error())?;
    if !response.status().is_success() {
        return Err(auth_error());
    }
    let response: TokenResponse = super::client::json_response(response).await?;
    if response.access_token.is_empty()
        || !response.token_type.eq_ignore_ascii_case("bearer")
        || response
            .scope
            .as_ref()
            .is_some_and(|scope| !scope.split_whitespace().any(|s| s == SCOPE))
    {
        return Err(auth_error());
    }
    Ok(response.access_token)
}

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
        assert!(!pairs.contains_key("access_type"));
    }
}

//! Tauri IPC commands. After the legacy stack was retired this is just the auth surface
//! (login / logout / register) plus the bridge that starts the node on login.
//! All messaging/contact/file/channel commands live in [`crate::chat_commands`].

use crate::services::auth_service::AuthError;
use crate::services::user::User;
use crate::state::AppState;

/// An IPC command error, serialized to the frontend as a tagged `{ kind, message }` object
/// so the UI can branch on the kind (e.g. route an `auth` failure to the login screen, or
/// label a failed message with a meaningful cause) instead of string-matching.
///
/// The `kind` is a stable, granular taxonomy that mirrors what the core can actually
/// distinguish — we only split a failure into a finer kind when the core error variants
/// genuinely tell us the cause; otherwise it stays `internal`. The serialized `kind`
/// strings (hyphenated where the frontend expects it) are:
/// - `peer-unknown`     — recipient/peer not yet discovered / not in roster
/// - `relay-unreachable`— sync transport / post-office / network couldn't carry the op
/// - `crypto`           — seal / ratchet / decrypt / at-rest crypto failure
/// - `auth`             — login / keystore / session authentication
/// - `authorization`    — permission denied
/// - `io`               — file / disk / log I/O failure
/// - `not-started`      — the node runtime isn't running yet
/// - `invalid-input`    — caller-supplied input was rejected
/// - `internal`         — anything not distinguishable as one of the above
#[derive(Debug, serde::Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    #[serde(rename = "peer-unknown")]
    PeerUnknown(String),
    #[serde(rename = "relay-unreachable")]
    RelayUnreachable(String),
    #[serde(rename = "crypto")]
    Crypto(String),
    #[serde(rename = "auth")]
    Auth(String),
    #[serde(rename = "authorization")]
    Authorization(String),
    #[serde(rename = "io")]
    Io(String),
    #[serde(rename = "not-started")]
    NotStarted(String),
    #[serde(rename = "invalid-input")]
    InvalidInput(String),
    #[serde(rename = "internal")]
    Internal(String),
}

impl CommandError {
    /// The node runtime isn't running yet (no session / pre-login).
    pub fn not_started() -> Self {
        CommandError::NotStarted("node not started".into())
    }

    /// Construct a caller-input rejection. Named `Validation` historically; kept as a
    /// thin constructor so the many call sites read unchanged.
    #[allow(non_snake_case)]
    pub fn Validation(msg: String) -> Self {
        CommandError::InvalidInput(msg)
    }

    /// Construct an authentication failure (login/keystore/session).
    #[allow(non_snake_case)]
    pub fn Authentication(msg: String) -> Self {
        CommandError::Auth(msg)
    }

    /// Construct an operational/internal failure. Named `Service` historically.
    #[allow(non_snake_case)]
    pub fn Service(msg: String) -> Self {
        CommandError::Internal(msg)
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::PeerUnknown(msg)
            | CommandError::RelayUnreachable(msg)
            | CommandError::Crypto(msg)
            | CommandError::Auth(msg)
            | CommandError::Authorization(msg)
            | CommandError::Io(msg)
            | CommandError::NotStarted(msg)
            | CommandError::InvalidInput(msg)
            | CommandError::Internal(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for CommandError {}

/// A bare string error is an operational/internal failure by default.
impl From<String> for CommandError {
    fn from(msg: String) -> Self {
        CommandError::Internal(msg)
    }
}

/// Map a node operation failure to the finest kind the core can actually distinguish.
/// Sends, file ops, channel ops and pairing all surface as `NodeError`, so this is the
/// single place that drives the user-visible failure reason for those paths.
impl From<mesh_talk_core::node::NodeError> for CommandError {
    fn from(e: mesh_talk_core::node::NodeError) -> Self {
        use mesh_talk_core::dm::DmError;
        use mesh_talk_core::eventlog::LogError;
        use mesh_talk_core::node::NodeError;

        let msg = e.to_string();
        match e {
            // Recipient not yet discovered / not in the roster.
            NodeError::UnknownPeer(_) => CommandError::PeerUnknown(msg),

            // Sealing/opening the payload failed → crypto (Encrypt/Decrypt); a malformed
            // envelope is an internal serialization bug, not a user-visible crypto cause.
            NodeError::Seal(DmError::Encrypt) | NodeError::Seal(DmError::Decrypt) => {
                CommandError::Crypto(msg)
            }
            NodeError::Seal(DmError::Serialization(_)) => CommandError::Internal(msg),

            // The networked sync session failed. SessionError is crate-private in core, so
            // we can't split its variants here — but a sync session is fundamentally a
            // network op, so a failure is overwhelmingly "couldn't reach the relay/peer".
            NodeError::Session(_) => CommandError::RelayUnreachable(msg),

            // Appending the event locally failed: I/O vs at-rest crypto vs everything else.
            NodeError::Log(LogError::Io(_)) | NodeError::Log(LogError::CorruptFile(_)) => {
                CommandError::Io(msg)
            }
            NodeError::Log(LogError::Storage(_))
            | NodeError::Log(LogError::CorruptId)
            | NodeError::Log(LogError::BadSignature) => CommandError::Crypto(msg),
            NodeError::Log(_) => CommandError::Internal(msg),

            // Channel/File carry only a string from the core; we can't reliably split them
            // further, so they stay internal rather than fabricating a distinction.
            NodeError::Channel(_) | NodeError::File(_) => CommandError::Internal(msg),
        }
    }
}

pub type CommandResult<T> = Result<T, CommandError>;

#[derive(serde::Serialize)]
pub struct UserInfo {
    pub id: String,
    pub username: String,
}

impl From<User> for UserInfo {
    fn from(user: User) -> Self {
        Self {
            id: user.user_id,
            username: user.name,
        }
    }
}

#[derive(serde::Serialize)]
pub struct LoginResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInfo>,
}

#[derive(serde::Serialize)]
pub struct LogoutResult {
    pub success: bool,
}

#[derive(serde::Serialize)]
pub struct RegisterResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<UserInfo>,
}

// ---------------------------------------------------------------------------
// Auth commands
// ---------------------------------------------------------------------------

#[tauri::command]
pub async fn login(
    app_handle: tauri::AppHandle,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<LoginResult, CommandError> {
    let result = login_impl(username, password.clone(), app_state.inner()).await?;

    if result.success {
        // Start the node: per-account stores under ~/.mesh-talk/accounts/<user_id>/.
        // NOTE: the node keystore intentionally uses the RAW `password` here, while the auth
        // keystore uses the trimmed form (auth_service::login trims). They are independent
        // stores; the node's was first created with the raw value, so it must keep using the
        // raw value. Do NOT "unify" these to the trimmed form without a keystore migration —
        // that would break decryption for any user whose password has leading/trailing space.
        if let Some(session) = app_state.session().get() {
            // "Stay signed in": persist the RAW password (the one the node keystore needs)
            // to the OS keychain so the next launch can auto-unlock; otherwise forget any
            // previously-saved secret. Keyed by the trimmed username the user typed.
            let stay = settings_state.get().stay_signed_in;
            let keychain_user = session.user.name.clone();
            if stay {
                crate::session_store::save(&keychain_user, &password);
                crate::settings::record_last_user(
                    &app_handle,
                    settings_state.inner(),
                    Some(keychain_user),
                );
            } else {
                crate::session_store::clear(&keychain_user);
                crate::settings::record_last_user(&app_handle, settings_state.inner(), None);
            }
            spawn_node_runtime(
                app_handle.clone(),
                session.user.user_id.clone(),
                session.user.name.clone(),
                password,
                node_state.inner().clone(),
            );
        }
    }

    Ok(result)
}

/// "Stay signed in" auto-login: if enabled and a username + keychain secret exist, run the
/// SAME path as [`login`] (verify the auth keystore + spawn the node) using the stored
/// credential, returning the `User` so the frontend can skip the login screen. Returns
/// `Ok(None)` (NOT an error) whenever there's nothing to resume — no saved session, or the
/// stored secret is stale — so the frontend falls back to manual login. A stale secret is
/// cleared so it can't keep failing on every launch.
#[tauri::command]
pub async fn auto_login(
    app_handle: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<Option<UserInfo>, CommandError> {
    let settings = settings_state.get();
    if !settings.stay_signed_in {
        return Ok(None);
    }
    let Some(username) = settings.last_user else {
        return Ok(None);
    };
    let Some(password) = crate::session_store::load(&username) else {
        return Ok(None);
    };

    match login_impl(username.clone(), password.clone(), app_state.inner()).await {
        Ok(result) if result.success => {
            if let Some(session) = app_state.session().get() {
                let user_info = UserInfo::from(session.user.clone());
                spawn_node_runtime(
                    app_handle.clone(),
                    session.user.user_id.clone(),
                    session.user.name.clone(),
                    password,
                    node_state.inner().clone(),
                );
                return Ok(Some(user_info));
            }
            Ok(None)
        }
        // Stored secret no longer authenticates (e.g. password changed elsewhere): forget it
        // and fall back to manual login rather than surfacing a hard error on every launch.
        _ => {
            crate::session_store::clear(&username);
            crate::settings::record_last_user(&app_handle, settings_state.inner(), None);
            Ok(None)
        }
    }
}

/// Explicit sign-out hook: forget the saved keychain secret + `last_user` so a subsequent
/// launch does NOT auto-login. Called by the frontend on "Sign out". Best-effort.
#[tauri::command]
pub fn clear_saved_session(
    app_handle: tauri::AppHandle,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<(), CommandError> {
    if let Some(user) = settings_state.get().last_user.as_deref() {
        crate::session_store::clear(user);
    }
    crate::settings::record_last_user(&app_handle, settings_state.inner(), None);
    Ok(())
}

async fn login_impl(
    username: String,
    password: String,
    app_state: &AppState,
) -> CommandResult<LoginResult> {
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err(CommandError::Validation(
            "Username and password are required".into(),
        ));
    }

    let normalized_username = username.trim().to_string();
    let normalized_password = password.trim().to_string();

    // The Argon2 password KDF inside auth_service.login is CPU-bound and would pin a tokio
    // worker for its full duration. Run it on the blocking pool (AuthService is Arc-cloneable);
    // only the cheap session mutation stays on the reactor.
    let auth_service = app_state.auth_service().clone();
    let auth_result = {
        let normalized_username = normalized_username.clone();
        let normalized_password = normalized_password.clone();
        tokio::task::spawn_blocking(move || {
            auth_service.login(normalized_username, normalized_password)
        })
        .await
        .map_err(|e| CommandError::Service(format!("join error: {e}")))?
    };

    match auth_result {
        Ok((user, token)) => {
            app_state
                .session()
                .set(token.clone(), user.clone(), normalized_password);
            Ok(LoginResult {
                success: true,
                token: Some(token),
                user: Some(UserInfo::from(user)),
            })
        }
        Err(AuthError::UserNotFound) | Err(AuthError::InvalidCredentials) => Err(
            CommandError::Authentication("Invalid username or password".into()),
        ),
        Err(AuthError::InvalidInput(msg)) => Err(CommandError::Validation(msg)),
        Err(AuthError::AlreadyLoggedIn) => Err(CommandError::Authentication(
            "A user is already signed in".into(),
        )),
        Err(AuthError::StorageError(msg)) | Err(AuthError::InternalError(msg)) => {
            Err(CommandError::Service(msg))
        }
        Err(other) => Err(CommandError::Service(format!(
            "Failed to login user '{normalized_username}': {other:?}"
        ))),
    }
}

#[tauri::command]
pub async fn logout(
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
) -> Result<LogoutResult, CommandError> {
    // Clear the session FIRST; only stop the node once logout actually succeeded, so an
    // error path (e.g. no session) can't leave the node torn down with the session intact.
    let result = logout_impl(app_state.inner())?;
    // Stop and drop the node runtime (Drop aborts its background tasks).
    node_state.0.lock().await.take();
    Ok(result)
}

fn logout_impl(app_state: &AppState) -> CommandResult<LogoutResult> {
    let session = require_session(app_state)?;
    app_state
        .auth_service()
        .logout(session.token.clone())
        .map_err(|e| CommandError::Service(format!("Failed to logout: {e:?}")))?;
    app_state.session().clear();
    Ok(LogoutResult { success: true })
}

#[tauri::command]
pub async fn register(
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<RegisterResult, CommandError> {
    register_impl(username, password, app_state.inner()).await
}

async fn register_impl(
    username: String,
    password: String,
    app_state: &AppState,
) -> CommandResult<RegisterResult> {
    if username.trim().is_empty() || password.trim().is_empty() {
        return Err(CommandError::Validation(
            "Username and password are required".into(),
        ));
    }

    let normalized_password = password.trim().to_string();
    if normalized_password.len() < 8 {
        return Err(CommandError::Validation(
            "Password must be at least 8 characters".into(),
        ));
    }

    // The Argon2 password KDF inside auth_service.register is CPU-bound; run it on the
    // blocking pool so it doesn't pin a tokio worker (AuthService is Arc-cloneable).
    let auth_service = app_state.auth_service().clone();
    let register_result = tokio::task::spawn_blocking(move || {
        auth_service.register(username, normalized_password, "127.0.0.1:7000".into())
    })
    .await
    .map_err(|e| CommandError::Service(format!("join error: {e}")))?;

    let user = match register_result {
        Ok(user) => user,
        Err(AuthError::UserAlreadyExists) => {
            return Err(CommandError::Validation("Username already exists".into()))
        }
        Err(AuthError::PasswordTooWeak) => {
            return Err(CommandError::Validation(
                "Password must be at least 8 characters".into(),
            ))
        }
        Err(AuthError::InvalidInput(msg)) => return Err(CommandError::Validation(msg)),
        Err(AuthError::StorageError(msg)) | Err(AuthError::InternalError(msg)) => {
            return Err(CommandError::Service(msg))
        }
        Err(other) => {
            return Err(CommandError::Service(format!(
                "Failed to register user: {other:?}"
            )))
        }
    };

    Ok(RegisterResult {
        success: true,
        user: Some(UserInfo::from(user)),
    })
}

fn require_session(state: &AppState) -> CommandResult<crate::state::SessionInfo> {
    state
        .session()
        .get()
        .ok_or_else(|| CommandError::Authentication("User session not found. Please login.".into()))
}

// ---------------------------------------------------------------------------
// Node lifecycle bridge
// ---------------------------------------------------------------------------

/// Spawn the node runtime in the background, wiring its inbound callbacks to the
/// app's Tauri events, and store it in `node_handle`. Shared by login and by account
/// adoption after device linking (which drops the old runtime and re-spawns; `start`
/// reloads the account keystore, so a re-spawn adopts a freshly-linked account secret).
/// `account_id` is the host-app namespace for the data directory — distinct from the
/// node's cryptographic account.
pub(crate) fn spawn_node_runtime(
    app_handle: tauri::AppHandle,
    account_id: String,
    display_name: String,
    password: String,
    node_handle: crate::chat_commands::NodeState,
) {
    let app_handle_for_dm = app_handle.clone();
    let app_handle_for_channel = app_handle.clone();
    let app_handle_for_file = app_handle.clone();
    let app_handle_for_profile = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        // The base directory for node per-account data (the app's `~/.mesh-talk`).
        let base_dir = crate::data_dir();
        match mesh_talk_core::node::NodeRuntime::start(
            &base_dir,
            &account_id,
            &display_name,
            &password,
            mesh_talk_core::node::DEFAULT_DISCOVERY_PORT,
            move |dm| {
                crate::events::emit_dm_received(
                    &app_handle_for_dm,
                    dm.from,
                    dm.from_name,
                    dm.text,
                    dm.reply_to,
                );
            },
            move |msg: mesh_talk_core::node::ReceivedChannelMessage| {
                crate::events::emit_channel_message(
                    &app_handle_for_channel,
                    hex::encode(msg.channel_id.as_bytes()),
                    msg.channel_name,
                    msg.from,
                    msg.text,
                    msg.reply_to,
                );
            },
            move |f: mesh_talk_core::node::ReceivedFile| {
                crate::events::emit_file_received(
                    &app_handle_for_file,
                    hex::encode(f.conv.as_bytes()),
                    f.from,
                    f.name,
                    f.size,
                    f.mime,
                    hex::encode(f.file_conv.as_bytes()),
                    f.media,
                );
            },
            move |p: mesh_talk_core::node::ReceivedProfile| {
                crate::events::emit_profile_received(
                    &app_handle_for_profile,
                    p.account_id,
                    p.avatar,
                );
            },
        )
        .await
        {
            Ok(runtime) => {
                *node_handle.0.lock().await = Some(runtime);
                log::info!("Node started for account {account_id}");
            }
            Err(e) => log::warn!("Node failed to start: {e}"),
        }
    });
}

/// Adopt an account secret just persisted by a successful device link: drop the running
/// node runtime and re-spawn it so it reloads the account keystore (now holding the
/// linked account) and re-advertises under it. Reuses the held session credentials — no
/// re-login required.
#[tauri::command]
pub async fn adopt_linked_account(
    app_handle: tauri::AppHandle,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
) -> Result<(), CommandError> {
    let pw = {
        let guard = node_state.0.lock().await;
        guard
            .as_ref()
            .map(|rt| rt.restart_password().to_string())
            .ok_or_else(CommandError::not_started)?
    };
    let session = app_state
        .session()
        .get()
        .ok_or_else(|| CommandError::Authentication("not logged in".into()))?;
    let account_id = session.user.user_id.clone();
    let display_name = session.user.name.clone();
    node_state.0.lock().await.take();
    spawn_node_runtime(
        app_handle,
        account_id,
        display_name,
        pw,
        node_state.inner().clone(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The frontend (frontend/src/lib/error.ts) hard-depends on this exact shape:
    // `{ "kind": <snake_case>, "message": <string> }`. Pin it so a stray serde attr change
    // breaks the build here rather than silently breaking the UI's error branching.
    #[test]
    fn command_error_serializes_as_tagged_kind_message() {
        let cases = [
            (CommandError::PeerUnknown("p".into()), "peer-unknown"),
            (
                CommandError::RelayUnreachable("r".into()),
                "relay-unreachable",
            ),
            (CommandError::Crypto("c".into()), "crypto"),
            (CommandError::Auth("a".into()), "auth"),
            (
                CommandError::Authorization("denied".into()),
                "authorization",
            ),
            (CommandError::Io("io".into()), "io"),
            (CommandError::NotStarted("ns".into()), "not-started"),
            (CommandError::InvalidInput("bad".into()), "invalid-input"),
            (CommandError::Internal("down".into()), "internal"),
        ];
        for (err, kind) in cases {
            let v = serde_json::to_value(&err).unwrap();
            assert_eq!(v["kind"], kind, "kind tag for {err:?}");
            assert!(v["message"].is_string(), "message is a string for {err:?}");
        }
    }

    // The legacy constructors (Validation/Authentication/Service) keep many call sites
    // unchanged while routing to the new granular kinds.
    #[test]
    fn legacy_constructors_map_to_new_kinds() {
        assert_eq!(
            serde_json::to_value(CommandError::Validation("x".into())).unwrap()["kind"],
            "invalid-input"
        );
        assert_eq!(
            serde_json::to_value(CommandError::Authentication("x".into())).unwrap()["kind"],
            "auth"
        );
        assert_eq!(
            serde_json::to_value(CommandError::Service("x".into())).unwrap()["kind"],
            "internal"
        );
    }
}

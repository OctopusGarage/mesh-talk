//! Tauri IPC commands. After the legacy stack was retired this is just the auth surface
//! (login / logout / register) plus the bridge that starts the node on login.
//! All messaging/contact/file/channel commands live in [`crate::chat_commands`].

use crate::chat_commands::RuntimeAuthority;
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
            NodeError::InvalidInput(_) => CommandError::InvalidInput(msg),
            NodeError::Authorization(_) => CommandError::Authorization(msg),
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
    /// The immutable login handle.
    pub username: String,
    /// The editable, peer-facing display name (nickname). Equals `username` until changed.
    pub display_name: String,
}

impl From<User> for UserInfo {
    fn from(user: User) -> Self {
        Self {
            id: user.user_id,
            username: user.name,
            display_name: user.display_name,
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
pub async fn login<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<LoginResult, CommandError> {
    let outcome = login_impl(username, password.clone(), app_state.inner()).await?;
    let result = outcome.result;

    if result.success {
        // Start the node: per-account stores under ~/.mesh-talk/accounts/<user_id>/.
        // NOTE: the node keystore intentionally uses the RAW `password` here, while the auth
        // keystore uses the trimmed form (auth_service::login trims). They are independent
        // stores; the node's was first created with the raw value, so it must keep using the
        // raw value. Do NOT "unify" these to the trimmed form without a keystore migration —
        // that would break decryption for any user whose password has leading/trailing space.
        {
            // "Stay signed in": persist the RAW password (the one the node keystore needs)
            // to the OS keychain so the next launch can auto-unlock; otherwise forget any
            // previously-saved secret. Keyed by the trimmed username the user typed.
            remember_login(
                &app_handle,
                app_state.inner(),
                settings_state.inner(),
                &outcome.user,
                &outcome.lease,
                &password,
            )?;
            spawn_node_runtime(
                app_handle.clone(),
                outcome.lease,
                password,
                node_state.inner().clone(),
                app_state.session().clone(),
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
pub async fn auto_login<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<Option<UserInfo>, CommandError> {
    let entry_generation = app_state.session().generation();
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
    #[cfg(test)]
    if let Some(entered) = app_state.owner_command_captured.lock().unwrap().as_ref() {
        let _ = entered.send("auto-login-entry");
    }

    #[cfg(test)]
    {
        let gate = app_state.auto_admission_gate.lock().unwrap().take();
        if let Some((entered, release)) = gate {
            let _ = entered.send(());
            let _ = release.await;
        }
    }
    match login_impl_guarded(
        username.clone(),
        password.clone(),
        app_state.inner(),
        Some(entry_generation),
    )
    .await
    {
        Ok(outcome) if outcome.result.success => {
            let user_info = UserInfo::from(outcome.user);
            spawn_node_runtime(
                app_handle.clone(),
                outcome.lease,
                password,
                node_state.inner().clone(),
                app_state.session().clone(),
            );
            Ok(Some(user_info))
        }
        // Stored secret no longer authenticates (e.g. password changed elsewhere): forget it
        // and fall back to manual login rather than surfacing a hard error on every launch.
        _ => {
            let _ = app_state.session().unchanged(entry_generation, || {
                if settings_state.get().last_user.as_deref() != Some(username.as_str()) {
                    return;
                }
                crate::session_store::clear(&username);
                crate::settings::record_last_user(&app_handle, settings_state.inner(), None);
            });
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

struct AuthOutcome {
    result: LoginResult,
    user: User,
    lease: crate::state::SessionLease,
}

fn remember_login<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    app: &AppState,
    settings: &crate::settings::SettingsState,
    user: &User,
    lease: &crate::state::SessionLease,
    raw_password: &str,
) -> CommandResult<()> {
    app.session()
        .matching(lease, |_| {
            if settings.get().stay_signed_in {
                crate::session_store::save(&user.name, raw_password);
                crate::settings::record_last_user(handle, settings, Some(user.name.clone()));
            } else {
                crate::session_store::clear(&user.name);
                crate::settings::record_last_user(handle, settings, None);
            }
        })
        .map_err(CommandError::Authentication)
}

/// Called inside successful logout's auth-operation guard.
fn forget_logged_out<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
    settings: &crate::settings::SettingsState,
    username: &str,
) {
    crate::session_store::clear(username);
    if settings.get().last_user.as_deref() == Some(username) {
        crate::settings::record_last_user(handle, settings, None);
    }
}

async fn login_impl(
    username: String,
    password: String,
    app_state: &AppState,
) -> CommandResult<AuthOutcome> {
    login_impl_guarded(username, password, app_state, None).await
}

async fn login_impl_guarded(
    username: String,
    password: String,
    app_state: &AppState,
    expected_generation: Option<u64>,
) -> CommandResult<AuthOutcome> {
    let auth_operation = app_state.auth_operation.clone().lock_owned().await;
    // Saved credentials describe the session observed at invocation, unlike an
    // explicit manual login. Reauthorize under auth serialization before mutation.
    if expected_generation.is_some_and(|generation| generation != app_state.session().generation())
    {
        return Err(CommandError::Authentication(
            "Saved login superseded".into(),
        ));
    }
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
    let session = app_state.session().clone();
    let auth_result = {
        let normalized_username = normalized_username.clone();
        let normalized_password = normalized_password.clone();
        tokio::task::spawn_blocking(move || {
            // The blocking operation owns auth serialization even if its IPC
            // future is cancelled. Publication belongs to this actual result.
            let _auth = auth_operation;
            let (user, token) =
                auth_service.login(normalized_username, normalized_password.clone())?;
            let lease = session.publish(token.clone(), user.clone(), normalized_password);
            Ok::<_, AuthError>(AuthOutcome {
                user: user.clone(),
                lease,
                result: LoginResult {
                    success: true,
                    token: Some(token),
                    user: Some(UserInfo::from(user)),
                },
            })
        })
        .await
        .map_err(|e| CommandError::Service(format!("join error: {e}")))?
    };

    match auth_result {
        Ok(outcome) => Ok(outcome),
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
pub async fn logout<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    settings_state: tauri::State<'_, crate::settings::SettingsState>,
) -> Result<LogoutResult, CommandError> {
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    let username = app_state
        .session()
        .matching(&lease, |info| info.user.name.clone())
        .map_err(CommandError::Authentication)?;
    #[cfg(test)]
    if let Some(entered) = app_state.owner_command_captured.lock().unwrap().as_ref() {
        let _ = entered.send("logout-entry");
    }
    // Clear the session FIRST; only stop the node once logout actually succeeded, so an
    // error path (e.g. no session) can't leave the node torn down with the session intact.
    let (result, retirement_ticket) = {
        let _auth = app_state.auth_operation.lock().await;
        app_state
            .session()
            .matching(&lease, |_| ())
            .map_err(CommandError::Authentication)?;
        let result = logout_impl(app_state.inner())?;
        let ticket = node_state.next_lifecycle_ticket();
        // Forget exactly the successfully signed-out credential before releasing
        // auth serialization. A replacement login can now remember its own secret.
        forget_logged_out(&app_handle, settings_state.inner(), &username);
        #[cfg(test)]
        if let Some(entered) = app_state.owner_command_captured.lock().unwrap().as_ref() {
            let _ = entered.send("logout-published");
        }
        (result, ticket)
    };
    // The child owns retirement, so cancellation of the host caller cannot drop
    // stop's producer joins or release profile serialization prematurely.
    let node = node_state.inner().clone();
    #[cfg(test)]
    let retirement_gate = app_state.logout_retirement_gate.clone();
    let retirement = tokio::spawn(async move {
        node.retire_if_current(retirement_ticket, async move {
            #[cfg(test)]
            {
                let gate = retirement_gate.lock().unwrap().take();
                if let Some((entered, release)) = gate {
                    let _ = entered.send(());
                    let _ = release.await;
                }
            }
        })
        .await;
    });
    retirement
        .await
        .map_err(|_| CommandError::Service("Runtime retirement failed".into()))?;
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

/// Change the current user's editable display name (nickname). Persists it (verifying the
/// session password), updates the in-memory session, and — if the node is running —
/// re-signs and hot-swaps its announce so peers see the new name within ~2s (an immediate
/// re-announce is fired too). The login `username` is unchanged; only the peer-facing name
/// moves. Returns the updated user.
#[tauri::command]
pub async fn rename_account(
    new_display_name: String,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
) -> Result<UserInfo, CommandError> {
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    #[cfg(test)]
    if let Some(entered) = app_state.owner_command_captured.lock().unwrap().as_ref() {
        let _ = entered.send("rename-entry");
    }
    let auth = app_state.auth_operation.clone().lock_owned().await;
    let session = app_state
        .session()
        .matching(&lease, |info| info.clone())
        .map_err(CommandError::Authentication)?;

    // Persist to the identity store. The Argon2 password verify inside is CPU-bound, so
    // run it on the blocking pool (AuthService is Arc-cloneable).
    let auth_service = app_state.auth_service().clone();
    let password = session.password.clone();
    let name_for_store = new_display_name.clone();
    let session_state = app_state.session().clone();
    let rename_lease = lease.clone();
    let updated = tokio::task::spawn_blocking(move || {
        let _auth = auth;
        let updated = auth_service.set_display_name(&password, &name_for_store)?;
        session_state
            .matching(&rename_lease, |info| {
                info.user.display_name = updated.display_name.clone()
            })
            .map_err(|_| AuthError::NotLoggedIn)?;
        Ok::<_, AuthError>(updated)
    })
    .await
    .map_err(|e| CommandError::Service(format!("join error: {e}")))?
    .map_err(|e| match e {
        AuthError::InvalidInput(msg) => CommandError::Validation(msg),
        AuthError::InvalidCredentials => CommandError::Authentication("Invalid password".into()),
        AuthError::NotLoggedIn => {
            CommandError::Authentication("User session not found. Please login.".into())
        }
        AuthError::StorageError(msg) | AuthError::InternalError(msg) => CommandError::Service(msg),
        other => CommandError::Service(format!("Failed to rename: {other:?}")),
    })?;

    // Auth serialization was released by the joined persistence/publication
    // operation before waiting for the runtime lifecycle lock.
    #[cfg(test)]
    if let Some(entered) = app_state.owner_command_captured.lock().unwrap().as_ref() {
        let _ = entered.send("rename-published");
    }

    // Hot-swap the live node's announce if it's running. A no-op before the node starts —
    // the new name is persisted and gets advertised on next login regardless.
    {
        #[cfg(test)]
        {
            let gate = app_state.rename_hot_gate.lock().unwrap().take();
            if let Some((entered, release)) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
        }
        let mut guard = node_state.0.lock().await;
        app_state
            .session()
            .matching(&lease, |info| {
                if node_state.check_installation(&lease).is_err() {
                    return;
                }
                if let Some(rt) = guard
                    .as_mut()
                    .filter(|rt| rt.host_account_id() == Some(lease.owner()))
                {
                    rt.set_display_name(&info.user.display_name);
                }
            })
            .map_err(CommandError::Authentication)?;
    }

    Ok(UserInfo::from(updated))
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

/// Startup errors can embed the account directory. Redact every occurrence at
/// the logging boundary while preserving the error category and remaining context.
fn node_start_error_for_log(
    error: &mesh_talk_core::node::RuntimeError,
    account_id: &str,
) -> String {
    let diagnostic = error.to_string();
    if account_id.is_empty() {
        diagnostic
    } else {
        diagnostic.replace(account_id, "[account]")
    }
}

/// Spawn the node runtime in the background, wiring its inbound callbacks to the
/// app's Tauri events, and store it in `node_handle`. Shared by login and by account
/// adoption after device linking (which drops the old runtime and re-spawns; `start`
/// reloads the account keystore, so a re-spawn adopts a freshly-linked account secret).
/// `account_id` is the host-app namespace for the data directory — distinct from the
/// node's cryptographic account.
pub(crate) fn spawn_node_runtime<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    lease: crate::state::SessionLease,
    password: String,
    node_handle: crate::chat_commands::NodeState,
    session: crate::state::SessionState,
) {
    let Ok(authority) = RuntimeAuthority::request(session, lease, node_handle.clone()) else {
        return;
    };
    let dm_authority = authority.clone();
    let channel_authority = authority.clone();
    let file_authority = authority.clone();
    let profile_authority = authority.clone();
    let call_authority = authority.clone();
    let app_handle_for_dm = app_handle.clone();
    let app_handle_for_channel = app_handle.clone();
    let app_handle_for_file = app_handle.clone();
    let app_handle_for_profile = app_handle.clone();
    let app_handle_for_call = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        let Ok(permit) = authority.begin().await else {
            return;
        };
        let display_name = permit.display_name.clone();
        let account_id = authority.lease.owner().to_owned();
        // The base directory for node per-account data (the app's `~/.mesh-talk`).
        #[cfg(test)]
        let (base_dir, discovery_port) = node_handle
            .4
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| (crate::data_dir(), crate::configured_discovery_port()));
        #[cfg(not(test))]
        let (base_dir, discovery_port) = (crate::data_dir(), crate::configured_discovery_port());
        match mesh_talk_core::node::NodeRuntime::start_configured_guarded(
            mesh_talk_core::node::RuntimeConfig {
                base_dir: &base_dir,
                account_id: &account_id,
                display_name: &display_name,
                password: &password,
                discovery_port,
            },
            mesh_talk_core::node::RuntimeEvents {
                on_dm: Box::new(move |dm| {
                    let _ = dm_authority.current(|_| {
                        crate::events::emit_dm_received(
                            &app_handle_for_dm,
                            dm.from,
                            dm.from_name,
                            dm.text,
                            dm.reply_to,
                        );
                    });
                }),
                on_channel: Box::new(move |msg: mesh_talk_core::node::ReceivedChannelMessage| {
                    let _ = channel_authority.current(|_| {
                        crate::events::emit_channel_message(
                            &app_handle_for_channel,
                            hex::encode(msg.channel_id.as_bytes()),
                            msg.channel_name,
                            msg.from,
                            msg.text,
                            msg.reply_to,
                        );
                    });
                }),
                on_file: Box::new(move |f: mesh_talk_core::node::ReceivedFile| {
                    let _ = file_authority.current(|_| {
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
                    });
                }),
                on_profile: Box::new(move |p: mesh_talk_core::node::ReceivedProfile| {
                    let _ = profile_authority.current(|_| {
                        crate::events::emit_profile_received(
                            &app_handle_for_profile,
                            p.account_id,
                            p.avatar,
                        );
                    });
                }),
                on_call_signal: Box::new(move |s: mesh_talk_core::node::ReceivedCallSignal| {
                    let _ = call_authority.current(|_| {
                        crate::events::emit_call_signal(&app_handle_for_call, s.from, s.payload);
                    });
                }),
            },
            |launch| {
                authority
                    .current(|info| launch(&info.user.display_name))
                    .map_err(|_| {
                        mesh_talk_core::node::RuntimeError::Io(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "session replaced",
                        ))
                    })
            },
        )
        .await
        {
            Ok(runtime) => {
                if !permit.install(runtime).await {
                    return;
                }
                // Keep lifecycle diagnostics without persisting an account identifier.
                log::info!("Node started");
            }
            Err(e) => log::warn!(
                "Node failed to start: {}",
                node_start_error_for_log(&e, &account_id)
            ),
        }
    });
}

/// Adopt an account secret just persisted by a successful device link: drop the running
/// node runtime and re-spawn it so it reloads the account keystore (now holding the
/// linked account) and re-advertises under it. Reuses the held session credentials — no
/// re-login required.
#[tauri::command]
pub async fn adopt_linked_account<R: tauri::Runtime>(
    app_handle: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
) -> Result<(), CommandError> {
    let lease = app_state
        .session()
        .capture()
        .map_err(CommandError::Authentication)?;
    let pw = {
        let guard = node_state.0.lock().await;
        app_state
            .session()
            .matching(&lease, |_| {
                node_state.check_installation(&lease)?;
                guard
                    .as_ref()
                    .filter(|rt| rt.host_account_id() == Some(lease.owner()))
                    .map(|rt| rt.restart_password().to_string())
                    .ok_or_else(CommandError::not_started)
            })
            .map_err(CommandError::Authentication)??
    };
    spawn_node_runtime(
        app_handle,
        lease,
        pw,
        node_state.inner().clone(),
        app_state.session().clone(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use tauri::Manager;
    struct AuthFixture {
        root: tempfile::TempDir,
        app_state: AppState,
        node: crate::chat_commands::NodeState,
        settings: crate::settings::SettingsState,
        webview: tauri::WebviewWindow<tauri::test::MockRuntime>,
        alice: String,
        bob: String,
    }

    impl AuthFixture {
        fn new(label: &str) -> Self {
            let root = tempfile::tempdir().unwrap();
            let auth = crate::services::auth_service::AuthService::new(std::sync::Arc::new(
                mesh_talk_core::identity::manager::IdentityManager::new(
                    mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
                ),
            ));
            let alice = format!("owneripc-{label}-alice");
            let bob = format!("owneripc-{label}-bob");
            auth.register(alice.clone(), "password-a".into(), "fixture".into())
                .unwrap();
            auth.register(bob.clone(), "password-b".into(), "fixture".into())
                .unwrap();
            let app_state = AppState::new(auth);
            let signed_in = tauri::async_runtime::block_on(login_impl(
                alice.clone(),
                " password-a ".into(),
                &app_state,
            ))
            .unwrap();
            let node = crate::chat_commands::NodeState::empty();
            let runtime = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
                root.path(),
                signed_in.lease.owner(),
                "Alice",
                " password-a ",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            ))
            .unwrap();
            *node.0.blocking_lock() = Some(runtime);
            let settings =
                crate::settings::SettingsState::isolated(root.path().join("settings.json"));
            let mut value = settings.get();
            value.stay_signed_in = true;
            settings.set(value);
            let app = tauri::test::mock_builder()
                .manage(app_state.clone())
                .manage(node.clone())
                .manage(settings.clone())
                .invoke_handler(tauri::generate_handler![
                    login,
                    auto_login,
                    adopt_linked_account,
                    rename_account,
                    logout,
                    crate::owner_commands::owner_node_identity
                ])
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .build()
                .unwrap();
            *node.4.lock().unwrap() = Some((root.path().to_owned(), 0));
            remember_login(
                webview.app_handle(),
                &app_state,
                &settings,
                &signed_in.user,
                &signed_in.lease,
                " password-a ",
            )
            .unwrap();
            Self {
                root,
                app_state,
                node,
                settings,
                webview,
                alice,
                bob,
            }
        }

        fn login_bob(&self) -> AuthOutcome {
            tauri::async_runtime::block_on(login_impl(
                self.bob.clone(),
                "password-b".into(),
                &self.app_state,
            ))
            .unwrap()
        }

        fn wait_phase(receiver: &std::sync::mpsc::Receiver<&'static str>, phase: &str) {
            loop {
                if receiver
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap()
                    == phase
                {
                    return;
                }
            }
        }
    }

    impl Drop for AuthFixture {
        fn drop(&mut self) {
            let _startup = self.node.test_startup_gate().blocking_lock();
            let old = self.node.0.blocking_lock().take();
            if let Some(old) = old {
                tauri::async_runtime::block_on(old.stop());
            }
            crate::session_store::clear(&self.alice);
            crate::session_store::clear(&self.bob);
        }
    }

    fn invoke_auth(
        webview: &tauri::WebviewWindow<tauri::test::MockRuntime>,
        cmd: &str,
        body: serde_json::Value,
    ) -> Result<serde_json::Value, serde_json::Value> {
        tauri::test::get_ipc_response(
            webview,
            tauri::webview::InvokeRequest {
                cmd: cmd.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|body| body.deserialize::<serde_json::Value>().unwrap())
    }

    #[test]
    fn stale_login_credential_result_and_original_logout_cleanup_preserve_new_owner_metadata() {
        let fixture = AuthFixture::new("metadata");
        let stale = fixture.app_state.session().capture().unwrap();
        let stale_user = fixture.app_state.session().get().unwrap().user;
        let raw = fixture
            .node
            .0
            .blocking_lock()
            .as_ref()
            .unwrap()
            .restart_password()
            .to_string();
        assert_eq!(raw, " password-a ");
        assert_eq!(
            fixture.app_state.session().get().unwrap().password,
            "password-a"
        );
        {
            let _auth = fixture.app_state.auth_operation.blocking_lock();
            logout_impl(&fixture.app_state).unwrap();
            forget_logged_out(
                fixture.webview.app_handle(),
                &fixture.settings,
                &fixture.alice,
            );
        }
        let bob = fixture.login_bob();
        remember_login(
            fixture.webview.app_handle(),
            &fixture.app_state,
            &fixture.settings,
            &bob.user,
            &bob.lease,
            " password-b ",
        )
        .unwrap();
        assert!(remember_login(
            fixture.webview.app_handle(),
            &fixture.app_state,
            &fixture.settings,
            &stale_user,
            &stale,
            "old wrong password"
        )
        .is_err());
        // Production forget helper is tested against a later B snapshot too;
        // the actual logout command executes it under auth serialization.
        forget_logged_out(
            fixture.webview.app_handle(),
            &fixture.settings,
            &fixture.alice,
        );
        assert_eq!(
            crate::session_store::load(&fixture.bob).as_deref(),
            Some(" password-b ")
        );
        assert_eq!(
            fixture.settings.get().last_user.as_deref(),
            Some(fixture.bob.as_str())
        );
        let persisted: serde_json::Value = serde_json::from_slice(
            &std::fs::read(fixture.root.path().join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(persisted["last_user"], fixture.bob);
        // Credential provider is cfg(test) memory-backed, while auth service,
        // session generation, settings state + isolated JSON and runtime are real.
    }

    #[test]
    fn registered_login_and_auto_login_publish_actual_owner_and_preserve_raw_runtime_password() {
        let fixture = AuthFixture::new("login");
        let old_device = fixture
            .node
            .0
            .blocking_lock()
            .as_ref()
            .unwrap()
            .user_id()
            .to_owned();
        let result = invoke_auth(
            &fixture.webview,
            "login",
            serde_json::json!({"username":fixture.alice, "password":" password-a "}),
        )
        .unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["user"]["username"], fixture.alice);
        let wait_installed = || {
            tauri::async_runtime::block_on(async {
                let lease = fixture.app_state.session().capture().unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(5), async {
                    loop {
                        let installed = fixture.node.test_installed_for(&lease);
                        if installed {
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .unwrap();
            })
        };
        wait_installed();
        let identity = invoke_auth(
            &fixture.webview,
            "owner_node_identity",
            serde_json::json!({"owner": result["user"]["id"]}),
        )
        .unwrap();
        assert_eq!(identity["device_id"], old_device);
        assert_eq!(
            fixture
                .node
                .0
                .blocking_lock()
                .as_ref()
                .unwrap()
                .restart_password(),
            " password-a "
        );
        // A newly authenticated generation cannot adopt the previous installed
        // generation's restart password/profile merely because UUID is unchanged.
        tauri::async_runtime::block_on(login_impl(
            fixture.alice.clone(),
            " password-a ".into(),
            &fixture.app_state,
        ))
        .unwrap();
        assert!(invoke_auth(
            &fixture.webview,
            "adopt_linked_account",
            serde_json::json!({})
        )
        .is_err());
        assert_eq!(
            crate::session_store::load(&fixture.alice).as_deref(),
            Some(" password-a ")
        );
        let automatic = invoke_auth(&fixture.webview, "auto_login", serde_json::json!({})).unwrap();
        assert_eq!(automatic["username"], fixture.alice);
        wait_installed();
        assert_eq!(
            fixture
                .node
                .0
                .blocking_lock()
                .as_ref()
                .unwrap()
                .restart_password(),
            " password-a "
        );
    }

    #[test]
    fn registered_stale_auto_login_success_cannot_resurrect_logout() {
        let fixture = AuthFixture::new("autoresurrection");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *fixture.app_state.auto_admission_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let webview = fixture.webview.clone();
        let automatic =
            std::thread::spawn(move || invoke_auth(&webview, "auto_login", serde_json::json!({})));
        tauri::async_runtime::block_on(entered_rx).unwrap();
        invoke_auth(&fixture.webview, "logout", serde_json::json!({})).unwrap();
        let ticket = fixture.node.test_lifecycle_ticket();
        release_tx.send(()).unwrap();
        let result = automatic.join().unwrap().unwrap();
        assert_eq!(result, serde_json::Value::Null);
        assert!(fixture.app_state.session().get().is_none());
        assert!(crate::session_store::load(&fixture.alice).is_none());
        assert!(fixture.settings.get().last_user.is_none());
        assert!(fixture.node.0.blocking_lock().is_none());
        assert_eq!(fixture.node.test_lifecycle_ticket(), ticket);
    }

    #[test]
    fn registered_stale_auto_login_failure_cannot_forget_replacement_owner() {
        let fixture = AuthFixture::new("autofailure");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
        let auth = fixture.app_state.auth_operation.blocking_lock();
        let webview = fixture.webview.clone();
        let automatic =
            std::thread::spawn(move || invoke_auth(&webview, "auto_login", serde_json::json!({})));
        AuthFixture::wait_phase(&entered_rx, "auto-login-entry");
        logout_impl(&fixture.app_state).unwrap();
        let (user, token) = fixture
            .app_state
            .auth_service()
            .login(fixture.bob.clone(), "password-b".into())
            .unwrap();
        let lease = fixture
            .app_state
            .session()
            .publish(token, user.clone(), "password-b".into());
        remember_login(
            fixture.webview.app_handle(),
            &fixture.app_state,
            &fixture.settings,
            &user,
            &lease,
            "password-b",
        )
        .unwrap();
        drop(auth);
        assert_eq!(automatic.join().unwrap().unwrap(), serde_json::Value::Null);
        assert_eq!(
            crate::session_store::load(&fixture.bob).as_deref(),
            Some("password-b")
        );
        assert_eq!(
            fixture.settings.get().last_user.as_deref(),
            Some(fixture.bob.as_str())
        );
    }

    #[test]
    fn registered_genuine_stale_auto_login_secret_is_forgotten_without_a_session_change() {
        let fixture = AuthFixture::new("stalesecret");
        {
            let _auth = fixture.app_state.auth_operation.blocking_lock();
            logout_impl(&fixture.app_state).unwrap();
        }
        crate::session_store::save(&fixture.alice, "wrong-password");
        assert_eq!(
            invoke_auth(&fixture.webview, "auto_login", serde_json::json!({})).unwrap(),
            serde_json::Value::Null
        );
        assert!(fixture.settings.get().last_user.is_none());
        assert!(crate::session_store::load(&fixture.alice).is_none());
    }

    #[test]
    fn aborted_host_logout_retains_retirement_serialization() {
        // Private host boundary gate, not a blocked core writer. Core retirement
        // tests separately prove stop joins actual blocking producers.
        let fixture = AuthFixture::new("abortlogout");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *fixture.app_state.logout_retirement_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let handle = fixture.webview.app_handle().clone();
        let caller = tauri::async_runtime::spawn(async move {
            logout(
                handle.clone(),
                handle.state(),
                handle.state(),
                handle.state(),
            )
            .await
        });
        tauri::async_runtime::block_on(entered_rx).unwrap();
        assert!(fixture.node.0.blocking_lock().is_none());
        caller.abort();
        assert!(tauri::async_runtime::block_on(caller).is_err());
        let signed_in = tauri::async_runtime::block_on(login_impl(
            fixture.alice.clone(),
            "password-a".into(),
            &fixture.app_state,
        ))
        .unwrap();
        let authority = RuntimeAuthority::request(
            fixture.app_state.session().clone(),
            signed_in.lease.clone(),
            fixture.node.clone(),
        )
        .unwrap();
        let serialization_retained = fixture.node.test_startup_gate().try_lock().is_err();
        let replacement = tauri::async_runtime::spawn(async move { authority.begin().await });
        let _ = release_tx.send(());
        let permit = tauri::async_runtime::block_on(replacement)
            .unwrap()
            .unwrap();
        // Only after retirement completes may the actual same-owner profile open.
        let reopened = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            fixture.root.path(),
            signed_in.lease.owner(),
            "Alice",
            " password-a ",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        assert!(tauri::async_runtime::block_on(permit.install(reopened)));
        assert!(
            serialization_retained,
            "aborted caller released startup serialization before retirement completed"
        );
    }

    #[test]
    fn registered_concurrent_rename_uses_latest_published_name() {
        let fixture = AuthFixture::new("renameorder");
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *fixture.app_state.rename_hot_gate.lock().unwrap() = Some((entered_tx, release_rx));
        let webview = fixture.webview.clone();
        let first = std::thread::spawn(move || {
            invoke_auth(
                &webview,
                "rename_account",
                serde_json::json!({"newDisplayName":"First name"}),
            )
        });
        tauri::async_runtime::block_on(entered_rx).unwrap();
        invoke_auth(
            &fixture.webview,
            "rename_account",
            serde_json::json!({"newDisplayName":"Latest name"}),
        )
        .unwrap();
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
        assert_eq!(
            fixture.app_state.session().get().unwrap().user.display_name,
            "Latest name"
        );
        let identities = mesh_talk_core::identity::manager::IdentityManager::new(
            mesh_talk_core::storage::file_manager::FileManager::new(fixture.root.path().to_owned()),
        );
        assert_eq!(
            identities
                .authenticate_user(&fixture.alice, "password-a")
                .unwrap()
                .effective_display_name(),
            "Latest name"
        );
        assert_eq!(
            fixture
                .node
                .0
                .blocking_lock()
                .as_ref()
                .unwrap()
                .display_name(),
            "Latest name"
        );
    }

    #[test]
    fn registered_delayed_rename_cannot_hot_rename_a_replacement_runtime() {
        let fixture = AuthFixture::new("rename");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
        let mut lifecycle = fixture.node.0.blocking_lock();
        let webview = fixture.webview.clone();
        let rename = std::thread::spawn(move || {
            invoke_auth(
                &webview,
                "rename_account",
                serde_json::json!({"newDisplayName":"Alice renamed"}),
            )
        });
        AuthFixture::wait_phase(&entered_rx, "rename-published");
        assert_eq!(
            fixture.app_state.session().get().unwrap().user.display_name,
            "Alice renamed"
        );
        {
            let _auth = fixture.app_state.auth_operation.blocking_lock();
            logout_impl(&fixture.app_state).unwrap();
        }
        let bob = fixture.login_bob();
        let old = lifecycle.take().unwrap();
        tauri::async_runtime::block_on(old.stop());
        let replacement = tauri::async_runtime::block_on(mesh_talk_core::node::NodeRuntime::start(
            fixture.root.path(),
            bob.lease.owner(),
            "Bob",
            "password-b",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        ))
        .unwrap();
        *lifecycle = Some(replacement);
        drop(lifecycle);
        assert!(rename.join().unwrap().is_err());
        assert_eq!(
            fixture.app_state.session().get().unwrap().user.display_name,
            fixture.bob
        );
        assert_eq!(
            fixture
                .node
                .0
                .blocking_lock()
                .as_ref()
                .unwrap()
                .display_name(),
            "Bob"
        );
        let identities = mesh_talk_core::identity::manager::IdentityManager::new(
            mesh_talk_core::storage::file_manager::FileManager::new(fixture.root.path().to_owned()),
        );
        assert_eq!(
            identities
                .authenticate_user(&fixture.alice, "password-a")
                .unwrap()
                .effective_display_name(),
            "Alice renamed"
        );
    }

    #[test]
    fn registered_queued_logout_rejects_entry_generation_and_preserves_new_saved_login() {
        let fixture = AuthFixture::new("queuedlogout");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
        let auth = fixture.app_state.auth_operation.blocking_lock();
        let webview = fixture.webview.clone();
        let logout =
            std::thread::spawn(move || invoke_auth(&webview, "logout", serde_json::json!({})));
        AuthFixture::wait_phase(&entered_rx, "logout-entry");
        logout_impl(&fixture.app_state).unwrap();
        // Already hold the same auth mutex; publish a real B service result
        // synchronously to create an exact queued-command generation boundary.
        let (user, token) = fixture
            .app_state
            .auth_service()
            .login(fixture.bob.clone(), "password-b".into())
            .unwrap();
        let lease = fixture
            .app_state
            .session()
            .publish(token, user.clone(), "password-b".into());
        remember_login(
            fixture.webview.app_handle(),
            &fixture.app_state,
            &fixture.settings,
            &user,
            &lease,
            "password-b",
        )
        .unwrap();
        drop(auth);
        assert!(logout.join().unwrap().is_err());
        assert_eq!(
            crate::session_store::load(&fixture.bob).as_deref(),
            Some("password-b")
        );
        assert_eq!(
            fixture.settings.get().last_user.as_deref(),
            Some(fixture.bob.as_str())
        );
        assert!(
            fixture.node.0.blocking_lock().is_some(),
            "stale logout must not retire any runtime"
        );
    }

    #[test]
    fn registered_successful_logout_forgets_original_login_before_awaiting_runtime_retirement() {
        let fixture = AuthFixture::new("logout");
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        *fixture.app_state.owner_command_captured.lock().unwrap() = Some(entered_tx);
        let startup = fixture.node.test_startup_gate().blocking_lock();
        let webview = fixture.webview.clone();
        let logout =
            std::thread::spawn(move || invoke_auth(&webview, "logout", serde_json::json!({})));
        AuthFixture::wait_phase(&entered_rx, "logout-published");
        assert!(fixture.app_state.session().get().is_none());
        assert!(crate::session_store::load(&fixture.alice).is_none());
        assert!(fixture.settings.get().last_user.is_none());
        let bob = fixture.login_bob();
        remember_login(
            fixture.webview.app_handle(),
            &fixture.app_state,
            &fixture.settings,
            &bob.user,
            &bob.lease,
            "password-b",
        )
        .unwrap();
        let _new_start = RuntimeAuthority::request(
            fixture.app_state.session().clone(),
            bob.lease,
            fixture.node.clone(),
        )
        .unwrap();
        drop(startup);
        assert_eq!(
            logout.join().unwrap().unwrap(),
            serde_json::json!({"success":true})
        );
        assert_eq!(
            crate::session_store::load(&fixture.bob).as_deref(),
            Some("password-b")
        );
        assert_eq!(
            fixture.settings.get().last_user.as_deref(),
            Some(fixture.bob.as_str())
        );
    }
    fn fixture_user(owner: &str) -> User {
        User {
            user_id: owner.into(),
            name: owner.into(),
            display_name: owner.into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        }
    }

    #[test]
    fn stale_start_request_cannot_supersede_current_owner_or_callbacks() {
        let session = crate::state::SessionState::default();
        let node = crate::chat_commands::NodeState::empty();
        let old = session.publish("a".into(), fixture_user("alice"), "pw".into());
        let current = session.publish("b".into(), fixture_user("bob"), "pw".into());
        let valid = RuntimeAuthority::request(session.clone(), current, node.clone()).unwrap();
        let ticket = valid.ticket;
        assert!(RuntimeAuthority::request(session.clone(), old, node.clone()).is_err());
        assert_eq!(node.test_lifecycle_ticket(), ticket);
        let latest = RuntimeAuthority::request(session, valid.lease.clone(), node).unwrap();
        let mut callbacks = 0;
        for _ in 0..5 {
            assert!(valid.current(|_| callbacks += 1).is_err());
            latest.current(|_| callbacks += 1).unwrap();
        }
        assert_eq!(callbacks, 5);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn actual_prelaunch_guard_rejects_session_replacement_without_launching_producers() {
        for replacement_owner in ["bob", "alice"] {
            let root = tempfile::tempdir().unwrap();
            let session = crate::state::SessionState::default();
            let node = crate::chat_commands::NodeState::empty();
            let lease = session.publish("a".into(), fixture_user("alice"), "pw".into());
            let authority =
                RuntimeAuthority::request(session.clone(), lease, node.clone()).unwrap();
            let permit = authority.begin().await.unwrap();
            let (entered_tx, entered_rx) = std::sync::mpsc::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            let directory = root.path().to_owned();
            let prepare = tokio::spawn(async move {
                mesh_talk_core::node::NodeRuntime::start_guarded(
                    &directory,
                    "alice",
                    "captured old name",
                    "pw",
                    0,
                    |_| {},
                    |_| {},
                    |_| {},
                    |_| {},
                    |_| {},
                    move |launch| {
                        entered_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                        authority
                            .current(|info| launch(&info.user.display_name))
                            .map_err(|_| {
                                mesh_talk_core::node::RuntimeError::Io(std::io::Error::from(
                                    std::io::ErrorKind::PermissionDenied,
                                ))
                            })
                    },
                )
                .await
            });
            tokio::task::spawn_blocking(move || {
                entered_rx
                    // Native node preparation can exceed five seconds on a busy
                    // Windows runner. Wait for the actual launch boundary, while
                    // retaining a finite bound for a genuine startup deadlock.
                    .recv_timeout(std::time::Duration::from_secs(30))
                    .unwrap()
            })
            .await
            .unwrap();
            if replacement_owner == "alice" {
                // Supersede during the actual prepared initializer while keeping
                // the same Session owner AND generation, so only the ticket rejects it.
                RuntimeAuthority::request(
                    session.clone(),
                    session.capture().unwrap(),
                    node.clone(),
                )
                .unwrap();
            } else {
                session.publish("b".into(), fixture_user("bob"), "pw".into());
            }
            release_tx.send(()).unwrap();
            assert!(prepare.await.unwrap().is_err());
            assert!(node.0.lock().await.is_none());
            drop(permit);
            assert!(node.test_startup_gate().try_lock().is_ok());
        }
    }

    #[tokio::test]
    async fn actual_preinstall_rejection_stops_runtime_before_releasing_startup_serialization() {
        let root = tempfile::tempdir().unwrap();
        let session = crate::state::SessionState::default();
        let node = crate::chat_commands::NodeState::empty();
        let lease = session.publish("a".into(), fixture_user("alice"), "pw".into());
        let authority = RuntimeAuthority::request(session.clone(), lease, node.clone()).unwrap();
        let permit = authority.begin().await.unwrap();
        let runtime = mesh_talk_core::node::NodeRuntime::start_guarded(
            root.path(),
            "alice",
            "old",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |launch| {
                authority
                    .current(|info| launch(&info.user.display_name))
                    .map_err(|_| {
                        mesh_talk_core::node::RuntimeError::Io(std::io::Error::from(
                            std::io::ErrorKind::PermissionDenied,
                        ))
                    })
            },
        )
        .await
        .unwrap();
        let port = runtime.listen_tcp_port();
        let guard = node.0.lock().await;
        let (queued_tx, queued_rx) = tokio::sync::oneshot::channel();
        let install = tokio::spawn(async move {
            queued_tx.send(()).unwrap();
            permit.install(runtime).await
        });
        queued_rx.await.unwrap();
        assert!(node.test_startup_gate().try_lock().is_err());
        session.clear();
        session.publish("new-a".into(), fixture_user("alice"), "pw".into());
        drop(guard);
        assert!(!install.await.unwrap());
        assert!(node.test_startup_gate().try_lock().is_ok());
        assert!(node.0.lock().await.is_none());
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn replacement_begin_awaits_real_host_stop_and_latest_same_owner_ticket_wins() {
        let root = tempfile::tempdir().unwrap();
        let session = crate::state::SessionState::default();
        let node = crate::chat_commands::NodeState::empty();
        let lease = session.publish("a".into(), fixture_user("alice"), "pw".into());
        let old = mesh_talk_core::node::NodeRuntime::start(
            root.path(),
            "alice",
            "old",
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
        )
        .await
        .unwrap();
        let port = old.listen_tcp_port();
        *node.0.lock().await = Some(old);
        let stale =
            RuntimeAuthority::request(session.clone(), lease.clone(), node.clone()).unwrap();
        let latest = RuntimeAuthority::request(session.clone(), lease, node.clone()).unwrap();
        assert!(stale.begin().await.is_err());
        assert!(node.0.lock().await.is_some());
        let permit = latest.begin().await.unwrap();
        // Actual production coordinator + actual NodeRuntime::stop. Core retirement
        // tests independently prove blocked writers drain inside that awaited stop.
        assert!(node.0.lock().await.is_none());
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
        session.set_display_name("renamed during preparation".into());
        let runtime = mesh_talk_core::node::NodeRuntime::start_guarded(
            root.path(),
            "alice",
            &permit.display_name,
            "pw",
            0,
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |_| {},
            |launch| {
                latest
                    .current(|info| launch(&info.user.display_name))
                    .map_err(|_| {
                        mesh_talk_core::node::RuntimeError::Io(std::io::Error::from(
                            std::io::ErrorKind::PermissionDenied,
                        ))
                    })
            },
        )
        .await
        .unwrap();
        assert_eq!(runtime.display_name(), "renamed during preparation");
        session.set_display_name("renamed before install".into());
        assert!(permit.install(runtime).await);
        assert_eq!(
            node.0.lock().await.as_ref().unwrap().display_name(),
            "renamed before install"
        );
        let runtime = node.0.lock().await.take().unwrap();
        runtime.stop().await;
    }
    #[test]
    fn startup_error_redacts_account_paths_but_keeps_failure_context() {
        use mesh_talk_core::node::RuntimeError;
        use mesh_talk_core::storage::errors::StorageError;

        let account_id = "d86128de-1a5a-4449-b5cc-fb3c82aa49f8";
        let error = RuntimeError::Open(
            StorageError::DirectoryCreationFailed(
                format!("/data/accounts/{account_id}/nested/{account_id}").into(),
            )
            .into(),
        );
        let diagnostic = super::node_start_error_for_log(&error, account_id);
        assert!(!diagnostic.contains(account_id));
        assert!(diagnostic.contains("Failed to create directory"));
        assert!(diagnostic.contains("/data/accounts/[account]/nested/[account]"));
    }

    #[test]
    fn startup_error_preserves_unrelated_errors_and_handles_an_empty_identifier() {
        let error = mesh_talk_core::node::RuntimeError::Io(std::io::Error::from(
            std::io::ErrorKind::AddrInUse,
        ));
        assert_eq!(
            super::node_start_error_for_log(&error, "an-account"),
            error.to_string()
        );
        assert_eq!(
            super::node_start_error_for_log(&error, ""),
            error.to_string()
        );
    }

    #[test]
    fn startup_error_redacts_account_identifiers_in_windows_io_messages() {
        let account_id = "d86128de-1a5a-4449-b5cc-fb3c82aa49f8";
        let error = mesh_talk_core::node::RuntimeError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(r"access denied: C:\data\accounts\{account_id}\identity.keystore"),
        ));
        let diagnostic = super::node_start_error_for_log(&error, account_id);
        assert!(!diagnostic.contains(account_id));
        assert!(diagnostic.contains("access denied"));
        assert!(diagnostic.contains(r"C:\data\accounts\[account]\identity.keystore"));
    }

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

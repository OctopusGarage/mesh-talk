use crate::api::AppConfig;
use crate::domain::models::{ChatMessage, Contact, MessageStatus, User};
use crate::events::{emit_contact_added, emit_network_status_changed, emit_node_port_changed};
use crate::services::auth_service::AuthError;
use crate::services::node_service::{NodeService, NOTIFICATION_SERVICE};
use crate::state::{AppState, SessionInfo};
// use crate::utils::error_handling::{map_mesh_talk_error_to_command_error, validation_error, authentication_error, service_error, network_error, ResultExt};

use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Represents possible errors that can occur in command execution
#[derive(Debug)]
pub enum CommandError {
    /// Input validation errors
    Validation(String),
    /// Authentication-related errors
    Authentication(String),
    /// Authorization-related errors
    Authorization(String),
    /// Service-level errors
    Service(String),
    /// Network-related errors
    Network(String),
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommandError::Validation(msg)
            | CommandError::Authentication(msg)
            | CommandError::Authorization(msg)
            | CommandError::Service(msg)
            | CommandError::Network(msg) => write!(f, "{}", msg),
        }
    }
}

impl std::error::Error for CommandError {}

/// Type alias for command results
type CommandResult<T> = Result<T, CommandError>;

/// Requires an active user session, returning an error if none exists
fn require_session(state: &AppState) -> CommandResult<SessionInfo> {
    state
        .session()
        .get()
        .ok_or_else(|| CommandError::Authentication("User session not found. Please login.".into()))
}

/// Sends a message to connected peers
#[tauri::command]
pub async fn send_message(
    content: String,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    app_state: tauri::State<'_, AppState>,
) -> Result<ChatMessageInfo, String> {
    send_message_impl(content, node_service.inner(), app_state.inner())
        .await
        .map(ChatMessageInfo::from)
        .map_err(|e| e.to_string())
}

/// Internal implementation for sending a message with proper error handling
async fn send_message_impl(
    content: String,
    node_service: &Arc<Mutex<NodeService>>,
    app_state: &AppState,
) -> CommandResult<ChatMessage> {
    let trimmed = content.trim();

    if trimmed.is_empty() {
        return Err(CommandError::Validation(
            "Message content cannot be empty".into(),
        ));
    }

    let session = require_session(app_state)?;

    let message = app_state
        .message_service()
        .create_message(
            session.user.id,
            session.user.address.clone(),
            None,
            None,
            trimmed.to_string(),
        )
        .map_err(|e| CommandError::Service(format!("Failed to create message: {e:?}")))?;

    let service = node_service.lock().await;
    let broadcast_result = service.broadcast_message(trimmed.to_string()).await;
    drop(service);

    match broadcast_result {
        Ok(_) => {
            app_state
                .message_service()
                .mark_delivered(message.id)
                .map_err(|e| CommandError::Service(format!("Failed to record delivery: {e:?}")))?;
            let updated = app_state
                .message_service()
                .get_message(message.id)
                .map_err(|e| CommandError::Service(format!("Failed to reload message: {e:?}")))?;
            Ok(updated)
        }
        Err(err) => {
            let _ = app_state.message_service().mark_failed(message.id);
            Err(CommandError::Network(format!(
                "Failed to broadcast message: {err}"
            )))
        }
    }
}

/// Gets information about the current node
#[tauri::command]
pub async fn get_node_info(
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    app_state: tauri::State<'_, AppState>,
) -> Result<NodeInfo, String> {
    let service = node_service.inner().lock().await;

    // Get logged-in user information if available
    let username = app_state
        .session()
        .get()
        .map(|session| session.user.name.clone());

    // Determine status based on whether user is logged in
    let status = if username.is_some() {
        "Online".to_string()
    } else {
        "Offline".to_string()
    };

    // Get peer count
    let peer_count = {
        let peers = service.node.peers.lock().unwrap();
        peers.len() as u32
    };

    // Get local IP address
    let ip = get_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());
    let port = service.get_port();
    let node_name = service.get_name();
    let username_display = username.clone().unwrap_or_else(|| "Guest".to_string());
    let display_label = format!("{} • {} • {}:{}", node_name, username_display, ip, port);

    Ok(NodeInfo {
        name: node_name,
        port,
        username,
        status,
        peer_count,
        ip,
        display_label,
    })
}

/// Gets the local IP address of the machine
fn get_local_ip() -> Option<String> {
    // Try to infer the outward-facing interface without performing a full TCP handshake.
    if let Ok(socket) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(local_addr) = socket.local_addr() {
                let ip = local_addr.ip();
                if !ip.is_unspecified() {
                    return Some(ip.to_string());
                }
            }
        }
    }

    // Fallback to localhost when we cannot determine a better address.
    Some("127.0.0.1".to_string())
}

/// Logs in a user with the provided credentials
#[tauri::command]
pub async fn login(
    app_handle: tauri::AppHandle,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<LoginResult, String> {
    let result = login_impl(username, password, app_state.inner()).map_err(|e| e.to_string())?;

    if result.success {
        let app_handle_clone = app_handle.clone();
        let node_service_clone = node_service.inner().clone();
        let app_state_clone = app_state.inner().clone();

        tauri::async_runtime::spawn(async move {
            {
                let mut service = node_service_clone.lock().await;
                if service.get_name().trim().is_empty() || service.get_name() == "mesh-node" {
                    let generated_name = crate::api::default_node_name();
                    service.update_name(generated_name);
                }
            }

            if let Err(err) =
                start_network_impl(app_handle_clone, node_service_clone, app_state_clone).await
            {
                eprintln!("Failed to start network runtime: {}", err);
            }
        });
    }

    Ok(result)
}

/// Starts the network runtime
#[tauri::command]
pub async fn start_network(
    app_handle: tauri::AppHandle,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    app_state: tauri::State<'_, AppState>,
) -> Result<NetworkStartResult, String> {
    start_network_impl(
        app_handle,
        node_service.inner().clone(),
        app_state.inner().clone(),
    )
    .await
    .map_err(|e| e.to_string())
}

/// Internal implementation for starting the network runtime
async fn start_network_impl(
    app_handle: tauri::AppHandle,
    node_service: Arc<Mutex<NodeService>>,
    app_state: AppState,
) -> CommandResult<NetworkStartResult> {
    if app_state.session().get().is_none() {
        return Err(CommandError::Authentication(
            "User session not found. Please login.".into(),
        ));
    }

    if !app_state.try_start_network() {
        let port = {
            let service = node_service.lock().await;
            service.get_port()
        };
        return Ok(NetworkStartResult {
            success: true,
            port: Some(port),
        });
    }

    let node_service_for_launch = Arc::clone(&node_service);
    let app_state_snapshot = app_state.clone();
    let app_handle_clone = app_handle.clone();
    let session_username = app_state
        .session()
        .get()
        .map(|session| session.user.name.clone());

    // Set the user ID on the node service
    if let Some(session) = app_state.session().get() {
        let user_id = session.user.id;
        {
            let service = node_service.lock().await;
            service.set_user_id(user_id);
        }
    }

    match crate::launch_network_with_broadcast(node_service_for_launch, true, session_username)
        .await
    {
        Ok(startup) => {
            let actual_port = startup.port;
            app_state_snapshot.set_network_runtime(startup.runtime);
            println!(
                "Network runtime started (TCP port: {}, UDP broadcast: enabled)",
                actual_port
            );

            let runtime_name = {
                let service = node_service.lock().await;
                service.get_name()
            };

            if let Err(err) = AppConfig::persist_runtime_config(&runtime_name, actual_port) {
                eprintln!("Failed to persist runtime configuration: {}", err);
            }

            let current_ip = get_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());
            emit_node_port_changed(&app_handle_clone, actual_port, current_ip.clone());
            emit_network_status_changed(&app_handle_clone, "online".to_string(), 0);

            Ok(NetworkStartResult {
                success: true,
                port: Some(actual_port),
            })
        }
        Err(err) => {
            eprintln!("Failed to launch networking stack: {}", err);
            app_state_snapshot.reset_network_started();
            Err(CommandError::Network(
                crate::user_friendly_errors::format_any_error(&err),
            ))
        }
    }
}

/// Stops the network runtime
#[tauri::command]
pub async fn stop_network(app_state: tauri::State<'_, AppState>) -> Result<(), String> {
    stop_network_impl(app_state.inner()).map_err(|e| e.to_string())
}

/// Internal implementation for stopping the network runtime
fn stop_network_impl(app_state: &AppState) -> CommandResult<()> {
    if let Some(runtime) = app_state.take_network_runtime() {
        println!("Stopping network runtime");
        runtime.shutdown();
    }
    app_state.reset_network_started();
    Ok(())
}

#[tauri::command]
pub async fn connect_to_node(
    address: String,
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
    app_state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    connect_to_node_impl(address, node_service.inner(), app_state.inner())
        .await
        .map_err(|e| e.to_string())
}

async fn connect_to_node_impl(
    address: String,
    node_service: &Arc<Mutex<NodeService>>,
    app_state: &AppState,
) -> CommandResult<()> {
    let _session = require_session(app_state)?;

    let socket_addr: SocketAddr = address
        .parse()
        .map_err(|e| CommandError::Validation(format!("Invalid address '{address}': {e}")))?;

    let service = node_service.lock().await;
    service
        .connect_to_peer(socket_addr)
        .await
        .map_err(|e| CommandError::Network(format!("Failed to connect to {socket_addr}: {e}")))
}

fn login_impl(
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

    match app_state
        .auth_service()
        .login(normalized_username.clone(), normalized_password)
    {
        Ok((user, token)) => {
            app_state.session().set(token.clone(), user.clone());
            refresh_unread_count(app_state, user.id)?;
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
pub async fn logout(app_state: tauri::State<'_, AppState>) -> Result<LogoutResult, String> {
    logout_impl(app_state.inner()).map_err(|e| e.to_string())
}

fn logout_impl(app_state: &AppState) -> CommandResult<LogoutResult> {
    let session = require_session(app_state)?;

    app_state
        .auth_service()
        .logout(session.token.clone())
        .map_err(|e| CommandError::Service(format!("Failed to logout: {e:?}")))?;

    app_state.session().clear();
    stop_network_impl(app_state)?;
    NOTIFICATION_SERVICE.reset_unread_count();

    Ok(LogoutResult { success: true })
}

#[tauri::command]
pub async fn register(
    username: String,
    password: String,
    app_state: tauri::State<'_, AppState>,
) -> Result<RegisterResult, String> {
    register_impl(username, password, app_state.inner()).map_err(|e| e.to_string())
}

fn register_impl(
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

    let user = match app_state.auth_service().register(
        username.clone(),
        normalized_password,
        "127.0.0.1:7000".into(),
    ) {
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
        user: Some(UserInfo::new(user)),
    })
}

#[tauri::command]
pub async fn get_contacts(app_state: tauri::State<'_, AppState>) -> Result<ContactResult, String> {
    get_contacts_impl(app_state.inner()).map_err(|e| e.to_string())
}

fn get_contacts_impl(app_state: &AppState) -> CommandResult<ContactResult> {
    let session = require_session(app_state)?;
    let mut contacts = app_state.contact_service().get_contacts(session.user.id);
    contacts.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(ContactResult {
        success: true,
        contact: None,
        contacts: contacts.into_iter().map(ContactInfo::from).collect(),
    })
}

#[tauri::command]
pub async fn get_messages(
    app_state: tauri::State<'_, AppState>,
) -> Result<Vec<ChatMessageInfo>, String> {
    get_messages_impl(app_state.inner())
        .map(|messages| messages.into_iter().map(ChatMessageInfo::from).collect())
        .map_err(|e| e.to_string())
}

fn get_messages_impl(app_state: &AppState) -> CommandResult<Vec<ChatMessage>> {
    let session = require_session(app_state)?;

    let messages = app_state.message_service().get_all_messages();
    let filtered = messages
        .into_iter()
        .filter(|message| {
            message.from_user_id == session.user.id
                || message
                    .to_user_id
                    .map(|id| id == session.user.id)
                    .unwrap_or(true)
        })
        .collect();
    Ok(filtered)
}

#[tauri::command]
pub async fn mark_message_read(
    message_id: u64,
    app_state: tauri::State<'_, AppState>,
) -> Result<ChatMessageInfo, String> {
    mark_message_read_impl(message_id, app_state.inner())
        .map(ChatMessageInfo::from)
        .map_err(|e| e.to_string())
}

fn mark_message_read_impl(message_id: u64, app_state: &AppState) -> CommandResult<ChatMessage> {
    let session = require_session(app_state)?;

    let message = app_state
        .message_service()
        .get_message(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to load message: {e:?}")))?;

    ensure_message_access(&message, &session)?;

    app_state
        .message_service()
        .mark_read(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to mark message read: {e:?}")))?;

    let updated = app_state
        .message_service()
        .get_message(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to reload message: {e:?}")))?;

    refresh_unread_count(app_state, session.user.id)?;

    Ok(updated)
}

#[tauri::command]
pub async fn mark_message_delivered(
    message_id: u64,
    app_state: tauri::State<'_, AppState>,
) -> Result<ChatMessageInfo, String> {
    mark_message_delivered_impl(message_id, app_state.inner())
        .map(ChatMessageInfo::from)
        .map_err(|e| e.to_string())
}

fn mark_message_delivered_impl(
    message_id: u64,
    app_state: &AppState,
) -> CommandResult<ChatMessage> {
    let session = require_session(app_state)?;

    let message = app_state
        .message_service()
        .get_message(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to load message: {e:?}")))?;

    ensure_message_access(&message, &session)?;

    app_state
        .message_service()
        .mark_delivered(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to mark message delivered: {e:?}")))?;

    app_state
        .message_service()
        .get_message(message_id)
        .map_err(|e| CommandError::Service(format!("Failed to reload message: {e:?}")))
}

#[tauri::command]
pub async fn mark_all_messages_read(app_state: tauri::State<'_, AppState>) -> Result<(), String> {
    mark_all_messages_read_impl(app_state.inner()).map_err(|e| e.to_string())
}

fn mark_all_messages_read_impl(app_state: &AppState) -> CommandResult<()> {
    let session = require_session(app_state)?;

    app_state
        .message_service()
        .mark_all_read_for_user(session.user.id)
        .map_err(|e| CommandError::Service(format!("Failed to mark messages read: {e:?}")))?;

    refresh_unread_count(app_state, session.user.id)
}

fn ensure_message_access(message: &ChatMessage, session: &SessionInfo) -> CommandResult<()> {
    let is_sender = message.from_user_id == session.user.id;
    let is_recipient = message
        .to_user_id
        .map(|id| id == session.user.id)
        .unwrap_or(true);

    if is_sender || is_recipient {
        Ok(())
    } else {
        Err(CommandError::Authorization(
            "You do not have permission to modify this message".into(),
        ))
    }
}

fn refresh_unread_count(app_state: &AppState, user_id: u64) -> CommandResult<()> {
    let unread = app_state
        .message_service()
        .count_unread_for_user(user_id)
        .map_err(|e| CommandError::Service(format!("Failed to count unread messages: {e:?}")))?;

    NOTIFICATION_SERVICE.set_unread_count(unread);
    Ok(())
}

#[derive(serde::Serialize)]
pub struct NodeInfo {
    pub name: String,
    pub port: u16,
    pub username: Option<String>, // Logged-in username if available
    pub status: String,           // "Online" or "Offline"
    pub peer_count: u32,          // Number of connected peers
    pub ip: String,               // Local IP address
    pub display_label: String,    // Combined label with name, user, and endpoint
}

#[derive(serde::Serialize, Clone)]
pub struct UserInfo {
    pub id: u32,
    pub username: String,
}

impl From<User> for UserInfo {
    fn from(user: User) -> Self {
        Self {
            id: user.id as u32,
            username: user.name.clone(),
        }
    }
}

impl UserInfo {
    fn new(user: User) -> Self {
        Self {
            id: user.id as u32,
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

#[derive(serde::Serialize)]
pub struct NetworkStartResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

#[derive(serde::Serialize)]
pub struct ContactInfo {
    pub id: u32,
    pub name: String,
    pub address: String,
    pub status: String,
    pub is_online: bool,
    pub added_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl From<Contact> for ContactInfo {
    fn from(contact: Contact) -> Self {
        Self {
            id: contact.id as u32,
            name: contact.name,
            address: contact.address,
            status: if contact.is_online {
                "online".into()
            } else {
                "offline".into()
            },
            is_online: contact.is_online,
            added_at: contact.added_at,
            notes: contact.notes,
        }
    }
}

#[derive(serde::Serialize)]
pub struct ContactResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<ContactInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub contacts: Vec<ContactInfo>,
}

#[derive(Clone, serde::Serialize)]
pub struct ChatMessageInfo {
    pub id: u64,
    pub from_user_id: u64,
    pub from_address: String,
    pub to_user_id: Option<u64>,
    pub to_address: Option<String>,
    pub content: String,
    pub sent_at: u64,
    pub delivered_at: Option<u64>,
    pub read_at: Option<u64>,
    pub status: u8,
}

impl From<ChatMessage> for ChatMessageInfo {
    fn from(message: ChatMessage) -> Self {
        Self {
            id: message.id,
            from_user_id: message.from_user_id,
            from_address: message.from_address,
            to_user_id: message.to_user_id,
            to_address: message.to_address,
            content: message.content,
            sent_at: message.sent_at,
            delivered_at: message.delivered_at,
            read_at: message.read_at,
            status: match message.status {
                MessageStatus::Sent => 0,
                MessageStatus::Delivered => 1,
                MessageStatus::Read => 2,
                MessageStatus::Failed => 3,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::auth_service::AuthService;
    use crate::services::contact_service::ContactService;
    use crate::services::message_service::MessageService;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn unique_test_path() -> String {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        let counter = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        format!("./test_data_{}_{}", ts, counter)
    }

    struct TestHarness {
        app_state: AppState,
        node_service: Arc<Mutex<NodeService>>,
    }

    impl TestHarness {
        fn new() -> Self {
            // Create a temporary directory for test data
            let test_data_path = unique_test_path();

            // Initialize file manager
            let file_manager =
                crate::storage::file_manager::FileManager::new(test_data_path.into());

            // Initialize services with file manager
            let auth_service = AuthService::new(Arc::new(
                crate::identity::manager::IdentityManager::new(file_manager.clone()),
            ));
            let contact_manager = Arc::new(crate::contacts::manager::ContactManager::new(
                file_manager.clone(),
            ));
            let contact_service = ContactService::new(contact_manager);
            let message_service = MessageService::new(Arc::new(file_manager));

            let app_state = AppState::new(
                auth_service,
                contact_service,
                message_service,
                Arc::new(crate::api::AppConfig::default()),
            );
            let node_service = Arc::new(Mutex::new(NodeService::new("test-node".into(), 0)));

            Self {
                app_state,
                node_service,
            }
        }
    }

    impl Drop for TestHarness {
        fn drop(&mut self) {
            // Clean up test data directory
            let _ = std::fs::remove_dir_all("./test_data");
        }
    }

    #[test]
    fn register_impl_rejects_short_passwords() {
        let harness = TestHarness::new();
        let result = register_impl("alice".into(), "short".into(), &harness.app_state);

        assert!(matches!(
            result,
            Err(CommandError::Validation(ref msg)) if msg.contains("Password")
        ));
    }

    #[test]
    fn register_impl_rejects_empty_username() {
        let harness = TestHarness::new();
        let result = register_impl("   ".into(), "password123".into(), &harness.app_state);

        assert!(matches!(
            result,
            Err(CommandError::Validation(ref msg)) if msg.contains("Username")
        ));
    }

    #[test]
    fn mark_message_delivered_impl_updates_status() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        register_impl("alice".into(), "password123".into(), app_state).expect("register");
        login_impl("alice".into(), "password123".into(), app_state).expect("login");

        let session = app_state.session().get().expect("session");
        let message = app_state
            .message_service()
            .create_message(
                session.user.id,
                session.user.address.clone(),
                None,
                None,
                "hello world".into(),
            )
            .expect("create message");
        assert_eq!(message.status, MessageStatus::Sent);

        let updated = mark_message_delivered_impl(message.id, app_state).expect("mark delivered");
        assert_eq!(updated.status, MessageStatus::Delivered);
        assert!(updated.delivered_at.is_some());
    }

    #[test]
    fn mark_message_read_impl_updates_status() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        register_impl("alice".into(), "password123".into(), app_state).expect("register");
        login_impl("alice".into(), "password123".into(), app_state).expect("login");

        let session = app_state.session().get().expect("session");
        let message = app_state
            .message_service()
            .create_message(
                42,
                "peer:7000".into(),
                Some(session.user.id),
                Some(session.user.address.clone()),
                "incoming".into(),
            )
            .expect("create message");
        app_state
            .message_service()
            .mark_delivered(message.id)
            .expect("mark delivered");

        let updated = mark_message_read_impl(message.id, app_state).expect("mark read");
        assert_eq!(updated.status, MessageStatus::Read);
        assert!(updated.read_at.is_some());
    }

    #[test]
    fn mark_all_messages_read_impl_updates_all() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        register_impl("alice".into(), "password123".into(), app_state).expect("register");
        login_impl("alice".into(), "password123".into(), app_state).expect("login");

        let session = app_state.session().get().expect("session");

        let create_inbound = |content: &str| {
            let msg = app_state
                .message_service()
                .create_message(
                    777,
                    "peer:7000".into(),
                    Some(session.user.id),
                    Some(session.user.address.clone()),
                    content.into(),
                )
                .expect("create inbound");
            app_state
                .message_service()
                .mark_delivered(msg.id)
                .expect("deliver inbound");
            msg.id
        };

        let msg_a = create_inbound("one");
        let msg_b = create_inbound("two");

        mark_all_messages_read_impl(app_state).expect("mark all read");

        let reloaded_a = app_state
            .message_service()
            .get_message(msg_a)
            .expect("reload a");
        let reloaded_b = app_state
            .message_service()
            .get_message(msg_b)
            .expect("reload b");

        assert_eq!(reloaded_a.status, MessageStatus::Read);
        assert!(reloaded_a.read_at.is_some());
        assert_eq!(reloaded_b.status, MessageStatus::Read);
        assert!(reloaded_b.read_at.is_some());
    }

    #[test]
    fn login_impl_returns_auth_error_for_invalid_password() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        register_impl("alice".into(), "password123".into(), app_state).expect("register user");

        let result = login_impl("alice".into(), "wrongpass".into(), app_state);
        assert!(matches!(
            result,
            Err(CommandError::Authentication(ref msg)) if msg.contains("Invalid username or password")
        ));
    }

    #[tokio::test]
    async fn login_and_logout_cycle() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        let register =
            register_impl("alice".into(), "password".into(), app_state).expect("register");
        assert!(register.success);

        let login = login_impl("alice".into(), "password".into(), app_state).expect("login");
        assert!(login.success);
        assert!(app_state.session().get().is_some());

        let logout = logout_impl(app_state).expect("logout");
        assert!(logout.success);
        assert!(app_state.session().get().is_none());
    }

    #[tokio::test]
    async fn send_message_requires_auth() {
        let harness = TestHarness::new();

        let result =
            send_message_impl("hello".into(), &harness.node_service, &harness.app_state).await;
        assert!(matches!(result, Err(CommandError::Authentication(_))));
    }

    #[tokio::test]
    async fn send_message_happy_path() {
        let harness = TestHarness::new();
        let app_state = &harness.app_state;

        register_impl("bob".into(), "secret123".into(), app_state).expect("register");
        login_impl("bob".into(), "secret123".into(), app_state).expect("login");

        let message = send_message_impl("hi there".into(), &harness.node_service, app_state)
            .await
            .expect("send message");

        assert_eq!(message.content, "hi there");
        assert_eq!(message.status, MessageStatus::Delivered);
    }
}

/// Send a contact request to another user
#[tauri::command]
pub async fn send_contact_request(
    target_public_key: String,
    alias: Option<String>,
    app_state: tauri::State<'_, AppState>,
) -> Result<ContactRequestResult, String> {
    send_contact_request_impl(target_public_key, alias, app_state.inner())
        .await
        .map_err(|e| e.to_string())
}

async fn send_contact_request_impl(
    target_public_key: String,
    alias: Option<String>,
    app_state: &AppState,
) -> CommandResult<ContactRequestResult> {
    let session = require_session(app_state)?;

    println!(
        "Received contact request command for target: {}",
        target_public_key
    );

    // Get the contact request service from app state
    let contact_request_service = app_state.contact_request_service();

    // Send the contact request with user ID
    match contact_request_service
        .send_contact_request_with_user_id(
            &session.user.name,
            &session.user.name, // Using username as password placeholder - the service should handle this differently
            &target_public_key,
            alias.as_deref(),
            session.user.id, // Pass the user ID from the session
        )
        .await
    {
        Ok(_) => {
            println!(
                "Contact request sent successfully to: {}",
                target_public_key
            );
            Ok(ContactRequestResult {
                success: true,
                message: Some("Contact request sent successfully".to_string()),
                contact: None,
            })
        }
        Err(e) => {
            eprintln!("Failed to send contact request: {}", e);
            Err(CommandError::Service(format!(
                "Failed to send contact request: {e}"
            )))
        }
    }
}

/// Handle an incoming contact request and send a response
#[tauri::command]
pub async fn handle_contact_request(
    app_handle: tauri::AppHandle,
    request_json: String,
    approve: bool,
    app_state: tauri::State<'_, AppState>,
) -> Result<ContactRequestResult, String> {
    handle_contact_request_impl(&app_handle, request_json, approve, app_state.inner())
        .await
        .map_err(|e| e.to_string())
}

async fn handle_contact_request_impl(
    app_handle: &tauri::AppHandle,
    request_json: String,
    approve: bool,
    app_state: &AppState,
) -> CommandResult<ContactRequestResult> {
    let session = require_session(app_state)?;

    // Get the contact request service from app state
    let contact_request_service = app_state.contact_request_service();
    let contact_service = app_state.contact_service();

    let mut created_contact: Option<ContactSummary> = None;

    if approve {
        let (req_pub_key, req_alias) = extract_request_metadata(&request_json)
            .ok_or_else(|| CommandError::Validation("Invalid contact request format".into()))?;

        // Handle the contact request (this will also send auto-approval response)
        contact_request_service
            .handle_contact_request(
                &session.user.name,
                "", // Empty password placeholder - the service should get credentials differently
                &request_json,
            )
            .await
            .map_err(|e| CommandError::Service(format!("Failed to handle contact request: {e}")))?;

        let contact_address = req_pub_key.clone();

        created_contact = Some(ContactSummary {
            public_key: contact_address.clone(),
            alias: Some(req_alias.clone()),
            address: Some(contact_address.clone()),
        });

        if let Err(err) = contact_service.add_contact(
            session.user.id,
            req_alias.clone(),
            contact_address.clone(),
            None,
        ) {
            eprintln!(
                "Warning: failed to record contact '{}' for user '{}': {:?}",
                contact_address, session.user.name, err
            );
        }
    } else {
        // Extract requester public key from the request to send denial response
        // This is a simplified approach - in a real implementation, you'd parse the request properly
        if let Ok(contact_request) =
            crate::contacts::request::ContactRequest::from_json(&request_json)
        {
            contact_request_service
                .send_contact_response(
                    &session.user.name,
                    "", // Empty password placeholder - the service should get credentials differently
                    &contact_request.requester_public_key,
                    false, // not approved
                )
                .await
                .map_err(|e| {
                    CommandError::Service(format!("Failed to send contact denial: {e}"))
                })?;
        } else {
            return Err(CommandError::Validation(
                "Invalid contact request format".into(),
            ));
        }
    }

    if let Some(ref contact) = created_contact {
        emit_contact_added(
            app_handle,
            contact.public_key.clone(),
            contact.alias.clone(),
            contact.address.clone(),
        );
    }

    Ok(ContactRequestResult {
        success: true,
        message: if approve {
            Some("Contact request approved and added".to_string())
        } else {
            Some("Contact request denied".to_string())
        },
        contact: created_contact,
    })
}

fn extract_request_metadata(json: &str) -> Option<(String, String)> {
    if let Ok(request) = crate::contacts::request::ContactRequest::from_json(json) {
        return Some((request.requester_public_key, request.requester_alias));
    }

    let value: Value = serde_json::from_str(json).ok()?;
    let data = value.get("ContactRequest").unwrap_or(&value);
    let public_key = data
        .get("requester_public_key")
        .and_then(|v| v.as_str())?
        .to_string();
    let alias = data
        .get("requester_alias")
        .and_then(|v| v.as_str())?
        .to_string();
    Some((public_key, alias))
}

/// Get discovered network nodes
#[tauri::command]
pub async fn get_discovered_nodes(
    node_service: tauri::State<'_, Arc<Mutex<NodeService>>>,
) -> Result<Vec<DiscoveredNodeInfo>, String> {
    let service = node_service.inner().lock().await;
    let peer_info_map = service.node.peer_info.lock().unwrap();

    let nodes: Vec<DiscoveredNodeInfo> = peer_info_map
        .iter()
        .map(|(addr, info)| DiscoveredNodeInfo {
            name: info.node_name.clone(),
            username: info.username.clone(),
            address: addr.to_string(),
            listen_port: info.listen_port,
            ip: addr.ip().to_string(),
            port: info.listen_port.unwrap_or(addr.port()),
            display_label: info.display_label(),
            is_connected: info.is_connected,
            last_seen: None, // In a full implementation, we would track this
        })
        .collect();

    Ok(nodes)
}

#[derive(serde::Serialize)]
pub struct DiscoveredNodeInfo {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub address: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    pub ip: String,
    pub port: u16,
    pub display_label: String,
    pub is_connected: bool,
    pub last_seen: Option<u64>,
}

#[derive(serde::Serialize)]
pub struct ContactRequestResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contact: Option<ContactSummary>,
}

#[derive(Clone, serde::Serialize)]
pub struct ContactSummary {
    pub public_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

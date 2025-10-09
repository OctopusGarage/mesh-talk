pub mod api;
pub mod commands;
pub mod contacts;
pub mod crypto;
pub mod domain;
pub mod error;
pub mod events;
pub mod identity;
pub mod logger;
pub mod network;
pub mod notifications;
pub mod perf;
pub mod platform;
pub mod services;
pub mod state;
pub mod storage;
pub mod tray;
pub mod user_friendly_errors;
pub mod utils;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_friendly_error_module() {
        let error = error::MeshTalkError::auth("test error");
        let friendly_message = user_friendly_errors::format_user_friendly_error(&error);
        assert!(friendly_message.contains("Authentication failed"));
    }
}

use crate::api::AppConfig;
use crate::domain::models::PeerInfo;
use crate::events::setup_node_service_events;
use crate::network::runtime::NetworkRuntime;
use crate::network::udp::start_udp_broadcast;
use crate::network::utils::get_preferred_local_ip;
use crate::services::node_service::NodeService;
use crate::state::AppState;
use std::env;
use std::sync::Arc;
use tauri::Manager;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Debug)]
pub struct NetworkStartup {
    pub port: u16,
    pub runtime: NetworkRuntime,
}

// Tauri application entry point
pub fn run_tauri() {
    let _timer = perf_monitor!("application_startup");
    let base_config = AppConfig::from_env();
    log::info!(
        "Starting Mesh-Talk desktop runtime with node '{}' (requested TCP port: {})",
        base_config.name,
        base_config.port
    );
    let node_service = Arc::new(Mutex::new(NodeService::new(
        base_config.name.clone(),
        base_config.port,
    )));
    let app_config = Arc::new(base_config);

    // Initialize file manager
    lazy_static::lazy_static! {
        static ref FILE_MANAGER: Arc<crate::storage::file_manager::FileManager> = {
            log::info!("Initializing file manager...");
            // Use ~/.mesh-talk as the base data directory
            let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            let data_path = std::path::PathBuf::from(home_dir).join(".mesh-talk");
            let data_path_str = data_path.to_str().unwrap_or(".mesh-talk");
            log::info!("Data path: {}", data_path_str);
            let file_manager = crate::storage::file_manager::FileManager::new(data_path);
            log::info!("File manager initialized successfully");
            Arc::new(file_manager)
        };
    }

    // Force initialization of the file manager
    log::info!("Accessing file manager to force initialization...");
    let _file_manager = FILE_MANAGER.clone();
    log::info!("File manager accessed");

    // Get data path string for message service
    let data_path_str = {
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let data_path = std::path::PathBuf::from(home_dir).join(".mesh-talk");
        data_path.to_str().unwrap_or(".mesh-talk").to_string()
    };

    // Initialize identity manager first
    let identity_manager = Arc::new(crate::identity::manager::IdentityManager::new(
        FILE_MANAGER.as_ref().clone(),
    ));

    // Initialize contact service
    crate::services::contact_service::ContactService::init_global(
        FILE_MANAGER.as_ref().clone(),
        identity_manager,
    );

    // Initialize contact request service
    if let Err(e) = crate::services::contact_request_service::ContactRequestService::init_global(
        FILE_MANAGER.as_ref().clone(),
        Arc::clone(&node_service),
    ) {
        log::warn!("Failed to initialize contact request service: {}", e);
    }

    // Initialize message service
    crate::services::message_service::MessageService::init_global(data_path_str.clone());

    // Initialize auth service
    crate::services::auth_service::AuthService::init_global(FILE_MANAGER.as_ref().clone());

    // Wrap NodeService in Arc<Mutex<>> for sharing across threads
    // Shared application state for Tauri commands
    let app_state = AppState::new(
        crate::services::auth_service::AuthService::global().clone(),
        crate::services::contact_service::ContactService::global().clone(),
        crate::services::message_service::MessageService::global().clone(),
        Arc::clone(&app_config),
    );

    let file_transfer_manager = crate::services::file_transfer::FileTransferManager::init_global(
        data_path_str.clone(),
        app_state.clone(),
    );
    tauri::async_runtime::block_on(async {
        file_transfer_manager
            .set_node_service(Arc::clone(&node_service))
            .await;
    });

    log::info!("Network services are idle until a user signs in; no sockets are bound yet.");

    // Start Tauri app
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .setup(move |app| {
            // Initialize logging system
            if let Err(e) = crate::logger::init_logging(&app.handle()) {
                log::error!("Failed to initialize logging: {}", e);
            }

            // Get the node service from the app state
            let node_service = app.state::<Arc<Mutex<NodeService>>>().inner().clone();

            // Set up event listeners for the NodeService
            log::info!("Setting up NodeService event listeners...");
            let event_service = Arc::clone(&node_service);
            let app_handle = app.handle().clone();
            crate::tray::create_system_tray(&app_handle)?;
            let async_app_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                setup_node_service_events(event_service, async_app_handle).await;
            });

            // Update notification service with app handle
            crate::services::node_service::NOTIFICATION_SERVICE.set_app_handle(app_handle.clone());

            #[cfg(debug_assertions)]
            {
                let window = app.get_webview_window("main").unwrap();
                window.open_devtools();
                window.close_devtools();
            }
            Ok(())
        })
        .manage(node_service)
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::send_message,
            commands::get_node_info,
            commands::login,
            commands::start_network,
            commands::stop_network,
            commands::connect_to_node,
            commands::connect_to_user,
            commands::logout,
            commands::register,
            commands::get_contacts,
            commands::get_messages,
            commands::mark_message_read,
            commands::mark_message_delivered,
            commands::mark_all_messages_read,
            commands::get_discovered_nodes,
            commands::send_contact_request,
            commands::handle_contact_request,
            commands::send_file,
            commands::resume_file_transfer,
            commands::cancel_file_transfer,
            commands::list_file_transfers,
            commands::accept_incoming_file_transfer,
            commands::reject_incoming_file_transfer
        ])
        .run(tauri::generate_context!())
        .map_err(|e| log::error!("Error while running tauri application: {}", e))
        .unwrap_or(());
}

/// Launch networking stack for the provided node service (shared by CLI and integration tests).
///
/// This helper keeps the legacy behaviour of always enabling UDP broadcast.
pub async fn launch_network(
    node_service: Arc<Mutex<NodeService>>,
) -> std::io::Result<NetworkStartup> {
    launch_network_with_broadcast(node_service, true, None).await
}

/// Launch networking stack for the provided node service with conditional broadcast
///
/// The sequence of operations is as follows:
/// 1. Start TCP listener for incoming connections
/// 2. Start UDP discovery to listen for other nodes on the network
/// 3. Start UDP broadcast only if start_broadcast parameter is true (i.e., after authentication)
/// 4. Start reconnection manager
pub async fn launch_network_with_broadcast(
    node_service: Arc<Mutex<NodeService>>,
    start_broadcast: bool,
    broadcast_username: Option<String>,
) -> std::io::Result<NetworkStartup> {
    let listener = {
        // Start from port 7000 and try to find an available port
        let mut port = 7000;
        loop {
            match TcpListener::bind(("0.0.0.0", port)).await {
                Ok(listener) => {
                    log::info!("TCP service active on port {}", port);
                    break listener;
                }
                Err(_) => {
                    port += 1;
                    // Safety check to avoid infinite loop
                    if port > 10000 {
                        return Err(std::io::Error::other(
                            "Unable to find an available port between 7000 and 10000",
                        ));
                    }
                }
            }
        }
    };

    let actual_port = listener.local_addr()?.port();

    {
        let mut service = node_service.lock().await;
        service.update_port(actual_port);
    }

    // Handle incoming TCP connections
    let accept_service = Arc::clone(&node_service);
    let tcp_handle: JoinHandle<()> = tokio::spawn(async move {
        while let Ok((stream, addr)) = listener.accept().await {
            let service = accept_service.clone();
            tokio::spawn(async move {
                let service = service.lock().await;
                if let Err(e) = service.handle_incoming_connection(stream, addr).await {
                    log::error!("Failed to handle connection: {}", e);
                }
            });
        }
    });

    // Set up UDP discovery
    log::info!("Setting up UDP discovery...");
    let discovery_service = Arc::clone(&node_service);
    let local_ip = get_preferred_local_ip();
    let udp_discovery_handle: JoinHandle<()> = tokio::spawn({
        let local_ip = local_ip.clone();
        async move {
            tokio::spawn(crate::network::udp::start_udp_discovery(
                move |peer_addr, peer_name, peer_username, peer_port, peer_user_id| {
                    let service_clone = discovery_service.clone();
                    let local_ip = local_ip.clone();
                    tokio::spawn(async move {
                        // log::info!("[UDP Discovery Callback] Received peer_addr: {}", peer_addr);
                        let service_port = {
                            let service = service_clone.lock().await;
                            service.get_port()
                        };

                        let is_self = match &local_ip {
                            Some(ip) => peer_addr.ip() == *ip && peer_addr.port() == service_port,
                            None => {
                                peer_addr.ip().is_loopback() && peer_addr.port() == service_port
                            }
                        };

                        if is_self {
                            return;
                        }

                        let service_for_peer = service_clone.clone();
                        {
                            let service = service_for_peer.lock().await;
                            let mut peer_info_map = service.node.peer_info.lock().unwrap();
                            if let Some(existing) = peer_info_map.get_mut(&peer_addr) {
                                existing.update_metadata(
                                    peer_name.clone(),
                                    peer_username.clone(),
                                    Some(peer_port),
                                    peer_user_id.clone(),
                                );
                                existing.update_heartbeat();
                            } else {
                                let mut peer_info = PeerInfo::new(
                                    peer_addr,
                                    peer_name.clone(),
                                    peer_username.clone(),
                                    Some(peer_port),
                                );
                                peer_info.user_id = peer_user_id.clone();
                                peer_info.mark_connected();
                                let _label = peer_info.display_label();
                                peer_info_map.insert(peer_addr, peer_info);
                            }
                        }

                        let service = service_for_peer.lock().await;
                        let mut registry = service.node_registry.lock().unwrap();
                        registry.add_or_update_node(
                            peer_addr,
                            peer_name.clone(),
                            peer_username.clone(),
                            Some(peer_port),
                            peer_user_id.clone(), // Pass the user_id to the registry
                        );
                    });
                },
            ));
        }
    });

    // Start UDP broadcast only if requested
    let udp_broadcast_handle = if start_broadcast {
        log::info!("Starting UDP broadcast...");
        let broadcast_name = {
            let service = node_service.lock().await;
            service.get_name()
        };
        let (username_clone, user_id) = {
            let service = node_service.lock().await;
            let user_id_str = service.get_user_id();
            if user_id_str.is_empty() {
                (broadcast_username.clone(), None)
            } else {
                (broadcast_username.clone(), Some(user_id_str))
            }
        };
        Some(tokio::spawn(async move {
            if let Err(e) =
                start_udp_broadcast(broadcast_name, username_clone, actual_port, user_id).await
            {
                log::error!("UDP broadcast failed: {}", e);
            }
        }))
    } else {
        log::info!("Skipping UDP broadcast as requested.");
        None
    };

    // Start the reconnection manager
    log::info!("Starting reconnection manager...");
    let reconnection_service = Arc::clone(&node_service);
    let reconnection_handle: JoinHandle<()> = tokio::spawn(async move {
        let (reconnection_manager, node_name, message_handlers) = {
            let service = reconnection_service.lock().await;
            (
                service.get_reconnection_manager(),
                service.get_name(),
                service.message_handlers.clone(),
            )
        };

        reconnection_manager
            .start_monitoring(move |line| {
                let name_clone = node_name.clone();
                let message_handlers_clone = message_handlers.clone();
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::services::node_service::NodeService::handle_message_static(
                            line,
                            name_clone,
                            message_handlers_clone,
                        )
                        .await
                    {
                        log::error!("Failed to process message: {}", e);
                    }
                });
            })
            .await;
    });

    let runtime = NetworkRuntime::new(
        tcp_handle,
        udp_discovery_handle,
        udp_broadcast_handle,
        reconnection_handle,
    );

    Ok(NetworkStartup {
        port: actual_port,
        runtime,
    })
}

/// Run the CLI version of the application
pub async fn run_cli(node_service: Arc<Mutex<NodeService>>) -> std::io::Result<()> {
    let (node_name, requested_port) = {
        let service = node_service.lock().await;
        (service.get_name(), service.get_port())
    };
    log::info!(
        "Starting node service for '{}' (requested TCP port: {})",
        node_name,
        requested_port
    );

    // For CLI, we always start the broadcast since there's no login concept
    let network_startup =
        match launch_network_with_broadcast(Arc::clone(&node_service), true, None).await {
            Ok(startup) => startup,
            Err(e) => {
                log::error!("{}", crate::user_friendly_errors::format_any_error(&e));
                return Err(std::io::Error::other("Failed to launch network"));
            }
        };
    let actual_port = network_startup.port;
    let _runtime = network_startup.runtime;

    let node_name = {
        let service = node_service.lock().await;
        service.get_name()
    };
    log::info!("Local node: {} (TCP port: {})", node_name, actual_port);

    log::info!("Start chatting (type 'quit' to exit):");

    // Handle user input
    let mut input = String::new();
    let stdin = std::io::stdin();
    loop {
        input.clear();
        stdin.read_line(&mut input)?;
        let message = input.trim().to_string();

        if message == "quit" {
            break;
        }

        let service = node_service.clone();
        tokio::spawn(async move {
            let service = service.lock().await;
            if let Err(e) = service.broadcast_message(message).await {
                log::error!("{}", crate::user_friendly_errors::format_any_error(&e));
            }
        });
    }

    Ok(())
}

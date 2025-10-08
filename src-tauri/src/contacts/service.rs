use crate::contacts::manager::ContactManager;
use crate::contacts::request::{ContactRequest, ContactResponse};
use crate::events::{emit_contact_added, with_node_event_app_handle};
use crate::services::node_service::NodeService;
use serde_json::Value;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Service for handling contact requests
pub struct ContactRequestService {
    contact_manager: Arc<ContactManager>,
    node_service: Arc<Mutex<NodeService>>,
}

impl ContactRequestService {
    /// Create a new contact request service
    pub fn new(
        contact_manager: Arc<ContactManager>,
        node_service: Arc<Mutex<NodeService>>,
    ) -> Self {
        ContactRequestService {
            contact_manager,
            node_service,
        }
    }

    /// Send a contact request to another user
    pub async fn send_contact_request(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        target_public_key: &str,
        alias: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Default to a placeholder user ID if not provided
        self.send_contact_request_with_user_id(
            username,
            _password,
            target_public_key,
            alias,
            String::from("0"),
            None,
            None,
            None,
        )
        .await
    }

    /// Send a contact request to another user with user ID
    pub async fn send_contact_request_with_user_id(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        target_public_key: &str,
        alias: Option<&str>,
        user_id: String,
        remote_username: Option<String>, // Add username parameter
        remote_ip: Option<String>,       // Add IP parameter
        remote_port: Option<u16>,        // Add port parameter
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Note: In a real implementation, this would be called from a context that has access to the user's keys
        // without needing to re-authenticate. For now, we'll work with what's available.
        // The proper solution would be to store the signing key in the session after login.

        info!(
            "Sending contact request for user: {} with user ID: {}",
            username, user_id
        );

        // In a real implementation, we would get the user's signing key from their session context
        // For now, let's simulate a basic implementation that generates a contact request

        let (node_name, port) = {
            let service = self.node_service.lock().await;
            (service.get_name(), service.get_port())
        };

        let local_ip = resolve_local_ip().unwrap_or_else(|| "127.0.0.1".to_string());

        let alias_value = if let Some(value) = alias {
            value.to_string()
        } else {
            format!(
                "{} • {} • {}:{}",
                if node_name.trim().is_empty() {
                    "Unknown"
                } else {
                    &node_name
                },
                username,
                local_ip,
                port
            )
        };

        let requester_address = format!("{}:{}", local_ip, port);

        // Create a basic contact request - in a real implementation, this would be properly signed
        let contact_request = crate::contacts::request::ContactRequest {
            requester_public_key: requester_address.clone(),
            requester_alias: alias_value.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            signature: vec![], // This would be a real signature in a working implementation
        };

        info!(
            "Contact request created for target: {} using node alias: {} (address: {})",
            target_public_key, alias_value, requester_address
        );

        // Create a message for the contact request with user ID
        let message = crate::domain::message::Message::ContactRequest {
            requester_public_key: contact_request.requester_public_key,
            requester_alias: alias_value.clone(),
            timestamp: contact_request.timestamp,
            signature: contact_request.signature,
            node_name: Some(node_name.clone()),
            username: Some(username.to_string()),
            user_id: Some(user_id.clone()),
            ip: remote_ip.clone(), // Add the remote IP to the message
            port: remote_port,     // Add the remote port to the message
        };

        // Serialize the message
        let message_json = serde_json::to_string(&message)?;

        info!(
            "Serialized contact request message: {}",
            &message_json[..std::cmp::min(1000, message_json.len())]
        );

        let target_addr: SocketAddr = target_public_key
            .parse()
            .map_err(|e| format!("Invalid target address '{target_public_key}': {e}"))?;

        info!(
            "Attempting to deliver contact request to peer at {}",
            target_addr
        );

        {
            let node_service = self.node_service.lock().await;

            let mut delivered = false;
            if !user_id.trim().is_empty() && user_id != "0" {
                match node_service
                    .send_json_message_to_user(&user_id, message_json.clone())
                    .await
                {
                    Ok(_) => {
                        delivered = true;
                    }
                    Err(err) => {
                        warn!(
                            "Failed to deliver contact request via user_id {}: {}. Falling back to address {}",
                            user_id, err, target_addr
                        );
                    }
                }
            }

            if !delivered {
                node_service
                    .send_json_message_to_peer(target_addr, message_json.clone())
                    .await
                    .map_err(|e| {
                        format!("Failed to deliver contact request to {target_addr}: {e}")
                    })?;
            }
        }

        info!(
            "Contact request sent successfully to: {} with user ID: {}",
            target_addr, user_id
        );
        Ok(())
    }

    /// Handle an incoming contact request
    pub async fn handle_contact_request(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        request_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        log::info!("=== START HANDLE_CONTACT_REQUEST IN SERVICE ===");
        log::info!("Username: {}, Request JSON: {}", username, request_json);

        // Extract user_id from the request JSON if available
        let mut contact_user_id = None;
        if let Ok(value) = serde_json::from_str::<Value>(request_json) {
            if let Some(user_id_value) = value.get("user_id") {
                if let Some(user_id_str) = user_id_value.as_str() {
                    contact_user_id = Some(user_id_str.to_string());
                    log::info!("Extracted user_id from request: {:?}", user_id_str);
                }
            }
        }

        log::info!("Contact user_id: {:?}", contact_user_id);

        // Normalize user_id (trim whitespace, drop empty values)
        contact_user_id = contact_user_id
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty());

        // Deserialize the contact request
        log::info!("Deserializing contact request...");
        let request = match ContactRequest::from_json(request_json) {
            Ok(req) => {
                log::info!(
                    "Successfully deserialized contact request: requester={}, alias={}",
                    req.requester_public_key,
                    req.requester_alias
                );
                req
            }
            Err(e) => {
                log::warn!("Failed to deserialize with ContactRequest::from_json: {}. Trying fallback parsing...", e);
                let value: Value = serde_json::from_str(request_json)?;
                let requester_public_key = value
                    .get("requester_public_key")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing requester_public_key")?
                    .to_string();
                let requester_alias = value
                    .get("requester_alias")
                    .and_then(|v| v.as_str())
                    .ok_or("Missing requester_alias")?
                    .to_string();
                let timestamp = value
                    .get("timestamp")
                    .and_then(|v| v.as_u64())
                    .ok_or("Missing timestamp")?;

                log::info!(
                    "Fallback parsing successful: requester={}, alias={}",
                    requester_public_key,
                    requester_alias
                );

                ContactRequest {
                    requester_public_key,
                    requester_alias,
                    timestamp,
                    signature: Vec::new(),
                }
            }
        };

        // Verify the signature when present; otherwise skip (placeholder implementation)
        if request.signature.is_empty() {
            log::info!(
                "Received unsigned contact request from {}; skipping signature verification",
                request.requester_public_key
            );
        } else if !request.verify_signature().unwrap_or(false) {
            log::error!("Invalid signature in contact request");
            return Err("Invalid signature in contact request".into());
        }

        log::info!(
            "Received contact request from user: {}",
            request.requester_alias
        );

        // Ensure TCP connectivity to requester if user_id is known so response can reuse channel
        if let Some(uid) = contact_user_id.as_ref() {
            let node_service_clone = {
                let service = self.node_service.lock().await;
                service.clone()
            };

            match node_service_clone.ensure_connection_for_user(uid).await {
                Ok(status) => {
                    info!(
                        "Ensured TCP channel to requester user_id {} at {} (reused={})",
                        uid, status.addr, status.reused
                    );
                }
                Err(err) => {
                    warn!(
                        "Failed to prepare TCP channel for requester user_id {}: {}",
                        uid, err
                    );
                }
            }
        }

        // Parse the IP and port from the public key
        let parts: Vec<&str> = request.requester_public_key.split(':').collect();
        if parts.len() != 2 {
            log::error!(
                "Invalid requester public key format: {}",
                request.requester_public_key
            );
            return Err("Invalid requester public key format".into());
        }

        let ip = parts[0];
        let port = parts[1].parse::<u16>().map_err(|_| {
            log::error!(
                "Invalid port in requester public key: {}",
                request.requester_public_key
            );
            "Invalid port in requester public key"
        })?;

        log::info!(
            "Calling contact_manager.add_contact with IP: {}, Port: {}",
            ip,
            port
        );
        if let Err(err) = self.contact_manager.add_contact(
            username,
            _password,
            ip,
            port,
            &request.requester_alias,
            contact_user_id.clone(), // Use the extracted user_id
        ) {
            log::warn!(
                "Warning: failed to add contact '{}' for user '{}': {:?}",
                request.requester_public_key,
                username,
                err
            );
        } else {
            log::info!(
                "Added contact '{}' ({}) for user '{}'",
                request.requester_alias,
                request.requester_public_key,
                username
            );
        }

        // Send an approval response
        log::info!("Sending contact approval response...");
        let response_result = self
            .send_contact_response(
                username,
                _password,
                &request.requester_public_key,
                true,
                contact_user_id.clone(),
            )
            .await;

        match &response_result {
            Ok(_) => log::info!("Successfully sent contact approval response"),
            Err(e) => log::error!("Failed to send contact approval response: {}", e),
        }

        response_result?;

        log::info!("=== END HANDLE_CONTACT_REQUEST IN SERVICE ===");
        Ok(())
    }

    /// Send a contact response (approval/denial)
    pub async fn send_contact_response(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        target_public_key: &str,
        approved: bool,
        target_user_id: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // In a real implementation, this would get user information from session context
        // rather than re-authenticating with username/password

        let (node_name, port, local_user_id, node_service_clone) = {
            let service = self.node_service.lock().await;
            (
                service.get_name(),
                service.get_port(),
                service.get_user_id(),
                service.clone(),
            )
        };

        let responder_alias = if node_name.trim().is_empty() {
            username.to_string()
        } else {
            node_name
        };

        let responder_public_key = format!(
            "{}:{}",
            resolve_local_ip().unwrap_or_else(|| "127.0.0.1".to_string()),
            port
        );

        // Create a basic contact response - in a real implementation, this would be properly signed
        let contact_response = crate::contacts::request::ContactResponse {
            responder_public_key: responder_public_key.clone(),
            approved,
            responder_alias: responder_alias.clone(),
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            signature: vec![], // This would be a real signature in a working implementation
            user_id: if local_user_id.trim().is_empty() {
                None
            } else {
                Some(local_user_id.clone())
            },
        };

        // Create a message for the contact response
        let message = crate::domain::message::Message::ContactResponse {
            responder_public_key: contact_response.responder_public_key.clone(),
            approved: contact_response.approved,
            responder_alias: contact_response.responder_alias.clone(),
            timestamp: contact_response.timestamp,
            signature: contact_response.signature.clone(),
            user_id: if local_user_id.trim().is_empty() {
                None
            } else {
                Some(local_user_id.clone())
            },
        };

        // Serialize the message
        let message_json = serde_json::to_string(&message)?;

        let target_addr: SocketAddr = target_public_key
            .parse()
            .map_err(|e| format!("Invalid target address '{target_public_key}': {e}"))?;

        info!(
            "Attempting to deliver contact response to peer at {} (approved: {})",
            target_addr, approved
        );

        let normalized_user_id = target_user_id
            .as_ref()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        let mut delivered = false;
        if let Some(ref uid) = normalized_user_id {
            match node_service_clone
                .send_json_message_to_user(uid, message_json.clone())
                .await
            {
                Ok(_) => {
                    info!(
                        "Delivered contact response via user_id {} (approved={})",
                        uid, approved
                    );
                    delivered = true;
                }
                Err(err) => {
                    warn!(
                        "Failed to deliver contact response via user_id {}: {}. Falling back to address {}",
                        uid,
                        err,
                        target_addr
                    );
                }
            }
        }

        if !delivered {
            node_service_clone
                .send_json_message_to_peer(target_addr, message_json.clone())
                .await
                .map_err(|e| format!("Failed to deliver contact response to {target_addr}: {e}"))?;
            info!(
                "Delivered contact response via address {} (approved={})",
                target_addr, approved
            );
        }

        if approved {
            info!("Sent contact approval to {}", target_addr);
        } else {
            info!("Sent contact denial to {}", target_addr);
        }

        Ok(())
    }

    /// Handle an incoming contact response
    pub async fn handle_contact_response(
        &self,
        username: &str,
        password: &str,
        response_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Deserialize the contact response
        let response = ContactResponse::from_json(response_json)?;

        if response.signature.is_empty() {
            info!(
                "Received unsigned contact response from {}; skipping signature verification",
                response.responder_public_key
            );
        } else if !response.verify_signature().unwrap_or(false) {
            return Err("Invalid signature in contact response".into());
        }

        // Parse the IP and port from the public key
        let parts: Vec<&str> = response.responder_public_key.split(':').collect();
        if parts.len() != 2 {
            log::error!(
                "Invalid responder public key format: {}",
                response.responder_public_key
            );
            return Err("Invalid responder public key format".into());
        }

        let ip = parts[0];
        let port = parts[1].parse::<u16>().map_err(|_| {
            log::error!(
                "Invalid port in responder public key: {}",
                response.responder_public_key
            );
            "Invalid port in responder public key"
        })?;

        if response.approved {
            if let Some(ref uid) = response.user_id {
                let trimmed_uid = uid.trim();
                if !trimmed_uid.is_empty() {
                    let node_service_clone = {
                        let service = self.node_service.lock().await;
                        service.clone()
                    };

                    match node_service_clone
                        .ensure_connection_for_user(trimmed_uid)
                        .await
                    {
                        Ok(status) => {
                            info!(
                                "Ensured TCP channel to responder user_id {} at {} (reused={})",
                                trimmed_uid, status.addr, status.reused
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to prepare TCP channel for responder user_id {}: {}",
                                trimmed_uid, err
                            );
                        }
                    }
                }
            }

            match self.contact_manager.add_contact(
                username,
                password,
                ip,
                port,
                &response.responder_alias,
                response.user_id.clone(),
            ) {
                Ok(_) => {
                    info!(
                        "Contact request approved by user: {} ({})",
                        response.responder_alias, response.responder_public_key
                    );

                    let address_string = format!("{}:{}", ip, port);
                    with_node_event_app_handle(|app_handle| {
                        emit_contact_added(
                            app_handle,
                            response.responder_public_key.clone(),
                            Some(response.responder_alias.clone()),
                            Some(address_string.clone()),
                        );
                    });
                }
                Err(err) => info!(
                    "Warning: failed to persist approved contact '{}' for '{}': {:?}",
                    response.responder_public_key, username, err
                ),
            }
        } else {
            info!(
                "Contact request denied by user: {}",
                response.responder_alias
            );
        }

        Ok(())
    }

    /// Process an incoming network message and dispatch to appropriate handlers
    pub async fn process_incoming_message(
        &self,
        username: &str,
        password: &str,
        message_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Try to deserialize the message
        let message: crate::domain::message::Message =
            serde_json::from_str(message_json).map_err(|_| "Failed to deserialize message")?;

        match message {
            crate::domain::message::Message::ContactRequest {
                requester_public_key,
                requester_alias,
                timestamp,
                signature,
                node_name,
                username: message_username,
                user_id: message_user_id,
                ip: message_ip,
                port: message_port,
            } => {
                // Create a contact request from the message data
                let request = ContactRequest {
                    requester_public_key,
                    requester_alias,
                    timestamp,
                    signature,
                };

                // Serialize the request for the handler, including additional fields
                let mut request_json = request.to_json()?;

                // Add the additional fields to the JSON if they exist
                if let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&request_json) {
                    if let Some(obj) = value.as_object_mut() {
                        if let Some(node_name) = node_name {
                            obj.insert(
                                "node_name".to_string(),
                                serde_json::Value::String(node_name),
                            );
                        }
                        if let Some(ref message_username) = message_username {
                            obj.insert(
                                "username".to_string(),
                                serde_json::Value::String(message_username.clone()),
                            );
                        }
                        if let Some(message_user_id) = message_user_id {
                            obj.insert(
                                "user_id".to_string(),
                                serde_json::Value::String(message_user_id),
                            );
                        }
                        if let Some(message_ip) = message_ip {
                            obj.insert("ip".to_string(), serde_json::Value::String(message_ip));
                        }
                        if let Some(message_port) = message_port {
                            obj.insert(
                                "port".to_string(),
                                serde_json::Value::Number(message_port.into()),
                            );
                        }
                        request_json = serde_json::to_string(&value)?;
                    }
                }

                // Handle the contact request
                self.handle_contact_request(
                    message_username.as_deref().unwrap_or(""),
                    password,
                    &request_json,
                )
                .await?;
            }
            crate::domain::message::Message::ContactResponse {
                responder_public_key,
                approved,
                responder_alias,
                timestamp,
                signature,
                ..
            } => {
                // Create a contact response from the message data
                let response = ContactResponse {
                    responder_public_key,
                    approved,
                    responder_alias,
                    timestamp,
                    signature,
                    user_id: None,
                };

                // Serialize the response for the handler
                let response_json = response.to_json()?;

                // Handle the contact response
                self.handle_contact_response(username, password, &response_json)
                    .await?;
            }
            _ => {
                // Ignore other message types
            }
        }

        Ok(())
    }
}

fn resolve_local_ip() -> Option<String> {
    if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
        if socket.connect("8.8.8.8:80").is_ok() {
            if let Ok(addr) = socket.local_addr() {
                let ip = addr.ip();
                if !ip.is_unspecified() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::file_manager::FileManager;

    #[test]
    fn test_contact_request_service_creation() {
        // Create a temporary directory for test data
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
        let data_path = temp_dir.path().to_path_buf();

        let file_manager = FileManager::new(data_path);
        let node_service = Arc::new(Mutex::new(crate::services::node_service::NodeService::new(
            "test-node".to_string(),
            0,
        )));
        let identity_manager = crate::identity::manager::IdentityManager::new(file_manager.clone());
        let contact_manager = Arc::new(ContactManager::new(file_manager.clone(), identity_manager));
        let _service = ContactRequestService::new(contact_manager, node_service);

        // Just test that the service can be created
        assert!(true);
    }

    #[test]
    fn test_contact_request_service_process_incoming_message() {
        // Create a temporary directory for test data
        let temp_dir = tempfile::tempdir().expect("Failed to create temp directory");
        let data_path = temp_dir.path().to_path_buf();

        let file_manager = FileManager::new(data_path);
        let node_service = Arc::new(Mutex::new(crate::services::node_service::NodeService::new(
            "test-node".to_string(),
            0,
        )));
        let identity_manager = crate::identity::manager::IdentityManager::new(file_manager.clone());
        let contact_manager = Arc::new(ContactManager::new(file_manager.clone(), identity_manager));
        let service = ContactRequestService::new(contact_manager, node_service);

        // Test processing a contact request message
        let contact_request_message = crate::domain::message::Message::ContactRequest {
            requester_public_key: "test_public_key".to_string(),
            requester_alias: "Test User".to_string(),
            timestamp: 1234567890,
            signature: vec![1, 2, 3, 4],
            node_name: Some("test-node".to_string()),
            username: Some("tester".to_string()),
            user_id: Some("test-user-1".to_string()),
            ip: Some("127.0.0.1".to_string()),
            port: Some(7000),
        };

        let message_json = serde_json::to_string(&contact_request_message).unwrap();

        // This should not panic, though it will return an error because the user doesn't exist
        let result = futures::executor::block_on(service.process_incoming_message(
            "nonexistent_user",
            "password",
            &message_json,
        ));

        // We expect an error because the user doesn't exist
        assert!(result.is_err());

        // Test processing a contact response message
        let contact_response_message = crate::domain::message::Message::ContactResponse {
            responder_public_key: "test_public_key".to_string(),
            approved: true,
            responder_alias: "Test User".to_string(),
            timestamp: 1234567890,
            signature: vec![1, 2, 3, 4],
            user_id: Some("test-user-1".to_string()),
        };

        let message_json = serde_json::to_string(&contact_response_message).unwrap();

        // This should not panic, though it will return an error because the user doesn't exist
        let result = futures::executor::block_on(service.process_incoming_message(
            "nonexistent_user",
            "password",
            &message_json,
        ));

        // We expect an error because the user doesn't exist
        assert!(result.is_err());
    }
}

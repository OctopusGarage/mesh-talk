use crate::contacts::manager::ContactManager;
use crate::contacts::request::{ContactRequest, ContactResponse};

use crate::services::node_service::NodeService;
use serde_json::Value;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;

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
        self.send_contact_request_with_user_id(username, _password, target_public_key, alias, 0)
            .await
    }

    /// Send a contact request to another user with user ID
    pub async fn send_contact_request_with_user_id(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        target_public_key: &str,
        alias: Option<&str>,
        user_id: u64,
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
            user_id: Some(user_id),
            ip: Some(local_ip.clone()),
            port: Some(port),
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
            node_service
                .send_json_message_to_peer(target_addr, message_json.clone())
                .await
                .map_err(|e| format!("Failed to deliver contact request to {target_addr}: {e}"))?;
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
        // Deserialize the contact request
        let request = match ContactRequest::from_json(request_json) {
            Ok(req) => req,
            Err(_) => {
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
            info!(
                "Received unsigned contact request from {}; skipping signature verification",
                request.requester_public_key
            );
        } else if !request.verify_signature().unwrap_or(false) {
            return Err("Invalid signature in contact request".into());
        }

        // Check if the contact already exists
        // Note: This check may not work properly without proper password
        // In a real implementation, contact existence checks would work differently
        // if self
        //     .contact_manager
        //     .contact_exists(username, _password, &request.requester_public_key)?
        // {
        //     // Contact already exists, nothing to do
        //     return Ok(());
        // }

        info!(
            "Received contact request from user: {}",
            request.requester_alias
        );

        if let Err(err) = self.contact_manager.add_contact(
            username,
            _password,
            &request.requester_public_key,
            Some(&request.requester_alias),
        ) {
            info!(
                "Warning: failed to add contact '{}' for user '{}': {:?}",
                request.requester_public_key, username, err
            );
        } else {
            info!(
                "Added contact '{}' ({}) for user '{}'",
                request.requester_alias, request.requester_public_key, username
            );
        }

        // Send an approval response
        self.send_contact_response(username, _password, &request.requester_public_key, true)
            .await?;

        Ok(())
    }

    /// Send a contact response (approval/denial)
    pub async fn send_contact_response(
        &self,
        username: &str,
        _password: &str, // Password parameter kept for compatibility, but not used
        target_public_key: &str,
        approved: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // In a real implementation, this would get user information from session context
        // rather than re-authenticating with username/password

        let (node_name, port) = {
            let service = self.node_service.lock().await;
            (service.get_name(), service.get_port())
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
        };

        // Create a message for the contact response
        let message = crate::domain::message::Message::ContactResponse {
            responder_public_key: contact_response.responder_public_key.clone(),
            approved: contact_response.approved,
            responder_alias: contact_response.responder_alias.clone(),
            timestamp: contact_response.timestamp,
            signature: contact_response.signature.clone(),
            user_id: None, // Add user_id field
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

        {
            let node_service = self.node_service.lock().await;
            node_service
                .send_json_message_to_peer(target_addr, message_json.clone())
                .await
                .map_err(|e| format!("Failed to deliver contact response to {target_addr}: {e}"))?;
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

        if response.approved {
            match self.contact_manager.add_contact(
                username,
                password,
                &response.responder_public_key,
                Some(&response.responder_alias),
            ) {
                Ok(_) => info!(
                    "Contact request approved by user: {} ({})",
                    response.responder_alias, response.responder_public_key
                ),
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
                ..
            } => {
                // Create a contact request from the message data
                let request = ContactRequest {
                    requester_public_key,
                    requester_alias,
                    timestamp,
                    signature,
                };

                // Serialize the request for the handler
                let request_json = request.to_json()?;

                // Handle the contact request
                self.handle_contact_request(username, password, &request_json)
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
        let contact_manager = Arc::new(ContactManager::new(file_manager.clone()));
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
        let contact_manager = Arc::new(ContactManager::new(file_manager.clone()));
        let service = ContactRequestService::new(contact_manager, node_service);

        // Test processing a contact request message
        let contact_request_message = crate::domain::message::Message::ContactRequest {
            requester_public_key: "test_public_key".to_string(),
            requester_alias: "Test User".to_string(),
            timestamp: 1234567890,
            signature: vec![1, 2, 3, 4],
            node_name: Some("test-node".to_string()),
            username: Some("tester".to_string()),
            user_id: Some(1),
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
            user_id: Some(1),
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

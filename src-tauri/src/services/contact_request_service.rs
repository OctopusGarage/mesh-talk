use crate::contacts::manager::ContactManager;
use crate::contacts::service::ContactRequestService as InnerContactRequestService;
use crate::identity::manager::IdentityManager;
use crate::services::node_service::NodeService;
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

static INSTANCE: OnceLock<Arc<InnerContactRequestService>> = OnceLock::new();

#[derive(Clone)]
pub struct ContactRequestService {
    inner: Arc<InnerContactRequestService>,
}

impl ContactRequestService {
    /// Initialize the global contact request service instance
    pub fn init_global(
        file_manager: crate::storage::file_manager::FileManager,
        node_service: Arc<Mutex<NodeService>>,
    ) -> Result<(), String> {
        let identity_manager = Arc::new(IdentityManager::new(file_manager.clone()));
        let contact_manager = Arc::new(ContactManager::new(
            file_manager,
            (*identity_manager).clone(),
        ));

        // Note: The original ContactRequestService expects a Libp2pNetwork which is not available
        // For now, we'll create the service without the network and handle it differently
        let inner = InnerContactRequestService::new(contact_manager, node_service);

        INSTANCE
            .set(Arc::new(inner))
            .map_err(|_| "ContactRequestService already initialized".to_string())?;

        Ok(())
    }

    /// Get the global contact request service instance
    pub fn global() -> &'static Arc<InnerContactRequestService> {
        INSTANCE
            .get()
            .expect("ContactRequestService not initialized")
    }

    /// Send a contact request to another user
    pub async fn send_contact_request(
        &self,
        username: &str,
        password: &str,
        target_public_key: &str,
        alias: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .send_contact_request(username, password, target_public_key, alias)
            .await
    }

    /// Send a contact request to another user with user ID
    pub async fn send_contact_request_with_user_id(
        &self,
        username: &str,
        password: &str,
        target_public_key: &str,
        alias: Option<&str>,
        user_id: String,
        remote_username: Option<String>,
        remote_ip: Option<String>,
        remote_port: Option<u16>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .send_contact_request_with_user_id(
                username,
                password,
                target_public_key,
                alias,
                user_id,
                remote_username,
                remote_ip,
                remote_port,
            )
            .await
    }

    /// Handle an incoming contact request
    pub async fn handle_contact_request(
        &self,
        username: &str,
        password: &str,
        request_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .handle_contact_request(username, password, request_json)
            .await
    }

    /// Send a contact response (approval/denial)
    pub async fn send_contact_response(
        &self,
        username: &str,
        password: &str,
        target_public_key: &str,
        approved: bool,
        target_user_id: Option<String>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .send_contact_response(
                username,
                password,
                target_public_key,
                approved,
                target_user_id,
            )
            .await
    }

    /// Handle an incoming contact response
    pub async fn handle_contact_response(
        &self,
        username: &str,
        password: &str,
        response_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .handle_contact_response(username, password, response_json)
            .await
    }

    /// Process an incoming network message
    pub async fn process_incoming_message(
        &self,
        username: &str,
        password: &str,
        message_json: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.inner
            .process_incoming_message(username, password, message_json)
            .await
    }
}

use crate::contacts::contact::Contact;
use crate::contacts::errors::ContactError;
use crate::contacts::manager::ContactManager;

use std::sync::Arc;
use tracing::{debug, error, info};

/// Contact discovery service
pub struct ContactDiscovery {
    contact_manager: Arc<ContactManager>,
}

impl ContactDiscovery {
    /// Create a new contact discovery service
    pub fn new(contact_manager: Arc<ContactManager>) -> Self {
        ContactDiscovery { contact_manager }
    }

    /// Handle network events and discover contacts
    pub fn handle_network_event(
        &self,
        _username: &str,
        _password: &str,
        event: &str, // Simplified event handling
    ) -> Result<(), ContactError> {
        // For now, we'll just log the event
        debug!("Received network event: {}", event);
        Ok(())
    }

    /// Add a discovered peer as a contact
    #[allow(dead_code)]
    pub fn add_discovered_contact(
        &self,
        username: &str,
        password: &str,
        peer_id: &str,
        addresses: Vec<String>, // Use String instead of libp2p::Multiaddr
    ) -> Result<(), ContactError> {
        // Validate the peer ID
        if !Contact::validate_public_key(peer_id) {
            return Err(ContactError::InvalidPublicKey);
        }

        // Check if the contact already exists
        if self
            .contact_manager
            .contact_exists(username, password, peer_id)?
        {
            // Contact already exists, nothing to do
            return Ok(());
        }

        // Try to add the contact with a default alias
        let alias = Some(format!("User-{}", &peer_id[..8.min(peer_id.len())]));
        self.contact_manager.add_discovered_contact(
            username,
            password,
            peer_id,
            alias.as_deref(),
        )?;

        info!("Discovered and added new contact: {}", peer_id);
        debug!("  Addresses: {:?}", addresses);

        Ok(())
    }

    /// Handle peer expiration
    #[allow(dead_code)]
    pub fn handle_peer_expiration(
        &self,
        _username: &str,
        _password: &str,
        _peer_id: &str,
    ) -> Result<(), ContactError> {
        // In the future, we might want to mark contacts as offline or remove them
        // For now, we'll just log the event
        debug!("Peer expired: {}", _peer_id);
        Ok(())
    }

    /// Manually add a contact by peer ID
    pub fn manually_add_contact(
        &self,
        username: &str,
        password: &str,
        peer_id: &str,
        alias: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the peer ID
        if !Contact::validate_public_key(peer_id) {
            return Err(ContactError::InvalidPublicKey);
        }

        // Check if the contact already exists
        if self
            .contact_manager
            .contact_exists(username, password, peer_id)?
        {
            return Err(ContactError::ContactAlreadyExists(peer_id.to_string()));
        }

        // Add the contact
        self.contact_manager
            .add_contact(username, password, peer_id, alias)?;

        info!("Manually added contact: {}", peer_id);
        Ok(())
    }

    /// Discover contacts from a list of peer IDs
    pub fn discover_contacts_from_list(
        &self,
        username: &str,
        password: &str,
        peer_ids: &[String],
    ) -> Result<Vec<String>, ContactError> {
        let mut discovered_contacts = Vec::new();

        for peer_id in peer_ids {
            // Validate the peer ID
            if !Contact::validate_public_key(peer_id) {
                continue;
            }

            // Check if the contact already exists
            if self
                .contact_manager
                .contact_exists(username, password, peer_id)?
            {
                continue;
            }

            // Try to add the contact with a default alias
            let alias = Some(format!("User-{}", &peer_id[..8.min(peer_id.len())]));
            match self.contact_manager.add_discovered_contact(
                username,
                password,
                peer_id,
                alias.as_deref(),
            ) {
                Ok(()) => {
                    info!("Discovered and added new contact: {}", peer_id);
                    discovered_contacts.push(peer_id.clone());
                }
                Err(e) => {
                    error!("Failed to add discovered contact {}: {:?}", peer_id, e);
                }
            }
        }

        Ok(discovered_contacts)
    }
}

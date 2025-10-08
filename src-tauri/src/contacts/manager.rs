use crate::contacts::contact::Contact;
use crate::contacts::errors::ContactError;

use crate::contacts::public_key_encryption::PublicKeyFileManager;
use crate::identity::manager::IdentityManager;
use crate::storage::file_manager::FileManager;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactList {
    pub contacts: HashMap<String, Contact>, // ip:port -> Contact
                                            // Removed alias_index since we're not using aliases anymore
}

impl Default for ContactList {
    fn default() -> Self {
        Self::new()
    }
}

impl ContactList {
    pub fn new() -> Self {
        ContactList {
            contacts: HashMap::new(),
            // Removed alias_index since we're not using aliases anymore
        }
    }

    // Removed alias_index related methods since we're not using aliases anymore
}

#[derive(Clone)]
pub struct ContactManager {
    // Keep the original file manager for other operations
    file_manager: FileManager,
    // Add the public key encryption file manager for contacts
    public_key_file_manager: PublicKeyFileManager,
}

impl ContactManager {
    pub fn new(file_manager: FileManager, identity_manager: IdentityManager) -> Self {
        let public_key_file_manager = PublicKeyFileManager::new(
            file_manager.base_path().to_path_buf(), // Get the base path from the file manager
            identity_manager,
        );
        ContactManager {
            file_manager,
            public_key_file_manager,
        }
    }

    pub fn add_contact(
        &self,
        username: &str,
        password: &str,
        contact_ip: &str,
        contact_port: u16,
        contact_username: &str,
        contact_user_id: Option<String>,
    ) -> Result<(), ContactError> {
        log::info!("=== START ADD_CONTACT IN MANAGER ===");
        log::info!("Username: {}, Contact IP: {}, Contact Port: {}, Contact username: {}, Contact user ID: {:?}", 
                  username, contact_ip, contact_port, contact_username, contact_user_id);

        // Validate the contact IP
        if !Contact::validate_ip(contact_ip) {
            log::warn!("Invalid contact IP: {}", contact_ip);
            return Err(ContactError::InvalidPublicKey);
        }

        // Validate the contact port
        if !Contact::validate_port(contact_port) {
            log::warn!("Invalid contact port: {}", contact_port);
            return Err(ContactError::InvalidPublicKey);
        }

        log::info!(
            "Validated contact data - IP: {}, Port: {}, Username: {}",
            contact_ip,
            contact_port,
            contact_username
        );

        // Load existing contacts for deduplication
        log::info!("Loading existing contacts...");
        let mut contact_list = self.load_contacts(username, password)?;
        log::info!("Loaded {} existing contacts", contact_list.contacts.len());

        // Deduplicate by user_id when provided
        if let Some(ref user_id) = contact_user_id {
            if let Some((existing_key, mut existing_contact)) = contact_list
                .contacts
                .iter()
                .find(|(_, contact)| {
                    contact
                        .user_id
                        .as_ref()
                        .map(|id| id == user_id)
                        .unwrap_or(false)
                })
                .map(|(key, contact)| (key.clone(), contact.clone()))
            {
                log::info!(
                    "Contact with user_id {} already exists (key: {}). Updating entry.",
                    user_id,
                    existing_key
                );

                existing_contact.ip = contact_ip.to_string();
                existing_contact.port = contact_port;
                existing_contact.username = contact_username.to_string();
                existing_contact.user_id = Some(user_id.clone());

                contact_list.contacts.remove(&existing_key);
                let contact_key = format!("{}:{}", contact_ip, contact_port);
                contact_list
                    .contacts
                    .insert(contact_key, existing_contact.clone());

                log::info!("Saving updated contact for existing user_id {}", user_id);
                self.save_contacts(username, password, &contact_list)?;
                log::info!("=== UPDATED EXISTING CONTACT BY USER_ID ===");
                return Ok(());
            }
        }

        let contact_address = format!("{}:{}", contact_ip, contact_port);
        if contact_list.contacts.contains_key(&contact_address) {
            log::warn!("Contact already exists with address: {}", contact_address);
            return Err(ContactError::ContactAlreadyExists(contact_address));
        }

        // Create the contact
        log::info!("Creating contact object...");
        let contact = Contact::new(
            contact_ip.to_string(),
            contact_port,
            contact_username.to_string(),
            contact_user_id.clone(),
        );
        log::info!(
            "Created contact - IP: {}, Port: {}, Username: {}, User ID: {:?}",
            contact.ip,
            contact.port,
            contact.username,
            contact.user_id
        );

        // Add the new contact using IP:Port as the key
        log::info!(
            "Adding new contact to contact list with key: {}...",
            contact_address
        );
        contact_list
            .contacts
            .insert(contact_address, contact.clone());
        log::info!("Added contact to contact list");

        // Save the updated contacts
        log::info!("Saving updated contacts...");
        match self.save_contacts(username, password, &contact_list) {
            Ok(_) => {
                log::info!("=== SUCCESSFULLY ADDED CONTACT ===");
                Ok(())
            }
            Err(e) => {
                log::error!("Failed to save contacts: {}", e);
                Err(e)
            }
        }
    }

    pub fn remove_contact(
        &self,
        username: &str,
        password: &str,
        contact_address: &str, // IP:Port format
    ) -> Result<(), ContactError> {
        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_address) {
            let contact_clone = contact.clone();

            // Remove the contact
            contact_list.contacts.remove(contact_address);

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;
            Ok(())
        } else {
            Err(ContactError::ContactNotFound(contact_address.to_string()))
        }
    }

    pub fn get_contact(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
    ) -> Result<Contact, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Get the contact
        contact_list
            .contacts
            .get(contact_public_key)
            .cloned()
            .ok_or_else(|| ContactError::ContactNotFound(contact_public_key.to_string()))
    }

    pub fn list_contacts(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Vec<Contact>, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Return the contacts as a vector
        Ok(contact_list.contacts.values().cloned().collect())
    }

    pub fn update_contact_username(
        &self,
        username: &str,
        password: &str,
        contact_address: &str, // IP:Port format
        new_username: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the new username
        let username_string = new_username.map(|s| s.to_string());
        if !Contact::validate_username(&username_string.clone().unwrap_or_default()) {
            log::warn!("Invalid username: {:?}", username_string);
            return Err(ContactError::InvalidAlias);
        }

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_address) {
            let mut contact_clone = contact.clone();

            // Remove the group index
            // (Note: We're not using a group index in this implementation)

            // Update the username in the contact
            contact_clone.username = username_string.unwrap_or_default();

            // Update the contact in the list
            contact_list
                .contacts
                .insert(contact_address.to_string(), contact_clone.clone());

            // Add to new group index
            // (Note: We're not using a group index in this implementation)

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;

            Ok(())
        } else {
            Err(ContactError::ContactNotFound(contact_address.to_string()))
        }
    }

    pub fn add_discovered_contact(
        &self,
        username: &str,
        password: &str,
        contact_address: &str, // IP:Port format
        default_alias: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the contact address (IP:Port format)
        let parts: Vec<&str> = contact_address.split(':').collect();
        if parts.len() != 2 {
            log::warn!("Invalid contact address format: {}", contact_address);
            return Err(ContactError::InvalidPublicKey);
        }

        let ip = parts[0];
        let port_str = parts[1];
        let port = port_str.parse::<u16>().map_err(|_| {
            log::warn!("Invalid port in contact address: {}", contact_address);
            ContactError::InvalidPublicKey
        })?;

        if !Contact::validate_ip(ip) {
            log::warn!("Invalid IP in contact address: {}", contact_address);
            return Err(ContactError::InvalidPublicKey);
        }

        if !Contact::validate_port(port) {
            log::warn!("Invalid port in contact address: {}", contact_address);
            return Err(ContactError::InvalidPublicKey);
        }

        // Check if the contact already exists
        if self.contact_exists(username, password, contact_address)? {
            log::warn!("Contact already exists with address: {}", contact_address);
            return Err(ContactError::ContactAlreadyExists(
                contact_address.to_string(),
            ));
        }

        // Create the contact with default alias
        let contact = Contact::new(
            ip.to_string(),                          // IP address
            port,                                    // Port number
            default_alias.unwrap_or(ip).to_string(), // Use IP as default username if not provided
            None,
        );

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Add the new contact using IP:Port as the key
        contact_list
            .contacts
            .insert(contact_address.to_string(), contact.clone());

        // Save the updated contacts
        self.save_contacts(username, password, &contact_list)?;

        Ok(())
    }

    pub fn contact_exists(
        &self,
        username: &str,
        password: &str,
        contact_address: &str, // IP:Port format
    ) -> Result<bool, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists by address (IP:Port)
        Ok(contact_list.contacts.contains_key(contact_address))
    }

    /// Search contacts by alias or public key
    pub fn search_contacts(
        &self,
        username: &str,
        password: &str,
        query: &str,
    ) -> Result<Vec<Contact>, ContactError> {
        let normalized_query = query.trim().to_lowercase();
        if normalized_query.is_empty() {
            return Ok(vec![]);
        }

        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Filter contacts that match the query
        let matching_contacts = contact_list
            .contacts
            .values()
            .filter(|contact| {
                contact
                    .get_address()
                    .to_lowercase()
                    .contains(&normalized_query)
                    || contact.username.to_lowercase().contains(&normalized_query)
            })
            .cloned()
            .collect();

        Ok(matching_contacts)
    }

    fn load_contacts(&self, username: &str, password: &str) -> Result<ContactList, ContactError> {
        log::info!(
            "Loading contacts for user '{}' from file: users/{}/data/contacts.json",
            username,
            username
        );

        // Try to read the contacts file using public key encryption
        let result: Result<ContactList, _> = self.public_key_file_manager.read_encrypted_file(
            username,
            password,
            "data/contacts.json",
        );

        match result {
            Ok(mut contacts) => {
                log::info!(
                    "Successfully loaded {} contacts for user '{}'",
                    contacts.contacts.len(),
                    username
                );

                // Log details about each contact being loaded
                for (public_key, contact) in &contacts.contacts {
                    log::debug!(
                        "Loaded contact - Public Key: {}, User ID: {:?}, Username: {}, Added: {}",
                        public_key,
                        contact.user_id,
                        contact.username,
                        contact.added_at
                    );
                }

                // Removed alias index rebuild since we're not using aliases anymore

                Ok(contacts)
            }
            Err(crate::storage::errors::StorageError::FileNotFound(path)) => {
                log::info!(
                    "Contacts file not found for user '{}', path: {} - returning empty contact list",
                    username,
                    path.display()
                );
                Ok(ContactList::new())
            }
            Err(crate::storage::errors::StorageError::Deserialization(_)) => {
                log::warn!(
                    "Failed to deserialize contacts file for user '{}' - returning empty contact list",
                    username
                );
                // If deserialization fails, return an empty contact list
                // This can happen if the file format has changed
                Ok(ContactList::new())
            }
            Err(e) => {
                log::error!("Failed to load contacts for user '{}': {}", username, e);
                Err(ContactError::StorageError(e.to_string()))
            }
        }
    }

    /// Add a contact to a group
    pub fn add_contact_to_group(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
        group: &str,
    ) -> Result<(), ContactError> {
        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_public_key) {
            let mut contact_clone = contact.clone();

            // Add the group to the contact
            contact_clone.add_group(group);

            // Update the contact in the list
            contact_list
                .contacts
                .insert(contact_public_key.to_string(), contact_clone.clone());

            // Update the alias index

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;
            Ok(())
        } else {
            Err(ContactError::ContactNotFound(
                contact_public_key.to_string(),
            ))
        }
    }

    /// Remove a contact from a group
    pub fn remove_contact_from_group(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
        group: &str,
    ) -> Result<(), ContactError> {
        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_public_key) {
            let mut contact_clone = contact.clone();

            // Remove the group from the contact
            contact_clone.remove_group(group);

            // Update the contact in the list
            contact_list
                .contacts
                .insert(contact_public_key.to_string(), contact_clone.clone());

            // Update the alias index

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;
            Ok(())
        } else {
            Err(ContactError::ContactNotFound(
                contact_public_key.to_string(),
            ))
        }
    }

    /// Get all contacts in a specific group
    pub fn get_contacts_in_group(
        &self,
        username: &str,
        password: &str,
        group: &str,
    ) -> Result<Vec<Contact>, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Filter contacts that belong to the specified group
        let contacts_in_group = contact_list
            .contacts
            .values()
            .filter(|contact| contact.in_group(group))
            .cloned()
            .collect();

        Ok(contacts_in_group)
    }

    /// Get all groups for a specific contact
    pub fn get_contact_groups(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
    ) -> Result<Vec<String>, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Get the contact and return its groups
        if let Some(contact) = contact_list.contacts.get(contact_public_key) {
            Ok(contact.groups.clone())
        } else {
            Err(ContactError::ContactNotFound(
                contact_public_key.to_string(),
            ))
        }
    }

    /// Get all groups that the user has contacts in
    pub fn get_all_groups(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Vec<String>, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Collect all unique groups
        let mut groups = std::collections::HashSet::new();
        for contact in contact_list.contacts.values() {
            for group in &contact.groups {
                groups.insert(group.clone());
            }
        }

        // Convert to sorted vector
        let mut group_list: Vec<String> = groups.into_iter().collect();
        group_list.sort();

        Ok(group_list)
    }

    /// Export contacts to a JSON string
    pub fn export_contacts(&self, username: &str, password: &str) -> Result<String, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Serialize the contacts to JSON
        let json = serde_json::to_string(&contact_list.contacts).map_err(|e| {
            ContactError::StorageError(format!("Failed to serialize contacts: {}", e))
        })?;

        Ok(json)
    }

    /// Import contacts from a JSON string
    pub fn import_contacts(
        &self,
        username: &str,
        password: &str,
        json: &str,
    ) -> Result<usize, ContactError> {
        // Deserialize the contacts from JSON
        let contacts: HashMap<String, Contact> = serde_json::from_str(json).map_err(|e| {
            ContactError::StorageError(format!("Failed to deserialize contacts: {}", e))
        })?;

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        let mut imported_count = 0;

        // Add each contact, avoiding duplicates
        for (public_key, contact) in contacts {
            if let std::collections::hash_map::Entry::Vacant(e) =
                contact_list.contacts.entry(public_key)
            {
                e.insert(contact.clone());
                imported_count += 1;
            }
        }

        // Save the updated contacts
        self.save_contacts(username, password, &contact_list)?;

        Ok(imported_count)
    }

    fn save_contacts(
        &self,
        username: &str,
        password: &str,
        contact_list: &ContactList,
    ) -> Result<(), ContactError> {
        // Log what we're saving and where
        log::info!(
            "Saving contacts for user '{}' to file: users/{}/data/contacts.json, contact count: {}",
            username,
            username,
            contact_list.contacts.len()
        );

        // Log details about each contact being saved
        for (public_key, contact) in &contact_list.contacts {
            log::debug!(
                "Saving contact - Public Key: {}, User ID: {:?}, Username: {}, Added: {}",
                public_key,
                contact.user_id,
                contact.username,
                contact.added_at
            );
        }

        // Save the contacts file using public key encryption
        let result = self
            .public_key_file_manager
            .write_encrypted_file(username, password, "data/contacts.json", contact_list)
            .map_err(|e| ContactError::StorageError(e.to_string()));

        match &result {
            Ok(_) => log::info!(
                "Successfully saved {} contacts for user '{}' to data/contacts.json",
                contact_list.contacts.len(),
                username
            ),
            Err(e) => log::error!("Failed to save contacts for user '{}': {}", username, e),
        }

        result
    }
}

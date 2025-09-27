use crate::contacts::contact::Contact;
use crate::contacts::errors::ContactError;

use crate::storage::file_manager::FileManager;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactList {
    pub contacts: HashMap<String, Contact>, // public_key -> Contact
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub alias_index: HashMap<String, HashSet<String>>, // lowercase_alias -> public_keys
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
            alias_index: HashMap::new(),
        }
    }

    /// Update the alias index when a contact is added or modified
    pub fn update_alias_index(&mut self, contact: &Contact) {
        if let Some(alias) = &contact.alias {
            let lowercase_alias = alias.to_lowercase();
            let public_keys = self.alias_index.entry(lowercase_alias).or_default();
            public_keys.insert(contact.public_key.clone());
        }
    }

    /// Remove a contact from the alias index
    pub fn remove_from_alias_index(&mut self, contact: &Contact) {
        if let Some(alias) = &contact.alias {
            let lowercase_alias = alias.to_lowercase();
            if let Some(public_keys) = self.alias_index.get_mut(&lowercase_alias) {
                public_keys.remove(&contact.public_key);
                // Clean up empty entries
                if public_keys.is_empty() {
                    self.alias_index.remove(&lowercase_alias);
                }
            }
        }
    }

    /// Find contacts by alias using the index
    pub fn find_by_alias(&self, alias: &str) -> Vec<&Contact> {
        let lowercase_alias = alias.to_lowercase();
        if let Some(public_keys) = self.alias_index.get(&lowercase_alias) {
            public_keys
                .iter()
                .filter_map(|public_key| self.contacts.get(public_key))
                .collect()
        } else {
            vec![]
        }
    }

    /// Rebuild the alias index from the contacts
    pub fn rebuild_alias_index(&mut self) {
        // Clear the existing index
        self.alias_index.clear();

        // Create a vector of contacts to avoid borrowing issues
        let contacts: Vec<Contact> = self.contacts.values().cloned().collect();

        // Rebuild from contacts
        for contact in contacts {
            self.update_alias_index(&contact);
        }
    }
}

#[derive(Clone)]
pub struct ContactManager {
    file_manager: FileManager,
}

impl ContactManager {
    pub fn new(file_manager: FileManager) -> Self {
        ContactManager { file_manager }
    }

    pub fn add_contact(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
        alias: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the public key
        if !Contact::validate_public_key(contact_public_key) {
            return Err(ContactError::InvalidPublicKey);
        }

        // Validate the alias
        let alias_string = alias.map(|s| s.to_string());
        if !Contact::validate_alias(&alias_string) {
            return Err(ContactError::InvalidAlias);
        }

        // Check if the contact already exists
        if self.contact_exists(username, password, contact_public_key)? {
            return Err(ContactError::ContactAlreadyExists(
                contact_public_key.to_string(),
            ));
        }

        // Create the contact
        let contact = Contact::new(contact_public_key.to_string(), alias_string);

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Add the new contact
        contact_list
            .contacts
            .insert(contact_public_key.to_string(), contact.clone());

        // Update the alias index
        contact_list.update_alias_index(&contact);

        // Save the updated contacts
        self.save_contacts(username, password, &contact_list)?;

        Ok(())
    }

    pub fn remove_contact(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
    ) -> Result<(), ContactError> {
        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_public_key) {
            let contact_clone = contact.clone();

            // Remove from alias index
            contact_list.remove_from_alias_index(&contact_clone);

            // Remove the contact
            contact_list.contacts.remove(contact_public_key);

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;
            Ok(())
        } else {
            Err(ContactError::ContactNotFound(
                contact_public_key.to_string(),
            ))
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

    pub fn update_contact_alias(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
        alias: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the alias
        let alias_string = alias.map(|s| s.to_string());
        if !Contact::validate_alias(&alias_string) {
            return Err(ContactError::InvalidAlias);
        }

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists and get its current state
        if let Some(contact) = contact_list.contacts.get(contact_public_key) {
            let mut contact_clone = contact.clone();

            // Remove from old alias index
            contact_list.remove_from_alias_index(&contact_clone);

            // Update the alias in the contact
            contact_clone.alias = alias_string;

            // Update the contact in the list
            contact_list
                .contacts
                .insert(contact_public_key.to_string(), contact_clone.clone());

            // Add to new alias index
            contact_list.update_alias_index(&contact_clone);

            // Save the updated contacts
            self.save_contacts(username, password, &contact_list)?;

            Ok(())
        } else {
            Err(ContactError::ContactNotFound(
                contact_public_key.to_string(),
            ))
        }
    }

    /// Add a discovered contact with default settings
    pub fn add_discovered_contact(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
        default_alias: Option<&str>,
    ) -> Result<(), ContactError> {
        // Validate the public key
        if !Contact::validate_public_key(contact_public_key) {
            return Err(ContactError::InvalidPublicKey);
        }

        // Check if the contact already exists
        if self.contact_exists(username, password, contact_public_key)? {
            return Err(ContactError::ContactAlreadyExists(
                contact_public_key.to_string(),
            ));
        }

        // Create the contact with default alias
        let contact = Contact::new(
            contact_public_key.to_string(),
            default_alias.map(|s| s.to_string()),
        );

        // Load existing contacts
        let mut contact_list = self.load_contacts(username, password)?;

        // Add the new contact
        contact_list
            .contacts
            .insert(contact_public_key.to_string(), contact.clone());

        // Update the alias index
        contact_list.update_alias_index(&contact);

        // Save the updated contacts
        self.save_contacts(username, password, &contact_list)?;

        Ok(())
    }

    pub fn contact_exists(
        &self,
        username: &str,
        password: &str,
        contact_public_key: &str,
    ) -> Result<bool, ContactError> {
        // Load existing contacts
        let contact_list = self.load_contacts(username, password)?;

        // Check if the contact exists
        Ok(contact_list.contacts.contains_key(contact_public_key))
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
                    .public_key
                    .to_lowercase()
                    .contains(&normalized_query)
                    || contact
                        .alias
                        .as_ref()
                        .map(|alias| alias.to_lowercase().contains(&normalized_query))
                        .unwrap_or(false)
            })
            .cloned()
            .collect();

        Ok(matching_contacts)
    }

    fn load_contacts(&self, username: &str, password: &str) -> Result<ContactList, ContactError> {
        // Try to read the contacts file
        let result: Result<ContactList, _> =
            self.file_manager
                .read_encrypted_file(username, "data/contacts.json", password);

        match result {
            Ok(mut contacts) => {
                // If the alias index is empty, rebuild it from the contacts
                if contacts.alias_index.is_empty() && !contacts.contacts.is_empty() {
                    contacts.rebuild_alias_index();
                }
                Ok(contacts)
            }
            Err(crate::storage::errors::StorageError::FileNotFound(_)) => {
                // If the file doesn't exist, return an empty contact list
                Ok(ContactList::new())
            }
            Err(crate::storage::errors::StorageError::Deserialization(_)) => {
                // If deserialization fails, return an empty contact list
                // This can happen if the file format has changed
                Ok(ContactList::new())
            }
            Err(e) => Err(ContactError::StorageError(e.to_string())),
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
            contact_list.update_alias_index(&contact_clone);

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
            contact_list.update_alias_index(&contact_clone);

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
                contact_list.update_alias_index(&contact);
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
        // Save the contacts file
        self.file_manager
            .write_encrypted_file(username, "data/contacts.json", contact_list, password)
            .map_err(|e| ContactError::StorageError(e.to_string()))
    }
}

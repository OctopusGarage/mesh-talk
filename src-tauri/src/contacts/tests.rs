#[cfg(test)]
mod tests {
    use crate::contacts::contact::Contact;
    use crate::contacts::discovery::ContactDiscovery;
    use crate::contacts::manager::ContactManager;
    use crate::identity::manager::IdentityManager;
    use crate::storage::file_manager::FileManager;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[test]
    fn test_contact_creation() {
        let public_key = "test_public_key".to_string();
        let alias = Some("Test User".to_string());
        let contact = Contact::new(public_key.clone(), alias.clone(), None);

        assert_eq!(contact.public_key, public_key);
        assert_eq!(contact.alias, alias);
        assert!(!contact.is_online);
    }

    #[test]
    fn test_contact_validation() {
        assert!(Contact::validate_public_key("valid_public_key"));
        assert!(!Contact::validate_public_key(""));
        assert!(Contact::validate_alias(&None));
        assert!(Contact::validate_alias(&Some("valid_alias".to_string())));
        assert!(!Contact::validate_alias(&Some("a".repeat(51)))); // Too long
    }

    #[test]
    fn test_contact_manager_add_contact() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        let public_key = "contact_public_key";
        let alias = Some("Test Contact");

        // Add a contact
        let result = contact_manager.add_contact(username, password, public_key, alias, None);
        assert!(result.is_ok());

        // Try to add the same contact again (should fail)
        let result = contact_manager.add_contact(username, password, public_key, alias, None);
        assert!(result.is_err());

        // Get the contact and verify it exists
        let contact = contact_manager
            .get_contact(username, password, public_key)
            .unwrap();
        assert_eq!(contact.public_key, public_key);
        assert_eq!(contact.alias, alias.map(|s| s.to_string()));
    }

    #[test]
    fn test_contact_manager_get_and_list_contacts() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        let public_key1 = "contact_public_key1";
        let alias1 = Some("Test Contact 1");
        let public_key2 = "contact_public_key2";
        let alias2 = Some("Test Contact 2");

        // Add contacts
        contact_manager
            .add_contact(username, password, public_key1, alias1, None)
            .unwrap();
        contact_manager
            .add_contact(username, password, public_key2, alias2, None)
            .unwrap();

        // Get a specific contact
        let contact = contact_manager.get_contact(username, password, public_key1);
        assert!(contact.is_ok());
        let contact = contact.unwrap();
        assert_eq!(contact.public_key, public_key1);
        assert_eq!(contact.alias, Some("Test Contact 1".to_string()));

        // List all contacts
        let contacts = contact_manager.list_contacts(username, password);
        assert!(contacts.is_ok());
        let contacts = contacts.unwrap();
        assert_eq!(contacts.len(), 2);
    }

    #[test]
    fn test_contact_manager_remove_contact() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        let public_key = "contact_public_key";
        let alias = Some("Test Contact");

        // Add a contact
        contact_manager
            .add_contact(username, password, public_key, alias)
            .unwrap();

        // Remove the contact
        let result = contact_manager.remove_contact(username, password, public_key);
        assert!(result.is_ok());

        // Try to get the removed contact (should fail)
        let result = contact_manager.get_contact(username, password, public_key);
        assert!(result.is_err());
    }

    #[test]
    fn test_contact_manager_update_alias() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Add a contact
        let public_key = "contact_public_key";
        let alias = Some("Test Contact");
        contact_manager
            .add_contact(username, password, public_key, alias)
            .unwrap();

        // Update the alias
        contact_manager
            .update_contact_alias(username, password, public_key, Some("Updated Contact"))
            .unwrap();

        // Get the contact and verify the alias was updated
        let contact = contact_manager
            .get_contact(username, password, public_key)
            .unwrap();
        assert_eq!(contact.alias, Some("Updated Contact".to_string()));
    }

    #[test]
    fn test_contact_manager_add_discovered_contact() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        let public_key = "discovered_public_key";
        let alias = Some("Discovered Contact");

        // Add a discovered contact
        let result = contact_manager.add_discovered_contact(username, password, public_key, alias);
        assert!(result.is_ok());

        // Try to add the same contact again (should fail)
        let result = contact_manager.add_discovered_contact(username, password, public_key, alias);
        assert!(result.is_err());

        // Get the contact and verify it exists
        let contact = contact_manager
            .get_contact(username, password, public_key)
            .unwrap();
        assert_eq!(contact.public_key, public_key);
        assert_eq!(contact.alias, alias.map(|s| s.to_string()));
    }

    #[test]
    fn test_contact_manager_search_contacts() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Add some contacts
        contact_manager
            .add_contact(username, password, "public_key_1", Some("Alice"), None)
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_2", Some("Bob Smith"), None)
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_3", Some("Charlie"), None)
            .unwrap();

        // Search for contacts by alias
        let results = contact_manager
            .search_contacts(username, password, "Alice")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].alias, Some("Alice".to_string()));

        // Search for contacts by partial alias
        let results = contact_manager
            .search_contacts(username, password, "Bob")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].alias, Some("Bob Smith".to_string()));

        // Search for contacts by public key
        let results = contact_manager
            .search_contacts(username, password, "public_key_1")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].public_key, "public_key_1");

        // Search with empty query
        let results = contact_manager
            .search_contacts(username, password, "")
            .unwrap();
        assert_eq!(results.len(), 0);

        // Search with whitespace
        let results = contact_manager
            .search_contacts(username, password, "  Alice  ")
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].alias, Some("Alice".to_string()));

        // Search with no matches
        let results = contact_manager
            .search_contacts(username, password, "NonExistent")
            .unwrap();
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_contact_manager_alias_indexing() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Add contacts with aliases
        contact_manager
            .add_contact(username, password, "public_key_1", Some("Alice"), None)
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_2", Some("Alice"), None)
            .unwrap(); // Same alias
        contact_manager
            .add_contact(username, password, "public_key_3", Some("Bob"), None)
            .unwrap();

        // Check that we can list all contacts
        let contacts = contact_manager.list_contacts(username, password).unwrap();
        assert_eq!(contacts.len(), 3);

        // Update an alias
        contact_manager
            .update_contact_alias(username, password, "public_key_1", Some("Charlie"))
            .unwrap();

        // Check that the contact was updated
        let contact = contact_manager
            .get_contact(username, password, "public_key_1")
            .unwrap();
        assert_eq!(contact.alias, Some("Charlie".to_string()));

        // Remove a contact
        contact_manager
            .remove_contact(username, password, "public_key_2")
            .unwrap();

        // Check that we have fewer contacts
        let contacts = contact_manager.list_contacts(username, password).unwrap();
        assert_eq!(contacts.len(), 2);
    }

    #[test]
    fn test_contact_manager_export_import() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Add some contacts
        contact_manager
            .add_contact(username, password, "public_key_1", Some("Alice"))
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_2", Some("Bob"))
            .unwrap();

        // Export contacts
        let exported = contact_manager.export_contacts(username, password).unwrap();
        assert!(!exported.is_empty());

        // Create a new user for import testing
        let username2 = "testuser2";
        let _user2 = identity_manager.register_user(username2, password).unwrap();

        // Import contacts
        let imported_count = contact_manager
            .import_contacts(username2, password, &exported)
            .unwrap();
        assert_eq!(imported_count, 2);

        // Check that the contacts were imported
        let contacts = contact_manager.list_contacts(username2, password).unwrap();
        assert_eq!(contacts.len(), 2);

        // Check that the contacts have the correct data
        let contact1 = contact_manager
            .get_contact(username2, password, "public_key_1")
            .unwrap();
        assert_eq!(contact1.alias, Some("Alice".to_string()));

        let contact2 = contact_manager
            .get_contact(username2, password, "public_key_2")
            .unwrap();
        assert_eq!(contact2.alias, Some("Bob".to_string()));

        // Test importing again (should not create duplicates)
        let imported_count = contact_manager
            .import_contacts(username2, password, &exported)
            .unwrap();
        assert_eq!(imported_count, 0); // No new contacts should be imported

        // Check that we still have the same number of contacts
        let contacts = contact_manager.list_contacts(username2, password).unwrap();
        assert_eq!(contacts.len(), 2);
    }

    #[test]
    fn test_contact_manager_grouping() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Add some contacts
        contact_manager
            .add_contact(username, password, "public_key_1", Some("Alice"))
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_2", Some("Bob"))
            .unwrap();
        contact_manager
            .add_contact(username, password, "public_key_3", Some("Charlie"))
            .unwrap();

        // Add contacts to groups
        contact_manager
            .add_contact_to_group(username, password, "public_key_1", "Friends")
            .unwrap();
        contact_manager
            .add_contact_to_group(username, password, "public_key_1", "Work")
            .unwrap();
        contact_manager
            .add_contact_to_group(username, password, "public_key_2", "Friends")
            .unwrap();
        contact_manager
            .add_contact_to_group(username, password, "public_key_3", "Family")
            .unwrap();

        // Check that contacts are in the correct groups
        let friends = contact_manager
            .get_contacts_in_group(username, password, "Friends")
            .unwrap();
        assert_eq!(friends.len(), 2);

        let work = contact_manager
            .get_contacts_in_group(username, password, "Work")
            .unwrap();
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].alias, Some("Alice".to_string()));

        let family = contact_manager
            .get_contacts_in_group(username, password, "Family")
            .unwrap();
        assert_eq!(family.len(), 1);
        assert_eq!(family[0].alias, Some("Charlie".to_string()));

        // Check that a contact is in the correct groups
        let alice_groups = contact_manager
            .get_contact_groups(username, password, "public_key_1")
            .unwrap();
        assert_eq!(alice_groups.len(), 2);
        assert!(alice_groups.contains(&"Friends".to_string()));
        assert!(alice_groups.contains(&"Work".to_string()));

        // Check all groups
        let all_groups = contact_manager.get_all_groups(username, password).unwrap();
        assert_eq!(all_groups.len(), 3);
        assert!(all_groups.contains(&"Friends".to_string()));
        assert!(all_groups.contains(&"Work".to_string()));
        assert!(all_groups.contains(&"Family".to_string()));

        // Remove a contact from a group
        contact_manager
            .remove_contact_from_group(username, password, "public_key_1", "Work")
            .unwrap();

        // Check that the contact is no longer in that group
        let work = contact_manager
            .get_contacts_in_group(username, password, "Work")
            .unwrap();
        assert_eq!(work.len(), 0);

        // Check that the contact is still in the other group
        let friends = contact_manager
            .get_contacts_in_group(username, password, "Friends")
            .unwrap();
        assert_eq!(friends.len(), 2);

        // Check that the contact's groups are updated
        let alice_groups = contact_manager
            .get_contact_groups(username, password, "public_key_1")
            .unwrap();
        assert_eq!(alice_groups.len(), 1);
        assert!(alice_groups.contains(&"Friends".to_string()));
    }

    #[test]
    fn test_contact_discovery() {
        let temp_dir = TempDir::new().unwrap();
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());
        let contact_discovery = ContactDiscovery::new(Arc::new(contact_manager));

        let username = "testuser";
        let password = "testpassword";

        // Register a user first
        let _user = identity_manager.register_user(username, password).unwrap();

        // Manually add a contact
        contact_discovery
            .manually_add_contact(username, password, "peer_id_1", Some("Peer 1"))
            .unwrap();

        // Check that the contact was added
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());
        let contact = contact_manager
            .get_contact(username, password, "peer_id_1")
            .unwrap();
        assert_eq!(contact.public_key, "peer_id_1");
        assert_eq!(contact.alias, Some("Peer 1".to_string()));

        // Try to add the same contact again (should fail)
        let result = contact_discovery.manually_add_contact(
            username,
            password,
            "peer_id_1",
            Some("Peer 1 Again"),
        );
        assert!(result.is_err());

        // Discover contacts from a list
        let peer_ids = vec!["peer_id_2".to_string(), "peer_id_3".to_string()];
        let discovered = contact_discovery
            .discover_contacts_from_list(username, password, &peer_ids)
            .unwrap();
        assert_eq!(discovered.len(), 2);
        assert!(discovered.contains(&"peer_id_2".to_string()));
        assert!(discovered.contains(&"peer_id_3".to_string()));

        // Check that the contacts were added
        let contacts = contact_manager.list_contacts(username, password).unwrap();
        assert_eq!(contacts.len(), 3);

        // Try to discover the same contacts again (should not add duplicates)
        let peer_ids = vec!["peer_id_2".to_string(), "peer_id_4".to_string()];
        let discovered = contact_discovery
            .discover_contacts_from_list(username, password, &peer_ids)
            .unwrap();
        assert_eq!(discovered.len(), 1);
        assert_eq!(discovered[0], "peer_id_4");

        // Check that we now have 4 contacts
        let contacts = contact_manager.list_contacts(username, password).unwrap();
        assert_eq!(contacts.len(), 4);
    }
}

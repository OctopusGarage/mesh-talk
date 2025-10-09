#[cfg(test)]
mod tests {
    use super::super::contact::Contact;
    use super::super::manager::ContactManager;
    use crate::identity::manager::IdentityManager;
    use crate::storage::file_manager::FileManager;
    use tempfile::TempDir;

    fn setup() -> (TempDir, FileManager, IdentityManager, ContactManager) {
        let temp_dir = TempDir::new().expect("temp dir");
        let file_manager = FileManager::new(temp_dir.path().to_path_buf());
        let identity_manager = IdentityManager::new(file_manager.clone());
        let contact_manager = ContactManager::new(file_manager.clone(), identity_manager.clone());
        (temp_dir, file_manager, identity_manager, contact_manager)
    }

    #[test]
    fn contact_new_sets_fields() {
        let contact = Contact::new(
            "192.168.1.10".to_string(),
            9000,
            "alice".to_string(),
            Some("user-123".to_string()),
        );

        assert_eq!(contact.ip, "192.168.1.10");
        assert_eq!(contact.port, 9000);
        assert_eq!(contact.username, "alice");
        assert_eq!(contact.user_id.as_deref(), Some("user-123"));
        assert!(!contact.is_online);
        assert!(contact.groups.is_empty());
        assert!(contact.added_at > 0);
    }

    #[test]
    fn contact_validation_helpers() {
        assert!(Contact::validate_ip("10.0.0.1"));
        assert!(!Contact::validate_ip(""));
        assert!(Contact::validate_port(8080));
        assert!(!Contact::validate_port(0));
        assert!(Contact::validate_username("alice"));
        assert!(!Contact::validate_username(""));
        assert!(Contact::validate_user_id(&Some("user".into())));
        assert!(Contact::validate_user_id(&None));
        assert!(!Contact::validate_user_id(&Some(String::new())));
    }

    #[test]
    fn add_and_get_contact_round_trip() {
        let (_temp, _fm, identity_manager, contact_manager) = setup();
        let username = "owner";
        let password = "password123";
        identity_manager
            .register_user(username, password)
            .expect("register user");

        contact_manager
            .add_contact(
                username,
                password,
                "192.168.0.20",
                7100,
                "bob",
                Some("peer-1".into()),
            )
            .expect("add contact");

        // dedupe by user_id should update existing contact instead of adding duplicate
        contact_manager
            .add_contact(
                username,
                password,
                "192.168.0.21",
                7200,
                "bob-new",
                Some("peer-1".into()),
            )
            .expect("update existing contact");

        let contact = contact_manager
            .get_contact(username, password, "192.168.0.21:7200")
            .expect("get contact");
        assert_eq!(contact.username, "bob-new");
        assert_eq!(contact.user_id.as_deref(), Some("peer-1"));

        let contacts = contact_manager
            .list_contacts(username, password)
            .expect("list contacts");
        assert_eq!(contacts.len(), 1);
    }

    #[test]
    fn remove_contact_and_existence_check() {
        let (_temp, _fm, identity_manager, contact_manager) = setup();
        let username = "owner";
        let password = "password123";
        identity_manager
            .register_user(username, password)
            .expect("register user");

        contact_manager
            .add_contact(username, password, "127.0.0.1", 7001, "loop", None)
            .expect("add contact");

        assert!(contact_manager
            .contact_exists(username, password, "127.0.0.1:7001")
            .unwrap());

        contact_manager
            .remove_contact(username, password, "127.0.0.1:7001")
            .expect("remove contact");

        assert!(!contact_manager
            .contact_exists(username, password, "127.0.0.1:7001")
            .unwrap());
    }

    #[test]
    fn add_discovered_contact_uses_default_username() {
        let (_temp, _fm, identity_manager, contact_manager) = setup();
        let username = "owner";
        let password = "password123";
        identity_manager
            .register_user(username, password)
            .expect("register user");

        contact_manager
            .add_discovered_contact(username, password, "10.0.0.5:7331", Some("peer"))
            .expect("add discovered");

        let contact = contact_manager
            .get_contact(username, password, "10.0.0.5:7331")
            .expect("get contact");
        assert_eq!(contact.username, "peer");

        // second call should fail because contact already exists
        assert!(contact_manager
            .add_discovered_contact(username, password, "10.0.0.5:7331", Some("peer"))
            .is_err());
    }

    #[test]
    fn update_contact_username() {
        let (_temp, _fm, identity_manager, contact_manager) = setup();
        let username = "owner";
        let password = "password123";
        identity_manager
            .register_user(username, password)
            .expect("register user");

        contact_manager
            .add_contact(username, password, "172.16.0.9", 6050, "old", None)
            .expect("add contact");

        contact_manager
            .update_contact_username(username, password, "172.16.0.9:6050", Some("new-name"))
            .expect("update username");

        let contact = contact_manager
            .get_contact(username, password, "172.16.0.9:6050")
            .expect("get contact");
        assert_eq!(contact.username, "new-name");
    }
}

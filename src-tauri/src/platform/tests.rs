#[cfg(test)]
mod tests {
    use crate::platform::*;

    #[test]
    fn test_platform_module_compilation() {
        // This test ensures that the platform module compiles correctly
        // It doesn't test the actual functionality, just that the code is valid

        // Test that we can create instances of the platform-specific types
        #[cfg(target_os = "macos")]
        {
            let _storage = MacOSSecureStorage::new();
            let _notifications = MacOSNotificationProvider::new();
        }

        #[cfg(target_os = "windows")]
        {
            let _storage = WindowsSecureStorage::new();
            let _notifications = WindowsNotificationProvider::new();
        }

        #[cfg(target_os = "linux")]
        {
            let _storage = LinuxSecureStorage::new();
            let _notifications = LinuxNotificationProvider::new();
        }

        // Test that we can get platform-specific implementations
        let _secure_storage = get_secure_storage();
        let _notification_provider = get_notification_provider();
    }
}

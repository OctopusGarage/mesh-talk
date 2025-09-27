//! Common service traits and utilities for Mesh-Talk
//!
//! This module provides shared traits and utilities for service implementations
//! to promote consistency and code reuse.

use std::sync::{Arc, OnceLock};

/// Common trait for all service implementations
///
/// This trait defines common patterns and utilities that should be implemented
/// by all service types in the application.
pub trait Service: Clone + Send + Sync {
    /// Service-specific error type
    type Error;
    
    /// Service-specific result type
    type Result<T>;
    
    /// Initializes the service with required dependencies
    fn init(dependencies: ServiceDependencies) -> Self;
    
    /// Gets the name of the service for logging and debugging purposes
    fn service_name(&self) -> &'static str;
    
    /// Performs health check on the service
    fn health_check(&self) -> ServiceHealth;
    
    /// Shuts down the service gracefully
    fn shutdown(&self) -> Self::Result<()>;
}

/// Dependencies required by services
///
/// This struct contains common dependencies that services might need.
pub struct ServiceDependencies {
    /// File manager for persistent storage
    pub file_manager: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Configuration settings
    pub config: Option<std::collections::HashMap<String, String>>,
    /// Other service references for cross-service communication
    pub service_refs: std::collections::HashMap<String, Arc<dyn std::any::Any + Send + Sync>>,
}

/// Health status of a service
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceHealth {
    /// Service is healthy and operational
    Healthy,
    /// Service is degraded but still functional
    Degraded(String),
    /// Service is unhealthy and not functional
    Unhealthy(String),
    /// Service is initializing
    Initializing,
}

/// Generic service manager for singleton service instances
///
/// This provides a common pattern for managing singleton service instances
/// with lazy initialization.
pub struct ServiceManager<T> {
    instance: OnceLock<T>,
}

impl<T> ServiceManager<T> {
    /// Creates a new service manager
    pub fn new() -> Self {
        Self {
            instance: OnceLock::new(),
        }
    }
    
    /// Gets or initializes the service instance
    pub fn get_or_init<F>(&self, initializer: F) -> &T
    where
        F: FnOnce() -> T,
    {
        self.instance.get_or_init(initializer)
    }
    
    /// Sets the service instance if not already set
    pub fn set(&self, value: T) -> Result<(), T> {
        self.instance.set(value)
    }
    
    /// Gets a reference to the service instance if it exists
    pub fn get(&self) -> Option<&T> {
        self.instance.get()
    }
}

impl<T> Default for ServiceManager<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Extension trait for Result types to provide common service operations
pub trait ServiceResultExt<T, E> {
    /// Maps the result to include service context information
    fn with_service_context(self, service_name: &str, operation: &str) -> Result<T, E>;
    
    /// Converts to a standardized service result format
    fn to_service_result<F>(self, converter: F) -> ServiceResult<T>
    where
        F: FnOnce(E) -> ServiceError;
}

/// Standardized service result type
pub type ServiceResult<T> = Result<T, ServiceError>;

/// Standardized service error type
#[derive(Debug)]
pub struct ServiceError {
    /// Service that originated the error
    pub service: String,
    /// Operation that failed
    pub operation: String,
    /// Error message
    pub message: String,
    /// Underlying error if available
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Service '{}' operation '{}' failed: {}",
            self.service, self.operation, self.message
        )
    }
}

impl std::error::Error for ServiceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|e| e.as_ref() as &(dyn std::error::Error + 'static))
    }
}

impl<T, E> ServiceResultExt<T, E> for Result<T, E> {
    fn with_service_context(self, _service_name: &str, _operation: &str) -> Result<T, E> {
        // This is a placeholder implementation - in practice, this would
        // need to be implemented specifically for each error type
        self
    }
    
    fn to_service_result<F>(self, converter: F) -> ServiceResult<T>
    where
        F: FnOnce(E) -> ServiceError,
    {
        self.map_err(converter)
    }
}

/// Common service initialization utilities
pub mod init {
    use super::*;
    
    
    /// Initializes a global service instance
    ///
    /// This provides a standardized way to initialize global service instances
    /// with proper error handling and logging.
    pub fn init_global_service<T, F, E>(
        service_manager: &'static ServiceManager<T>,
        initializer: F,
    ) -> Result<(), E>
    where
        T: Clone,
        F: FnOnce() -> Result<T, E>,
    {
        match initializer() {
            Ok(service) => {
                service_manager.set(service).map_err(|_| {
                    // This is a bit tricky since we've consumed the error,
                    // but we can reconstruct a meaningful error
                    panic!("Failed to set service instance - this should not happen")
                }).unwrap_or(());
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
    
    /// Gets a global service instance, panicking if not initialized
    ///
    /// This provides a standardized way to access global service instances
    /// with clear error messages if they haven't been initialized.
    pub fn get_global_service<T>(
        service_manager: &'static ServiceManager<T>,
        service_name: &str,
    ) -> &'static T {
        service_manager.get().unwrap_or_else(|| {
            panic!(
                "Global {} service not initialized - call init_global() first",
                service_name
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    
    // Mock service for testing
    #[derive(Clone)]
    struct MockService {
        name: String,
    }
    
    impl Service for MockService {
        type Error = String;
        type Result<T> = Result<T, String>;
        
        fn init(_deps: ServiceDependencies) -> Self {
            Self {
                name: "mock".to_string(),
            }
        }
        
        fn service_name(&self) -> &'static str {
            "MockService"
        }
        
        fn health_check(&self) -> ServiceHealth {
            ServiceHealth::Healthy
        }
        
        fn shutdown(&self) -> Self::Result<()> {
            Ok(())
        }
    }
    
    #[test]
    fn test_service_manager() {
        let manager: ServiceManager<MockService> = ServiceManager::new();
        
        // Should be able to set once
        let service = MockService {
            name: "test".to_string(),
        };
        assert!(manager.set(service).is_ok());
        
        // Should be able to get
        let retrieved = manager.get();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().name, "test");
    }
    
    #[test]
    fn test_service_manager_get_or_init() {
        let manager: ServiceManager<String> = ServiceManager::new();
        
        // Should initialize and return the value
        let value = manager.get_or_init(|| "initialized".to_string());
        assert_eq!(value, "initialized");
        
        // Should return the same value on subsequent calls
        let value2 = manager.get_or_init(|| "different".to_string());
        assert_eq!(value2, "initialized");
    }
    
    #[test]
    fn test_service_health_enum() {
        let healthy = ServiceHealth::Healthy;
        let degraded = ServiceHealth::Degraded("Slow response times".to_string());
        let unhealthy = ServiceHealth::Unhealthy("Database connection failed".to_string());
        
        assert_eq!(healthy, ServiceHealth::Healthy);
        assert_ne!(degraded, healthy);
        assert_ne!(unhealthy, healthy);
    }
}
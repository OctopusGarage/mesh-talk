//! Common networking utilities for Mesh-Talk
//!
//! This module provides shared utilities for networking operations including
//! retry logic, timeouts, and connection management.

use crate::error::{MeshTalkError, MeshTalkResult, NetworkErrorKind};
use std::future::Future;
use std::time::Duration;
use tokio::time::timeout as tokio_timeout;

/// Configuration for retry operations
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_retries: usize,
    /// Base delay between retries in milliseconds
    pub base_delay_ms: u64,
    /// Maximum delay between retries in milliseconds
    pub max_delay_ms: u64,
    /// Factor for exponential backoff (e.g., 2.0 for doubling each time)
    pub backoff_factor: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 5,
            base_delay_ms: 100,
            max_delay_ms: 5000,
            backoff_factor: 2.0,
        }
    }
}

/// Configuration for timeout operations
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Timeout duration for operations
    pub timeout_duration: Duration,
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            timeout_duration: Duration::from_secs(10),
        }
    }
}

/// Executes an operation with retry logic and exponential backoff
pub async fn retry_with_backoff<F, Fut, T, E, EF>(
    operation: F,
    retry_config: &RetryConfig,
    error_filter: EF,
) -> Result<T, E>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    EF: Fn(&E) -> bool,
{
    let mut retries = 0;

    loop {
        match operation().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                // Check if we should retry based on the error
                if !error_filter(&e) || retries >= retry_config.max_retries {
                    return Err(e);
                }

                retries += 1;

                // Calculate exponential backoff delay
                let delay_ms = (retry_config.base_delay_ms as f64
                    * retry_config.backoff_factor.powi(retries as i32))
                    as u64;
                let capped_delay_ms = delay_ms.min(retry_config.max_delay_ms);

                tokio::time::sleep(Duration::from_millis(capped_delay_ms)).await;
            }
        }
    }
}

/// Executes an operation with a timeout
pub async fn with_timeout<F, Fut, T>(
    operation: F,
    timeout_config: &TimeoutConfig,
) -> MeshTalkResult<T>
where
    F: Future<Output = Result<T, std::io::Error>>,
{
    tokio_timeout(timeout_config.timeout_duration, operation)
        .await
        .map_err(|_| {
            MeshTalkError::network(NetworkErrorKind::ConnectionTimeout, "Operation timed out")
        })?
        .map_err(|e| {
            MeshTalkError::network_with_source(
                NetworkErrorKind::ConnectionFailed,
                "Operation failed",
                Box::new(e),
            )
        })
}

/// Standard retry configuration for network operations
pub fn standard_network_retry_config() -> RetryConfig {
    RetryConfig {
        max_retries: 3,
        base_delay_ms: 200,
        max_delay_ms: 3000,
        backoff_factor: 2.0,
    }
}

/// Standard timeout configuration for network operations
pub fn standard_network_timeout_config() -> TimeoutConfig {
    TimeoutConfig {
        timeout_duration: Duration::from_secs(15),
    }
}

/// Quick retry configuration for fast-fail operations
pub fn quick_retry_config() -> RetryConfig {
    RetryConfig {
        max_retries: 2,
        base_delay_ms: 50,
        max_delay_ms: 500,
        backoff_factor: 1.5,
    }
}

/// Aggressive retry configuration for critical operations
pub fn aggressive_retry_config() -> RetryConfig {
    RetryConfig {
        max_retries: 7,
        base_delay_ms: 50,
        max_delay_ms: 2000,
        backoff_factor: 1.8,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_with_backoff_success() {
        let attempt_counter = Arc::new(AtomicU32::new(0));
        let counter = attempt_counter.clone();

        let result = retry_with_backoff(
            || async {
                let current_attempt = counter.fetch_add(1, Ordering::SeqCst);
                if current_attempt < 2 {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Temporary error",
                    ))
                } else {
                    Ok("Success")
                }
            },
            &standard_network_retry_config(),
            |e: &std::io::Error| e.kind() == std::io::ErrorKind::Other,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Success");
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_with_backoff_max_retries() {
        let attempt_counter = Arc::new(AtomicU32::new(0));
        let counter = attempt_counter.clone();

        let result: Result<String, std::io::Error> = retry_with_backoff(
            || async {
                counter.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Persistent error",
                ))
            },
            &RetryConfig {
                max_retries: 2,
                ..Default::default()
            },
            |_| true,
        )
        .await;

        assert!(result.is_err());
        // Should have tried 3 times (initial + 2 retries)
        assert_eq!(attempt_counter.load(Ordering::SeqCst), 3);
    }
}

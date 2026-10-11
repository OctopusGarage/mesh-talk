//! Performance monitoring utilities for Mesh-Talk application
//!
//! This module provides lightweight performance monitoring capabilities
//! that can be used to track function execution times and resource usage.

use std::time::Instant;
use tracing::{debug, info};

/// A simple timer for measuring function execution time
pub struct Timer {
    start: Instant,
    name: String,
}

impl Timer {
    /// Create a new timer with the given name
    pub fn new(name: &str) -> Self {
        Self {
            start: Instant::now(),
            name: name.to_string(),
        }
    }

    /// Log the elapsed time and consume the timer
    pub fn log_elapsed(self) {
        let elapsed = self.start.elapsed();
        info!("Performance: {} took {:?}", self.name, elapsed);
    }

    /// Log the elapsed time with debug level and consume the timer
    pub fn debug_elapsed(self) {
        let elapsed = self.start.elapsed();
        debug!("Performance: {} took {:?}", self.name, elapsed);
    }
}

/// A scoped timer that automatically logs elapsed time when dropped
pub struct ScopedTimer {
    timer: Timer,
}

impl ScopedTimer {
    /// Create a new scoped timer with the given name
    pub fn new(name: &str) -> Self {
        Self {
            timer: Timer::new(name),
        }
    }
}

impl Drop for ScopedTimer {
    fn drop(&mut self) {
        // We can't move the timer out, so we duplicate the logic here
        let elapsed = self.timer.start.elapsed();
        info!("Performance: {} took {:?}", self.timer.name, elapsed);
    }
}

/// Macro for easily adding performance monitoring to functions
#[macro_export]
macro_rules! perf_monitor {
    ($name:expr) => {
        $crate::perf::ScopedTimer::new($name)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_timer() {
        let timer = Timer::new("test_operation");
        std::thread::sleep(std::time::Duration::from_millis(10));
        timer.log_elapsed();
    }

    #[test]
    fn test_scoped_timer() {
        let _timer = ScopedTimer::new("scoped_test_operation");
        std::thread::sleep(std::time::Duration::from_millis(10));
        // Timer will automatically log when dropped
    }
}

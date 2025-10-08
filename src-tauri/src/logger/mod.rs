//! Logger module for Mesh-Talk application
//!
//! This module provides unified logging functionality for both frontend and backend,
//! with support for file rotation, console output, and retention management.

use log::LevelFilter as LogLevelFilter;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tauri_plugin_log::{Target, TargetKind, TimezoneStrategy};

/// Maximum number of days to retain log files
const MAX_LOG_RETENTION_DAYS: u64 = 7;

/// Get the logs directory path (~/.mesh-talk/logs/)
fn get_mesh_talk_logs_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home_dir = env::var("HOME").map_err(|_| "Failed to get home directory")?;
    let logs_dir = PathBuf::from(home_dir).join(".mesh-talk").join("logs");
    fs::create_dir_all(&logs_dir)?;
    Ok(logs_dir)
}

/// Initialize the logging system
pub fn init_logging(app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // Get the custom logs directory (~/.mesh-talk/logs/)
    let logs_dir = get_mesh_talk_logs_dir()?;

    // Clean up old log files
    cleanup_old_logs(&logs_dir, MAX_LOG_RETENTION_DAYS)?;

    // Initialize tauri-plugin-log with file and console targets
    let log_plugin = tauri_plugin_log::Builder::new()
        .targets([
            Target::new(TargetKind::Stdout),
            Target::new(TargetKind::Folder {
                path: logs_dir.clone(),
                file_name: Some("mesh-talk".into()),
            }),
        ])
        .level(LogLevelFilter::Debug)
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .build();

    app_handle.plugin(log_plugin)?;

    println!("Logging initialized successfully");
    println!("Logs directory: {:?}", logs_dir);

    Ok(())
}

/// Clean up log files older than the specified number of days
fn cleanup_old_logs(logs_dir: &Path, max_days: u64) -> Result<(), std::io::Error> {
    let cutoff_time = SystemTime::now() - Duration::from_secs(max_days * 24 * 60 * 60);

    if let Ok(entries) = fs::read_dir(logs_dir) {
        for entry in entries {
            if let Ok(entry) = entry {
                let path = entry.path();

                // Only process files with .log extension
                if path.extension().map_or(false, |ext| ext == "log") {
                    if let Ok(metadata) = fs::metadata(&path) {
                        if let Ok(modified) = metadata.modified() {
                            if modified < cutoff_time {
                                // Delete old log file
                                if let Err(e) = fs::remove_file(&path) {
                                    println!("Failed to delete old log file {:?}: {}", path, e);
                                } else {
                                    println!("Deleted old log file: {:?}", path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Get the path to the logs directory
pub fn get_logs_directory(
    _app_handle: &tauri::AppHandle,
) -> Result<std::path::PathBuf, tauri::Error> {
    get_mesh_talk_logs_dir().map_err(|e| {
        tauri::Error::from(std::io::Error::new(
            std::io::ErrorKind::Other,
            e.to_string(),
        ))
    })
}

/// Get the current log file path
pub fn get_current_log_file(
    _app_handle: &tauri::AppHandle,
) -> Result<std::path::PathBuf, tauri::Error> {
    let logs_dir = get_logs_directory(_app_handle)?;
    Ok(logs_dir.join("mesh-talk.log"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_cleanup_old_logs() {
        let temp_dir = TempDir::new().unwrap();
        let logs_dir = temp_dir.path().join("logs");
        fs::create_dir_all(&logs_dir).unwrap();

        // Create a log file that should be kept (recent)
        let new_file = logs_dir.join("new.log");
        let mut file = File::create(&new_file).unwrap();
        writeln!(file, "new log content").unwrap();

        // Run cleanup
        cleanup_old_logs(&logs_dir, 7).unwrap();

        // Check that the new file still exists
        assert!(new_file.exists());
    }

    #[test]
    fn test_get_mesh_talk_logs_dir() {
        // Test that our custom log directory function works
        let result = get_mesh_talk_logs_dir();
        assert!(result.is_ok());
        let logs_dir = result.unwrap();
        // Check that it contains .mesh-talk/logs
        assert!(logs_dir.display().to_string().contains(".mesh-talk/logs"));
    }
}

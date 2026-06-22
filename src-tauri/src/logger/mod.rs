//! Logger module for Mesh-Talk application
//!
//! This module provides unified logging functionality for both frontend and backend,
//! with support for file rotation, console output, and retention management.

use log::LevelFilter as LogLevelFilter;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tauri_plugin_log::{Target, TargetKind, TimezoneStrategy};

/// Maximum number of days to retain log files
const MAX_LOG_RETENTION_DAYS: u64 = 7;

/// Get the logs directory path (~/.mesh-talk/logs/)
fn get_mesh_talk_logs_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let logs_dir = crate::data_dir().join("logs");
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
                if path.extension().is_some_and(|ext| ext == "log") {
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
    get_mesh_talk_logs_dir().map_err(|e| tauri::Error::from(std::io::Error::other(e.to_string())))
}

/// Get the current log file path
pub fn get_current_log_file(
    _app_handle: &tauri::AppHandle,
) -> Result<std::path::PathBuf, tauri::Error> {
    let logs_dir = get_logs_directory(_app_handle)?;
    Ok(logs_dir.join("mesh-talk.log"))
}

/// How much of the current log we surface to the UI (the most recent bytes). Bounds the
/// IPC payload so a long-running session's multi-MB log can't blow up the bug-report copy.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// The logs directory, as a string (for the Diagnostics "Reveal logs folder" button).
#[tauri::command]
pub fn get_logs_dir(app_handle: tauri::AppHandle) -> Result<String, crate::commands::CommandError> {
    let dir = get_logs_directory(&app_handle).map_err(|e| e.to_string())?;
    Ok(dir.to_string_lossy().into_owned())
}

/// The current log file path, as a string.
#[tauri::command]
pub fn get_log_file(app_handle: tauri::AppHandle) -> Result<String, crate::commands::CommandError> {
    let file = get_current_log_file(&app_handle).map_err(|e| e.to_string())?;
    Ok(file.to_string_lossy().into_owned())
}

/// Read the tail of the current log (last [`LOG_TAIL_BYTES`]), for the Diagnostics
/// "copy log tail / save for bug report" affordance. Returns an empty string if the log
/// file doesn't exist yet. Seeks to the tail so a large file isn't read whole.
#[tauri::command]
pub fn read_log_tail(
    app_handle: tauri::AppHandle,
) -> Result<String, crate::commands::CommandError> {
    use std::io::{Read, Seek, SeekFrom};
    let path = get_current_log_file(&app_handle).map_err(|e| e.to_string())?;
    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(e) => return Err(e.to_string().into()),
    };
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    // The log is UTF-8; a tail-seek can land mid-codepoint, so decode lossily.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Write the current log tail to `dest` (a user-chosen path), for the Diagnostics
/// "save for bug report" affordance. Keeps the write on the Rust side since the frontend
/// has no fs plugin.
#[tauri::command]
pub fn save_log_tail(
    app_handle: tauri::AppHandle,
    dest: String,
) -> Result<(), crate::commands::CommandError> {
    let tail = read_log_tail(app_handle)?;
    fs::write(&dest, tail).map_err(|e| e.to_string())?;
    Ok(())
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
        // Separator-agnostic (Windows uses '\'): the path ends with `.mesh-talk/logs`.
        assert!(logs_dir.ends_with("logs"));
        assert!(logs_dir.parent().unwrap().ends_with(".mesh-talk"));
    }
}

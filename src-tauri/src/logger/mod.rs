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
pub(crate) const GRAPHICS_LOG_TARGET: &str = "mesh_talk::graphics_configuration";

// Opt-in metadata only. No background task or timer is created by this observer.
const IPC_WORK_TARGET: &str = "mesh_talk::ipc_work";
const IPC_WORK_NAMES: usize = 256;
const IPC_WORK_WINDOW: Duration = Duration::from_secs(5);
static IPC_WORK_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
static IPC_WORK: std::sync::OnceLock<IpcWorkObserver> = std::sync::OnceLock::new();

fn ipc_work_enabled() -> bool {
    *IPC_WORK_ENABLED.get_or_init(|| {
        std::env::var_os("MESH_TALK_TRACE_WORK").as_deref() == Some(std::ffi::OsStr::new("1"))
    })
}

fn ipc_command_name(enabled: bool, command: &str) -> Option<String> {
    (enabled
        && !command.is_empty()
        && command.len() <= 96
        && command
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'))
    .then(|| command.to_owned())
}

/// Capture only a bounded name before Invoke is consumed. Disabled mode allocates nothing.
pub(crate) fn capture_ipc_work_name(command: &str) -> Option<String> {
    ipc_command_name(ipc_work_enabled(), command)
}

struct IpcWorkState {
    started: std::time::Instant,
    commands: std::collections::BTreeMap<String, u64>,
}
#[derive(serde::Serialize)]
struct IpcWorkSnapshot {
    window_ms: u128,
    commands: std::collections::BTreeMap<String, u64>,
    dropped: u64,
}
struct IpcWorkObserver {
    state: std::sync::Mutex<IpcWorkState>,
    dropped: std::sync::atomic::AtomicU64,
}
impl IpcWorkObserver {
    fn new(started: std::time::Instant) -> Self {
        Self {
            state: std::sync::Mutex::new(IpcWorkState {
                started,
                commands: Default::default(),
            }),
            dropped: Default::default(),
        }
    }

    fn record(
        &self,
        handled: bool,
        command: &str,
        now: std::time::Instant,
        emit: impl FnOnce(IpcWorkSnapshot),
    ) {
        if !handled {
            return;
        }
        let Ok(mut state) = self.state.try_lock() else {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        };
        if let Some(count) = state.commands.get_mut(command) {
            *count = count.saturating_add(1);
        } else if state.commands.len() < IPC_WORK_NAMES {
            state.commands.insert(command.to_owned(), 1);
        } else {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        let elapsed = now.saturating_duration_since(state.started);
        if elapsed < IPC_WORK_WINDOW {
            return;
        }
        let snapshot = IpcWorkSnapshot {
            window_ms: elapsed.as_millis(),
            commands: std::mem::take(&mut state.commands),
            dropped: self.dropped.swap(0, std::sync::atomic::Ordering::Relaxed),
        };
        state.started = now;
        drop(state);
        emit(snapshot);
    }
}

/// Counts registered dispatch attempts, not completion, success or side effects.
/// Caller must supply the generated handler's result, never a generic rejection result.
pub(crate) fn record_ipc_work(name: Option<String>, handled: bool) {
    let Some(name) = name else {
        return;
    };
    if !handled {
        return;
    }
    let now = std::time::Instant::now();
    IPC_WORK
        .get_or_init(|| IpcWorkObserver::new(now))
        .record(handled, &name, now, |snapshot| {
            if let Ok(json) = serde_json::to_string(&snapshot) {
                log::info!(target: IPC_WORK_TARGET, "ipc.work {json}");
            }
        });
}

fn configured_log_level(debug_build: bool) -> LogLevelFilter {
    if debug_build {
        LogLevelFilter::Debug
    } else {
        LogLevelFilter::Warn
    }
}

/// Get the logs directory path (~/.mesh-talk/logs/)
fn get_mesh_talk_logs_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let logs_dir = crate::data_dir().join("logs");
    fs::create_dir_all(&logs_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&logs_dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(logs_dir)
}

/// Initialize the logging system
pub fn init_logging(app_handle: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    // Get the custom logs directory (~/.mesh-talk/logs/)
    let logs_dir = get_mesh_talk_logs_dir()?;

    // Clean up old log files
    cleanup_old_logs(&logs_dir, MAX_LOG_RETENTION_DAYS)?;

    // Initialize tauri-plugin-log with file and console targets
    let mut targets = vec![Target::new(TargetKind::Folder {
        path: logs_dir.clone(),
        file_name: Some("mesh-talk".into()),
    })];
    if cfg!(debug_assertions) {
        targets.push(Target::new(TargetKind::Stdout));
    }
    let log_plugin = tauri_plugin_log::Builder::new()
        .targets(targets)
        .level(configured_log_level(cfg!(debug_assertions)))
        .level_for(GRAPHICS_LOG_TARGET, LogLevelFilter::Info)
        .level_for(
            IPC_WORK_TARGET,
            if ipc_work_enabled() {
                LogLevelFilter::Info
            } else {
                LogLevelFilter::Warn
            },
        )
        .level_for("mesh_talk_core::node::node", LogLevelFilter::Info)
        .timezone_strategy(TimezoneStrategy::UseLocal)
        .build();

    app_handle.plugin(log_plugin)?;

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
                if path.extension().is_some_and(|ext| ext == "log")
                    && let Ok(metadata) = fs::metadata(&path)
                    && let Ok(modified) = metadata.modified()
                    && modified < cutoff_time
                {
                    // Delete old log file
                    let _ = fs::remove_file(&path);
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
pub async fn read_log_tail(
    app_handle: tauri::AppHandle,
) -> Result<String, crate::commands::CommandError> {
    tokio::task::spawn_blocking(move || read_log_tail_blocking(&app_handle))
        .await
        .map_err(|e| crate::commands::CommandError::Internal(format!("log worker failed: {e}")))?
}

fn read_log_tail_blocking(
    app_handle: &tauri::AppHandle,
) -> Result<String, crate::commands::CommandError> {
    use std::io::{Read, Seek, SeekFrom};
    let path = get_current_log_file(app_handle).map_err(|e| e.to_string())?;
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
    file.take(LOG_TAIL_BYTES)
        .read_to_end(&mut buf)
        .map_err(|e| e.to_string())?;
    // The log is UTF-8; a tail-seek can land mid-codepoint, so decode lossily.
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Write the current log tail to `dest` (a user-chosen path), for the Diagnostics
/// "save for bug report" affordance. Keeps the write on the Rust side since the frontend
/// has no fs plugin.
#[tauri::command]
pub async fn save_log_tail(
    app_handle: tauri::AppHandle,
    dest: String,
) -> Result<(), crate::commands::CommandError> {
    tokio::task::spawn_blocking(move || {
        let tail = read_log_tail_blocking(&app_handle)?;
        fs::write(&dest, tail).map_err(|e| e.to_string())?;
        Ok(())
    })
    .await
    .map_err(|e| crate::commands::CommandError::Internal(format!("log worker failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn ipc_work_observer_bounds_names_and_disabled_capture() {
        assert!(ipc_command_name(false, "get_avatars").is_none());
        assert!(ipc_command_name(true, "payload:secret").is_none());
        assert!(ipc_command_name(true, &"a".repeat(97)).is_none());
        assert_eq!(
            ipc_command_name(true, "get_avatars").as_deref(),
            Some("get_avatars")
        );
    }

    #[test]
    fn ipc_work_observer_counts_only_handled_and_emits_after_unlock() {
        let start = std::time::Instant::now();
        let observer = IpcWorkObserver::new(start);
        let mut snapshots = Vec::new();
        observer.record(false, "unknown", start, |_| panic!("unhandled"));
        observer.record(true, "get_avatars", start, |_| panic!("early"));
        observer.record(true, "get_avatars", start + IPC_WORK_WINDOW, |snapshot| {
            assert!(
                observer.state.try_lock().is_ok(),
                "emit must not own observer lock"
            );
            observer.record(true, "reentrant", start + IPC_WORK_WINDOW, |_| {
                panic!("early reentry")
            });
            snapshots.push(snapshot);
        });
        observer.record(
            true,
            "get_avatars",
            start + IPC_WORK_WINDOW * 2,
            |snapshot| {
                assert_eq!(snapshot.commands.get("reentrant"), Some(&1));
            },
        );
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].commands.get("get_avatars"), Some(&2));
        assert!(!snapshots[0].commands.contains_key("unknown"));
        assert_eq!(snapshots[0].dropped, 0);
    }

    #[test]
    fn ipc_work_observer_reports_capacity_and_contention_without_waiting() {
        let start = std::time::Instant::now();
        let observer = IpcWorkObserver::new(start);
        for index in 0..IPC_WORK_NAMES {
            observer.record(true, &format!("command_{index}"), start, |_| {
                panic!("early")
            });
        }
        observer.record(true, "overflow", start, |_| panic!("early"));
        let held = observer.state.lock().unwrap();
        observer.record(true, "get_avatars", start, |_| panic!("held"));
        drop(held);
        observer.record(true, "command_0", start + IPC_WORK_WINDOW, |snapshot| {
            assert_eq!(snapshot.commands.len(), IPC_WORK_NAMES);
            assert_eq!(snapshot.commands.get("command_0"), Some(&2));
            assert_eq!(snapshot.dropped, 2);
        });
    }

    #[test]
    fn release_logging_keeps_only_warnings_and_errors() {
        assert_eq!(configured_log_level(false), LogLevelFilter::Warn);
        assert_eq!(configured_log_level(true), LogLevelFilter::Debug);
    }

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

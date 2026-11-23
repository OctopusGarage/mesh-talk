//! Static environment / "About" facts for the Diagnostics dialog: app version, the data
//! and logs directories, OS/arch, and build info. Pure presentation — no node needed, so
//! these work even before (or without) login.

use crate::commands::CommandError;
use serde::Serialize;

mod process_resources;

static DESKTOP_STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static INITIAL_INTERFACE_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

pub(crate) fn record_desktop_start(started: std::time::Instant) {
    let _ = DESKTOP_STARTED.set(started);
}

pub(crate) fn record_initial_interface() {
    if let Some(started) = DESKTOP_STARTED.get() {
        INITIAL_INTERFACE_MS.get_or_init(|| elapsed_ms(started.elapsed()));
    }
}

fn elapsed_ms(elapsed: std::time::Duration) -> u64 {
    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
}

static DESKTOP_READY_MS: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

pub(crate) fn record_desktop_ready(elapsed: std::time::Duration) {
    let millis = elapsed_ms(elapsed);
    let _ = DESKTOP_READY_MS.set(millis);
    log::info!(target: crate::logger::STARTUP_LOG_TARGET, "desktop_host_ready_ms={millis}");
}

#[derive(Serialize)]
pub struct HostResourceSample {
    #[serde(flatten)]
    resources: process_resources::ProcessResources,
    sampled_at_ms: f64,
    authenticated_channels: u64,
    sent_frame_bytes: u64,
    received_frame_bytes: u64,
    messages: mesh_talk_core::transport::diagnostics::MessageCounters,
    direct_backlog: Option<(usize, Option<u64>)>,
}

#[tauri::command]
pub fn start_host_resource_sampling() -> u64 {
    mesh_talk_core::transport::diagnostics::begin()
}

#[tauri::command]
pub fn stop_host_resource_sampling(ticket: u64) {
    mesh_talk_core::transport::diagnostics::stop(ticket);
}

#[tauri::command]
pub async fn host_resource_sample(
    state: tauri::State<'_, crate::chat_commands::NodeState>,
    ticket: u64,
    include_backlog: Option<bool>,
) -> Result<Option<HostResourceSample>, CommandError> {
    let node = include_backlog
        .unwrap_or(false)
        .then(|| {
            state
                .0
                .try_read()
                .ok()
                .and_then(|slot| slot.as_ref().map(|runtime| runtime.handle()))
        })
        .flatten();
    tauri::async_runtime::spawn_blocking(move || {
        use std::{sync::OnceLock, time::Instant};
        static SAMPLE_ORIGIN: OnceLock<Instant> = OnceLock::new();
        let transport = mesh_talk_core::transport::diagnostics::sample(ticket)?;
        // Capture the interval in the sampling worker after blocking-pool admission.
        let sampled_at_ms = SAMPLE_ORIGIN
            .get_or_init(Instant::now)
            .elapsed()
            .as_secs_f64()
            * 1000.0;
        process_resources::read_resources().map(|resources| HostResourceSample {
            resources,
            sampled_at_ms,
            authenticated_channels: transport.authenticated_channels,
            sent_frame_bytes: transport.sent_frame_bytes,
            received_frame_bytes: transport.received_frame_bytes,
            messages: transport.messages,
            direct_backlog: node.and_then(|node| node.diagnostic_delivery_backlog()),
        })
    })
    .await
    .map_err(|error| CommandError::Io(error.to_string()))
}

#[cfg(any(target_os = "linux", test))]
fn graphics_env_value(value: Option<&std::ffi::OsStr>) -> &'static str {
    let Some(value) = value else { return "unset" };
    match value.to_str() {
        Some("x11") => "x11",
        Some("wayland") => "wayland",
        Some("tty") => "tty",
        Some("x11,wayland") => "x11,wayland",
        Some("wayland,x11") => "wayland,x11",
        Some("0") => "0",
        Some("1") => "1",
        _ => "custom",
    }
}

/// Configuration hints, not proof of the actual GTK backend or GPU renderer.
#[cfg(target_os = "linux")]
pub(crate) fn log_linux_graphics_configuration() {
    for key in [
        "XDG_SESSION_TYPE",
        "GDK_BACKEND",
        "WEBKIT_DISABLE_DMABUF_RENDERER",
        "WEBKIT_DISABLE_COMPOSITING_MODE",
    ] {
        let value = std::env::var_os(key);
        log::info!(
            target: crate::logger::GRAPHICS_LOG_TARGET,
            "Linux graphics configuration: pid={} {key}={}",
            std::process::id(),
            graphics_env_value(value.as_deref())
        );
    }
}

/// Copyable rows for the Diagnostics "Environment" section. All fields are plain strings
/// so the frontend can render each through the existing `CopyValue`.
#[derive(Serialize)]
pub struct EnvInfo {
    pub app_version: String,
    pub data_dir: String,
    pub logs_dir: String,
    pub os: String,
    pub arch: String,
    /// Rust target triple this binary was built for.
    pub target: String,
    /// Debug vs release build.
    pub build_profile: String,
    pub desktop_ready_ms: Option<u64>,
    pub initial_interface_ms: Option<u64>,
}

/// Snapshot the logical build identity and environment facts. `TARGET` is resolved at
/// compile time; the OS/arch are the runtime constants.
#[tauri::command]
pub fn env_info(app_handle: tauri::AppHandle) -> Result<EnvInfo, CommandError> {
    let logs_dir = crate::logger::get_logs_directory(&app_handle)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(EnvInfo {
        app_version: crate::updates::current_build_version().to_string(),
        data_dir: crate::data_dir().to_string_lossy().into_owned(),
        logs_dir,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        // `TARGET` is exported to the build by tauri-build; fall back to a derived triple.
        target: option_env!("TARGET").unwrap_or("unknown").to_string(),
        desktop_ready_ms: DESKTOP_READY_MS.get().copied(),
        initial_interface_ms: INITIAL_INTERFACE_MS.get().copied(),
        build_profile: if cfg!(debug_assertions) {
            "debug".to_string()
        } else {
            "release".to_string()
        },
    })
}

#[cfg(test)]
mod tests {
    use super::graphics_env_value;
    use std::ffi::OsStr;

    #[test]
    fn graphics_diagnostics_distinguish_known_configuration_without_logging_raw_values() {
        assert_eq!(graphics_env_value(None), "unset");
        for known in [
            "x11",
            "wayland",
            "tty",
            "x11,wayland",
            "wayland,x11",
            "0",
            "1",
        ] {
            assert_eq!(graphics_env_value(Some(OsStr::new(known))), known);
        }
        for unknown in ["", "private-path", "x11\nforged log entry"] {
            assert_eq!(graphics_env_value(Some(OsStr::new(unknown))), "custom");
        }
    }
}

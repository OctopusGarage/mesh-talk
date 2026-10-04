//! Static environment / "About" facts for the Diagnostics dialog: app version, the data
//! and logs directories, OS/arch, and build info. Pure presentation — no node needed, so
//! these work even before (or without) login.

use crate::commands::CommandError;
use serde::Serialize;

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
}

/// Snapshot the static environment facts. `CARGO_PKG_VERSION` / `TARGET` are resolved at
/// compile time; the OS/arch are the runtime constants.
#[tauri::command]
pub fn env_info(app_handle: tauri::AppHandle) -> Result<EnvInfo, CommandError> {
    let logs_dir = crate::logger::get_logs_directory(&app_handle)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(EnvInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        data_dir: crate::data_dir().to_string_lossy().into_owned(),
        logs_dir,
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        // `TARGET` is exported to the build by tauri-build; fall back to a derived triple.
        target: option_env!("TARGET").unwrap_or("unknown").to_string(),
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

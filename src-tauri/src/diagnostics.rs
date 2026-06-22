//! Static environment / "About" facts for the Diagnostics dialog: app version, the data
//! and logs directories, OS/arch, and build info. Pure presentation — no node needed, so
//! these work even before (or without) login.

use crate::commands::CommandError;
use serde::Serialize;

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

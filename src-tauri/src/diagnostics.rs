//! Static environment / "About" facts for the Diagnostics dialog: app version, the data
//! and logs directories, OS/arch, and build info. Pure presentation — no node needed, so
//! these work even before (or without) login.

use crate::commands::CommandError;
use serde::Serialize;

/// Cumulative OS counters for the current host process. Never starts a collector.
#[derive(Serialize)]
pub struct HostResourceSample {
    cpu_ms: f64,
    resident_bytes: Option<u64>,
    disk_write_bytes: Option<u64>,
}

#[tauri::command]
pub async fn host_resource_sample() -> Result<Option<HostResourceSample>, CommandError> {
    tauri::async_runtime::spawn_blocking(read_host_resources)
        .await
        .map_err(|error| CommandError::Io(error.to_string()))
}

#[cfg(target_os = "macos")]
fn read_host_resources() -> Option<HostResourceSample> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
    // The flavor specifies the size and layout written to the output pointer.
    let result = unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            info.as_mut_ptr().cast(),
        )
    };
    if result != 0 {
        return None;
    }
    // The OS initialized the complete V2 record on success.
    let info = unsafe { info.assume_init() };
    Some(HostResourceSample {
        cpu_ms: (info.ri_user_time as f64 + info.ri_system_time as f64) / 1_000_000.0,
        resident_bytes: Some(info.ri_resident_size),
        disk_write_bytes: Some(info.ri_diskio_byteswritten),
    })
}

#[cfg(target_os = "linux")]
fn read_host_resources() -> Option<HostResourceSample> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // RUSAGE_SELF is process-wide and leaves the output uninitialized on failure.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let usage = unsafe { usage.assume_init() };
    let millis = |time: libc::timeval| time.tv_sec as f64 * 1000.0 + time.tv_usec as f64 / 1000.0;
    let read_counter = |path, field| {
        let text = std::fs::read_to_string(path).ok()?;
        let value = text.lines().find_map(|line| line.strip_prefix(field))?;
        value.split_whitespace().next()?.parse::<u64>().ok()
    };
    Some(HostResourceSample {
        cpu_ms: millis(usage.ru_utime) + millis(usage.ru_stime),
        resident_bytes: read_counter("/proc/self/status", "VmRSS:")
            .and_then(|kib| kib.checked_mul(1024)),
        disk_write_bytes: read_counter("/proc/self/io", "write_bytes:"),
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn read_host_resources() -> Option<HostResourceSample> {
    None
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[test]
fn host_resource_sample_reads_cumulative_cpu_and_resident_memory() {
    let first = read_host_resources().expect("host process counters");
    let next = read_host_resources().expect("host process counters");
    assert!(first.cpu_ms.is_finite() && first.cpu_ms >= 0.0);
    assert!(next.cpu_ms >= first.cpu_ms);
    assert!(next.resident_bytes.is_some_and(|bytes| bytes > 0));
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

//! Portal-first selection and the optional XFCE X11 capture adapter.
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Backend {
    Portal,
    X11(PathBuf),
}

#[cfg(target_os = "linux")]
pub(super) fn find_x11_tool() -> Option<PathBuf> {
    find_x11_tool_in(&std::env::var_os("PATH")?)
}

fn find_x11_tool_in(paths: &std::ffi::OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::env::split_paths(paths)
        .map(|directory| directory.join("xfce4-screenshooter"))
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

pub(super) fn select_backend(portal: bool, x11: bool, tool: Option<PathBuf>) -> Option<Backend> {
    if portal {
        Some(Backend::Portal)
    } else if x11 {
        tool.map(Backend::X11)
    } else {
        None
    }
}

pub(super) async fn capture_x11(tool: &Path, timeout: Duration) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncReadExt;
    let directory =
        tempfile::tempdir().map_err(|e| format!("cannot create screenshot directory: {e}"))?;
    let path = directory.path().join("screen.png");
    let mut command = crate::background_command::command(tool);
    // WSLg can export WAYLAND_DISPLAY alongside an XFCE X11 session. Match the
    // verified native app backend instead of letting GTK pick the other display.
    command.env("GDK_BACKEND", "x11");
    command
        .args(["--fullscreen", "--save"])
        .arg(&path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start X11 screenshot tool: {e}"))?;
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(result) => result.map_err(|e| format!("X11 screenshot tool failed: {e}"))?,
        Err(_) => {
            // Reap before removing the private output directory.
            let _ = child.kill().await;
            return Err("X11 screenshot tool timed out".into());
        }
    };
    if !status.success() {
        return Err(format!("X11 screenshot tool failed: {status}"));
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| format!("cannot open X11 screenshot: {e}"))?;
    let mut bytes = Vec::new();
    file.take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|e| format!("cannot read X11 screenshot: {e}"))?;
    if bytes.len() > 128 * 1024 * 1024 {
        return Err("screenshot exceeds 128 MB".into());
    }
    if !super::platform::valid_screenshot_png(&bytes) {
        return Err("X11 screenshot tool returned an invalid PNG".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn selection_prefers_portal_and_only_falls_back_on_native_x11() {
        let tool = PathBuf::from("/usr/bin/xfce4-screenshooter");
        assert_eq!(
            select_backend(true, true, Some(tool.clone())),
            Some(Backend::Portal)
        );
        assert_eq!(select_backend(true, false, None), Some(Backend::Portal));
        assert_eq!(
            select_backend(false, true, Some(tool.clone())),
            Some(Backend::X11(tool))
        );
        assert_eq!(
            select_backend(false, false, Some(PathBuf::from("tool"))),
            None
        );
        assert_eq!(select_backend(false, true, None), None);
    }

    fn helper(directory: &Path, script: &str) -> PathBuf {
        let path = directory.join("capture helper");
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[test]
    fn discovery_requires_an_executable_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("xfce4-screenshooter");
        assert_eq!(find_x11_tool_in(dir.path().as_os_str()), None);
        std::fs::create_dir(&tool).unwrap();
        assert_eq!(find_x11_tool_in(dir.path().as_os_str()), None);
        std::fs::remove_dir(&tool).unwrap();
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(find_x11_tool_in(dir.path().as_os_str()), None);
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(find_x11_tool_in(dir.path().as_os_str()), Some(tool));
    }

    #[tokio::test]
    async fn helper_returns_png_with_fixed_arguments_and_removes_temporary_image() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = dir.path().join("fixture.png");
        let png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDRfixture";
        std::fs::write(&fixture, png).unwrap();
        let recorded = dir.path().join("destination");
        let tool = helper(
            dir.path(),
            &format!(
                "[ \"$1\" = --fullscreen ] && [ \"$2\" = --save ] && [ \"$GDK_BACKEND\" = x11 ] || exit 7\nprintf '%s' \"$3\" > '{}'\ncp '{}' \"$3\"",
                recorded.display(),
                fixture.display()
            ),
        );
        assert_eq!(
            capture_x11(&tool, Duration::from_secs(2)).await.unwrap(),
            png
        );
        let destination = std::fs::read_to_string(recorded).unwrap();
        assert!(!Path::new(&destination).exists());
        assert!(!Path::new(&destination).parent().unwrap().exists());
    }

    #[tokio::test]
    async fn helper_rejects_failures_missing_invalid_and_oversized_images() {
        for script in [
            "exit 3",
            "exit 0",
            "printf invalid > \"$3\"",
            "dd if=/dev/zero of=\"$3\" bs=1 count=1 seek=134217728 2>/dev/null",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let tool = helper(dir.path(), script);
            assert!(
                capture_x11(&tool, Duration::from_secs(2)).await.is_err(),
                "{script}"
            );
        }
    }

    #[tokio::test]
    async fn helper_timeout_kills_capture_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let recorded = dir.path().join("destination");
        let tool = helper(
            dir.path(),
            &format!(
                "printf '%s' \"$3\" > '{}'\nexec sleep 30",
                recorded.display()
            ),
        );
        let result = capture_x11(&tool, Duration::from_secs(2)).await;
        assert!(result.unwrap_err().contains("timed out"));
        let destination = std::fs::read_to_string(recorded).unwrap();
        assert!(!Path::new(&destination).parent().unwrap().exists());
    }
}

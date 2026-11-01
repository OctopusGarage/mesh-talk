//! Portal-first selection and the optional XFCE X11 capture adapter.
use crate::commands::CommandError;
use std::{
    future::Future,
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

pub(super) async fn resolve_backend(
    portal: impl Future<Output = bool>,
    x11: bool,
    tool: Option<PathBuf>,
) -> Result<Backend, CommandError> {
    let portal = tokio::time::timeout(Duration::from_secs(3), portal)
        .await
        .unwrap_or(false);
    select_backend(portal, x11, tool).ok_or_else(|| {
        CommandError::Internal(
            "screenshot unavailable: requires a Screenshot portal or X11 with xfce4-screenshooter"
                .into(),
        )
    })
}

async fn portal_supports_screen<E>(
    version: u32,
    targets: impl Future<Output = Result<bool, E>>,
) -> bool {
    version < 3 || targets.await.unwrap_or(false)
}

#[cfg(target_os = "linux")]
pub(super) async fn portal_available() -> bool {
    use ashpd::desktop::screenshot::{AvailableTargets, ScreenshotProxy};
    let Ok(proxy) = ScreenshotProxy::new().await else {
        return false;
    };
    portal_supports_screen(proxy.version(), async {
        proxy
            .available_targets()
            .await
            .map(|targets| targets.contains(AvailableTargets::Screen))
    })
    .await
}

pub(super) async fn capture(
    backend: Backend,
    portal: impl Future<Output = Result<Vec<u8>, CommandError>>,
) -> Result<Vec<u8>, CommandError> {
    match backend {
        Backend::Portal => portal.await,
        Backend::X11(tool) => capture_x11(&tool, Duration::from_secs(15))
            .await
            .map_err(CommandError::Internal),
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

    #[tokio::test]
    async fn portal_versions_require_screen_support_only_from_version_three() {
        assert!(
            portal_supports_screen::<()>(2, async { panic!("legacy portal has no targets") }).await
        );
        assert!(portal_supports_screen(3, async { Ok::<_, ()>(true) }).await);
        assert!(!portal_supports_screen(3, async { Ok::<_, ()>(false) }).await);
        assert!(!portal_supports_screen(4, async { Err(()) }).await);
    }

    #[tokio::test]
    async fn backend_resolution_preserves_portal_priority_and_reports_unavailability() {
        let tool = PathBuf::from("xfce4-screenshooter");
        assert_eq!(
            resolve_backend(async { true }, true, Some(tool.clone()))
                .await
                .unwrap(),
            Backend::Portal
        );
        assert_eq!(
            resolve_backend(async { false }, true, Some(tool.clone()))
                .await
                .unwrap(),
            Backend::X11(tool.clone())
        );
        for (x11, tool) in [(false, Some(tool)), (true, None)] {
            assert!(matches!(
                resolve_backend(async { false }, x11, tool).await,
                Err(CommandError::Internal(message)) if message.contains("screenshot unavailable")
            ));
        }
    }

    #[tokio::test]
    async fn stalled_portal_probe_is_cancelled_before_x11_fallback() {
        struct Probe(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Probe {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let probe = Probe(cancelled.clone());
        let tool = PathBuf::from("xfce4-screenshooter");
        let backend = resolve_backend(
            async move {
                let _probe = probe;
                std::future::pending::<bool>().await
            },
            true,
            Some(tool.clone()),
        )
        .await
        .unwrap();
        assert_eq!(backend, Backend::X11(tool));
        assert!(cancelled.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn portal_capture_returns_cancellation_bytes_and_errors_without_switching_backend() {
        for bytes in [Vec::new(), b"portal PNG bytes".to_vec()] {
            let expected = bytes.clone();
            assert_eq!(
                capture(Backend::Portal, async { Ok(bytes) }).await.unwrap(),
                expected
            );
        }
        assert!(matches!(
            capture(Backend::Portal, async { Err(CommandError::Authorization("denied".into())) }).await,
            Err(CommandError::Authorization(message)) if message == "denied"
        ));
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            capture(Backend::X11(dir.path().join("missing")), async { panic!("X11 errors must not invoke the portal") }).await,
            Err(CommandError::Internal(message)) if message.contains("cannot start X11 screenshot tool")
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn missing_session_bus_resolves_the_installed_x11_helper() {
        const CHILD: &str = "MESH_TALK_SCREENSHOT_PROBE_TEST";
        if let Some(tool) = std::env::var_os(CHILD) {
            let tool = PathBuf::from(tool);
            assert_eq!(find_x11_tool(), Some(tool.clone()));
            assert_eq!(
                resolve_backend(portal_available(), true, find_x11_tool())
                    .await
                    .unwrap(),
                Backend::X11(tool.clone())
            );
            let app = tauri::test::mock_builder()
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            // An installed helper alone must not enable capture without a native window.
            assert!(
                super::super::platform::linux_screenshot_backend(app.handle())
                    .await
                    .is_err()
            );
            let _window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .build()
                .unwrap();
            assert_eq!(
                super::super::platform::linux_screenshot_backend(app.handle())
                    .await
                    .unwrap(),
                Backend::X11(tool)
            );
            return;
        }
        // Isolate ashpd's cached session connection and process environment from
        // the other tests. No user session bus or portal is contacted.
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("xfce4-screenshooter");
        std::fs::write(&tool, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = crate::background_command::command(std::env::current_exe().unwrap());
        command.args(["--exact", "chat_commands::linux_screenshot::tests::missing_session_bus_resolves_the_installed_x11_helper"])
            .env(CHILD, &tool)
            .env("PATH", dir.path())
            .env("DBUS_SESSION_BUS_ADDRESS", format!("unix:path={}", dir.path().join("missing-bus").display()))
            .kill_on_drop(true);
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"));
    }

    fn helper(directory: &Path, script: &str) -> PathBuf {
        let path = directory.join("capture helper");
        // A parallel parent fork must never inherit a writable executable FD.
        // The single-threaded producer opens it only after exec; wait for that
        // producer to exit before making the completed helper executable.
        let output = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                r#"printf '#!/bin/sh\n%s\n' "$2" > "$1""#,
                "screenshot-test-helper",
            ])
            .arg(&path)
            .arg(script)
            .output()
            .expect("screenshot fixture producer must start");
        assert!(
            output.status.success(),
            "screenshot fixture producer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
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
            capture(Backend::X11(tool), async {
                panic!("X11 capture must not invoke the portal")
            })
            .await
            .unwrap(),
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

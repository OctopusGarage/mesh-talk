//! Opt-in WebKitGTK compatibility, configured before the desktop runtime starts.

use std::ffi::OsStr;

fn should_disable_dmabuf<'a>(
    args: impl IntoIterator<Item = &'a OsStr>,
    existing_override: Option<&OsStr>,
) -> bool {
    existing_override.is_none() && compatibility_requested(args)
}

fn compatibility_requested<'a>(args: impl IntoIterator<Item = &'a OsStr>) -> bool {
    args.into_iter()
        .take_while(|arg| *arg != OsStr::new("--"))
        .any(|arg| arg == OsStr::new("--linux-renderer-compat"))
}

/// Call only from `main`, before Tauri, GTK or any application threads start.
#[cfg(target_os = "linux")]
pub(super) fn configure() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !compatibility_requested(args.iter().map(|arg| arg.as_os_str())) {
        return;
    }
    let existing = std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER");
    if should_disable_dmabuf(args.iter().map(|arg| arg.as_os_str()), existing.as_deref()) {
        // Set before any threads/WebKit initialization; never change compositing or GDK.
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
        eprintln!("Linux renderer compatibility: DMA-BUF renderer disabled for this launch.");
    } else {
        eprintln!("Linux renderer compatibility: preserving explicit WEBKIT_DISABLE_DMABUF_RENDERER; unset it to use the compatibility default.");
    }
    eprintln!("Fully quit any existing Mesh-Talk instance (including the tray) before switching renderer mode; a second launch only focuses the existing window.");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decision(args: &[&str], existing: Option<&str>) -> bool {
        should_disable_dmabuf(args.iter().map(OsStr::new), existing.map(OsStr::new))
    }

    #[test]
    fn explicit_compatibility_requests_dmabuf_fallback() {
        assert!(decision(&["--linux-renderer-compat"], None));
    }

    #[test]
    fn ordinary_and_hidden_startup_do_not_change_renderer() {
        assert!(!decision(&[], None));
        assert!(!decision(&["--hidden"], None));
    }

    #[test]
    fn only_exact_flag_before_argument_terminator_is_accepted() {
        for args in [
            vec!["--linux-renderer-compat=1"],
            vec!["prefix--linux-renderer-compat"],
            vec!["--", "--linux-renderer-compat"],
        ] {
            assert!(!decision(&args, None));
        }
        assert!(decision(
            &["--hidden", "--linux-renderer-compat", "--"],
            None
        ));
    }

    #[test]
    fn explicit_environment_always_takes_precedence() {
        for value in ["0", "1", "", "custom"] {
            assert!(!decision(&["--linux-renderer-compat"], Some(value)));
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_environment_is_also_preserved() {
        use std::os::unix::ffi::OsStrExt;
        assert!(!should_disable_dmabuf(
            [OsStr::new("--linux-renderer-compat")],
            Some(OsStr::from_bytes(&[0xff])),
        ));
    }
}

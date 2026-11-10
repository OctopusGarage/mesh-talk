//! Own initial launch visibility and explicit hide/show precedence. Preserve saved
//! geometry and X11 in-process placement without overriding Wayland placement.
use tauri::Window;

#[cfg(target_os = "linux")]
use std::{collections::HashMap, sync::Mutex};
#[cfg(target_os = "linux")]
use tauri::{Manager, PhysicalPosition};

#[cfg(target_os = "linux")]
#[derive(Default)]
pub(crate) struct HiddenPositions(Mutex<HashMap<String, PhysicalPosition<i32>>>);

#[cfg(target_os = "linux")]
pub(crate) fn is_x11<R: tauri::Runtime>(window: &Window<R>) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        window.window_handle().map(|handle| handle.as_raw()),
        Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
    )
}

pub(crate) fn hide<R: tauri::Runtime>(window: &Window<R>) -> tauri::Result<()> {
    retire_initial_presentation(window);
    #[cfg(target_os = "linux")]
    if is_x11(window) && window.is_visible()? {
        // Repeated hides must not replace the remembered position with hidden
        // geometry. Do not reposition maximized, minimized or fullscreen windows.
        let position =
            if !window.is_maximized()? && !window.is_minimized()? && !window.is_fullscreen()? {
                window.outer_position().ok()
            } else {
                None
            };
        let state = window.state::<HiddenPositions>();
        let mut positions = state.0.lock().unwrap_or_else(|error| error.into_inner());
        positions.remove(window.label());
        if let Some(position) = position {
            positions.insert(window.label().to_owned(), position);
        }
    }
    window.hide()
}

pub(crate) fn show<R: tauri::Runtime>(window: &Window<R>) -> tauri::Result<()> {
    retire_initial_presentation(window);
    show_preserving_position(window)
}

/// Explicit user activation restores both hidden and minimized main windows.
/// Reuse `show` so startup retirement and platform placement stay authoritative.
pub(crate) fn restore_main<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    use tauri::Manager;
    if let Some(window) = app
        .get_webview_window("main")
        .map(|view| view.as_ref().window())
    {
        show(&window)?;
        window.unminimize()?;
        window.set_focus()?;
    }
    Ok(())
}

fn show_preserving_position<R: tauri::Runtime>(window: &Window<R>) -> tauri::Result<()> {
    #[cfg(target_os = "linux")]
    let position = if is_x11(window)
        && !window.is_visible()?
        && !window.is_maximized()?
        && !window.is_minimized()?
        && !window.is_fullscreen()?
    {
        let state = window.state::<HiddenPositions>();
        state
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(window.label())
            .copied()
    } else {
        None
    };
    window.show()?;
    #[cfg(target_os = "linux")]
    if let Some(position) = position {
        // Apply after mapping: the window manager can otherwise choose a fresh
        // placement. Tauri serializes these native requests on its event loop.
        window.set_position(position)?;
    }
    Ok(())
}

/// Initial normal launches wait for the renderer's authorized, decoded surface.
/// Explicit visibility always wins over a late renderer acknowledgement or timer.
pub(crate) struct InitialPresentation(std::sync::atomic::AtomicBool);

impl InitialPresentation {
    pub(crate) fn new(normal_launch: bool) -> Self {
        Self(std::sync::atomic::AtomicBool::new(normal_launch))
    }

    fn retire(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn claim(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

pub(crate) fn retire_initial_presentation<R: tauri::Runtime>(window: &Window<R>) {
    use tauri::Manager;
    if window.label() == "main"
        && let Some(state) = window.try_state::<InitialPresentation>()
    {
        state.retire();
    }
}

pub(crate) fn observe_window_event<R: tauri::Runtime>(
    window: &Window<R>,
    event: &tauri::WindowEvent,
) {
    if matches!(event, tauri::WindowEvent::CloseRequested { .. })
        || (matches!(event, tauri::WindowEvent::Resized(_))
            && window.is_minimized().unwrap_or(false))
    {
        retire_initial_presentation(window);
    }
}

fn complete_initial_presentation<R: tauri::Runtime>(window: &Window<R>) -> tauri::Result<()> {
    use tauri::Manager;
    let queued_window = window.clone();
    window.run_on_main_thread(move || {
        // Claim here, after older user hide/show requests have retired startup.
        if queued_window
            .try_state::<InitialPresentation>()
            .is_some_and(|state| state.claim())
        {
            let _ = show_preserving_position(&queued_window);
            let _ = queued_window.set_focus();
        }
    })
}

pub(crate) fn defer_initial_presentation(app: &tauri::AppHandle, normal_launch: bool) {
    use tauri::Manager;
    if !normal_launch {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Fault liveness: an unavailable renderer or failed image must not leave
        // the normal-launch window permanently inaccessible. The renderer's
        // owner gate keeps its unresolved/rejected states free of private art.
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        if let Some(window) = app.get_webview_window("main") {
            let _ = complete_initial_presentation(&window.as_ref().window());
        }
    });
}

#[tauri::command]
pub(crate) fn initial_presentation_ready<R: tauri::Runtime>(
    window: tauri::Window<R>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Initial presentation belongs to the main window".into());
    }
    complete_initial_presentation(&window).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_main_retires_pending_startup_and_tolerates_missing_window() {
        use tauri::Manager;
        let app = tauri::test::mock_builder()
            .manage(InitialPresentation::new(true))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        restore_main(app.handle()).unwrap();
        assert!(
            app.state::<InitialPresentation>()
                .0
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        #[cfg(target_os = "linux")]
        app.manage(HiddenPositions::default());
        tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        restore_main(app.handle()).unwrap();
        restore_main(app.handle()).unwrap();
        assert!(!app.state::<InitialPresentation>().claim());
    }

    #[test]
    fn initial_presentation_is_one_shot_for_acknowledgement_and_fallback() {
        let state = InitialPresentation::new(true);
        assert!(state.claim());
        assert!(!state.claim());
    }

    #[test]
    fn hidden_launch_and_explicit_hide_never_allow_delayed_presentation() {
        assert!(!InitialPresentation::new(false).claim());
        let state = InitialPresentation::new(true);
        // The queued acknowledgement has not claimed yet: user hide wins.
        state.retire();
        assert!(!state.claim());
    }

    #[test]
    fn registered_initial_presentation_checks_window_and_retires_on_explicit_hide() {
        use tauri::Manager;
        let app = tauri::test::mock_builder()
            .manage(InitialPresentation::new(true))
            .invoke_handler(tauri::generate_handler![initial_presentation_ready])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        #[cfg(target_os = "linux")]
        app.manage(HiddenPositions::default());
        let main = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .visible(false)
            .build()
            .unwrap();
        let other = tauri::WebviewWindowBuilder::new(&app, "other", Default::default())
            .visible(false)
            .build()
            .unwrap();
        let invoke = |view: &tauri::WebviewWindow<tauri::test::MockRuntime>| {
            tauri::test::get_ipc_response(
                view,
                tauri::webview::InvokeRequest {
                    cmd: "initial_presentation_ready".into(),
                    callback: tauri::ipc::CallbackFn(0),
                    error: tauri::ipc::CallbackFn(1),
                    url: if cfg!(windows) {
                        "http://tauri.localhost"
                    } else {
                        "tauri://localhost"
                    }
                    .parse()
                    .unwrap(),
                    body: tauri::ipc::InvokeBody::Json(serde_json::json!({})),
                    headers: Default::default(),
                    invoke_key: tauri::test::INVOKE_KEY.into(),
                },
            )
        };
        assert!(invoke(&other).is_err());
        assert!(
            app.state::<InitialPresentation>()
                .0
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        observe_window_event(&main.as_ref().window(), &tauri::WindowEvent::Focused(false));
        observe_window_event(
            &main.as_ref().window(),
            &tauri::WindowEvent::Resized(tauri::PhysicalSize::new(900, 600)),
        );
        assert!(
            app.state::<InitialPresentation>()
                .0
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        hide(&main.as_ref().window()).unwrap();
        assert!(invoke(&main).is_ok());
        assert!(!app.state::<InitialPresentation>().claim());
        // A separately armed launch can also be surfaced by explicit user
        // intent; that show retires any later renderer/fallback work.
        app.state::<InitialPresentation>()
            .0
            .store(true, std::sync::atomic::Ordering::SeqCst);
        show(&main.as_ref().window()).unwrap();
        assert!(invoke(&main).is_ok());
        assert!(!app.state::<InitialPresentation>().claim());
    }

    #[test]
    fn native_main_thread_completion_claims_once_and_never_arms_a_hidden_launch() {
        use tauri::Manager;
        for normal_launch in [true, false] {
            let app = tauri::test::mock_builder()
                .manage(InitialPresentation::new(normal_launch))
                .build(tauri::test::mock_context(tauri::test::noop_assets()))
                .unwrap();
            #[cfg(target_os = "linux")]
            app.manage(HiddenPositions::default());
            let view = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .visible(false)
                .build()
                .unwrap();
            complete_initial_presentation(&view.as_ref().window()).unwrap();
            complete_initial_presentation(&view.as_ref().window()).unwrap();
            assert!(!app.state::<InitialPresentation>().claim());
        }
    }

    #[test]
    fn queued_native_acknowledgement_cannot_claim_before_dispatch_or_undo_user_hide() {
        use tauri::Manager;
        let app = tauri::test::mock_builder()
            .manage(InitialPresentation::new(true))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        #[cfg(target_os = "linux")]
        app.manage(HiddenPositions::default());
        let view = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .visible(false)
            .build()
            .unwrap();
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        let (hidden_tx, hidden_rx) = std::sync::mpsc::channel();
        let mut hidden_rx = Some(hidden_rx);
        app.run(move |app, event| {
            if matches!(event, tauri::RunEvent::Ready) {
                let window = view.as_ref().window();
                let worker_window = window.clone();
                let queued_tx = queued_tx.clone();
                let hidden_rx = hidden_rx.take().unwrap();
                std::thread::spawn(move || {
                    complete_initial_presentation(&worker_window).unwrap();
                    queued_tx.send(()).unwrap();
                    hidden_rx
                        .recv_timeout(std::time::Duration::from_secs(2))
                        .unwrap();
                    let handle = worker_window.app_handle().clone();
                    worker_window
                        .run_on_main_thread(move || {
                            assert!(!handle.state::<InitialPresentation>().claim());
                            handle.get_webview_window("main").unwrap().close().unwrap();
                        })
                        .unwrap();
                });
                queued_rx
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .unwrap();
                // The native event loop is inside Ready: queued work cannot
                // have claimed permission before this explicit hide.
                assert!(
                    app.state::<InitialPresentation>()
                        .0
                        .load(std::sync::atomic::Ordering::SeqCst)
                );
                hide(&window).unwrap();
                hidden_tx.send(()).unwrap();
            }
        });
    }
}

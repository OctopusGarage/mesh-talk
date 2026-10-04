//! Real desktop entry point for native contact evaluation. The driver dependency
//! is dev-only. Cargo's existing example dev-dependencies enable fast-test-kdf;
//! the separately built production app and CLI retain regular KDF parameters.

#[cfg(target_os = "macos")]
fn native_keyboard_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri::{Emitter, Listener, Manager};
    #[derive(serde::Deserialize)]
    struct KeyRequest {
        key: String,
        nonce: String,
    }
    tauri::plugin::Builder::new("native-contact-keyboard")
        .setup(|app, _| {
            let chrome_handle = app.clone();
            app.listen("native-contact-chrome", move |event| {
                let nonce = event.payload().to_owned();
                let result_handle = chrome_handle.clone();
                let _ = chrome_handle.run_on_main_thread(move || {
                    let result = native_chrome_bounds();
                    let _ = result_handle.emit(
                        "native-contact-chrome-result",
                        serde_json::json!({"nonce": nonce, "result": result}),
                    );
                });
            });
            let handle = app.clone();
            app.listen("native-contact-key", move |event| {
                let Ok(request) = serde_json::from_str::<KeyRequest>(event.payload()) else {
                    return;
                };
                let result_handle = handle.clone();
                let nonce = request.nonce.clone();
                let queue_error_handle = handle.clone();
                let queued = handle.run_on_main_thread(move || {
                    let result = dispatch_native_key(&request.key).and_then(|()| {
                        if request.key == "Focus" {
                            // Activating NSWindow alone does not make WKWebView
                            // first responder. Focus the owned production webview
                            // through Wry's native makeFirstResponder path.
                            result_handle
                                .get_webview_window("main")
                                .ok_or_else(|| "Owned main webview missing".to_string())?
                                .as_ref()
                                .set_focus()
                                .map_err(|error| error.to_string())?;
                        }
                        Ok(())
                    });
                    let _ = result_handle.emit(
                        "native-contact-key-result",
                        serde_json::json!({ "nonce": request.nonce, "error": result.err(), "focus": native_focus_state() }),
                    );
                });
                if let Err(error) = queued {
                    let _ = queue_error_handle.emit(
                        "native-contact-key-result",
                        serde_json::json!({ "nonce": nonce, "error": error.to_string() }),
                    );
                }
            });
            Ok(())
        })
        .build()
}

#[cfg(target_os = "macos")]
fn native_focus_state() -> serde_json::Value {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;
    let Some(main) = MainThreadMarker::new() else {
        return serde_json::json!({"error":"not main thread"});
    };
    let app = NSApplication::sharedApplication(main);
    let window = app.keyWindow().or_else(|| unsafe { app.mainWindow() });
    // SAFETY: observation runs on the AppKit main thread, in the owned app.
    serde_json::json!({
        "applicationActive": unsafe { app.isActive() },
        "keyWindow": window.as_ref().is_some_and(|window| window.isKeyWindow()),
        "firstResponderClass": window.and_then(|window| window.firstResponder()).map(|responder| responder.class().name().to_string_lossy().into_owned()),
    })
}

/// Real AppKit standard-button rectangles in content-view logical coordinates,
/// with a top-left origin matching the native webview's DOM viewport.
#[cfg(target_os = "macos")]
fn native_chrome_bounds() -> Result<serde_json::Value, String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSWindowButton};
    let main = MainThreadMarker::new().ok_or("Chrome observation requires main thread")?;
    let app = NSApplication::sharedApplication(main);
    let window = app.keyWindow().ok_or("Owned app has no key window")?;
    let content = window
        .contentView()
        .ok_or("Owned window has no content view")?;
    let height = content.bounds().size.height;
    let mut buttons = Vec::new();
    for kind in [
        NSWindowButton::CloseButton,
        NSWindowButton::MiniaturizeButton,
        NSWindowButton::ZoomButton,
    ] {
        let button = window
            .standardWindowButton(kind)
            .ok_or("Standard window button missing")?;
        let rect = button.convertRect_toView(button.bounds(), Some(&content));
        let top = if content.isFlipped() {
            rect.origin.y
        } else {
            height - rect.origin.y - rect.size.height
        };
        buttons.push(serde_json::json!({"x":rect.origin.x,"y":top,"right":rect.origin.x+rect.size.width,"bottom":top+rect.size.height}));
    }
    Ok(
        serde_json::json!({"source":"AppKit standardWindowButton converted to contentView", "buttons":buttons}),
    )
}

/// Deliver real AppKit key events to this process's key window. This exercises
/// WKWebView's default Tab/Enter handling without global input or TCC changes;
/// it is native in-process automation, not a physical keyboard user study.
#[cfg(target_os = "macos")]
fn dispatch_native_key(key: &str) -> Result<(), String> {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
    use objc2_foundation::{NSPoint, NSString};
    let main = MainThreadMarker::new().ok_or("Native input must run on the main thread")?;
    let application = NSApplication::sharedApplication(main);
    if key == "Focus" {
        // SAFETY: AppKit's main thread, and only this application's own windows.
        let window = unsafe { application.mainWindow() }
            .or_else(|| application.keyWindow())
            .or_else(|| application.windows().firstObject())
            .ok_or("Owned app has no window")?;
        // This dev-only driver also supports macOS versions before activate().
        #[allow(deprecated)]
        application.activateIgnoringOtherApps(true);
        window.makeKeyAndOrderFront(None);
        return Ok(());
    }
    let (code, characters) = match key {
        "Tab" => (48, "\t"),
        "Enter" => (36, "\r"),
        "Escape" => (53, "\u{1b}"),
        _ => return Err("Unsupported evaluation key".into()),
    };
    let window = application
        .keyWindow()
        .ok_or("Owned app has no key window")?;
    let characters = NSString::from_str(characters);
    for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
        // SAFETY: executed on AppKit's main thread; valid owned key window,
        // immutable strings, supported key codes, nil graphics context, and all
        // retained objects outlive synchronous delivery into that same window.
        let event = unsafe {
            NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
                event_type, NSPoint::new(0.0, 0.0), NSEventModifierFlags::empty(), 0.0,
                window.windowNumber(), None, &characters, &characters, false, code,
            )
        }.ok_or("AppKit failed to construct an evaluation key event")?;
        window.sendEvent(&event);
    }
    Ok(())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = std::path::PathBuf::from(args.next().expect("absolute fixture data root required"));
    let port: u16 = args
        .next()
        .expect("driver port required")
        .parse()
        .expect("driver port");
    let discovery_port: u16 = args
        .next()
        .expect("isolated discovery port required")
        .parse()
        .expect("discovery port");
    assert!(args.next().is_none(), "unexpected arguments");
    assert!(root.is_absolute(), "fixture root must be absolute");
    std::fs::create_dir_all(&root).expect("create fixture root");
    // Isolate OS config, webview storage, and window-state plugin preferences as
    // well as the application's explicit data root, without changing HOME.
    mesh_talk::run_tauri_configured(Some(root.clone()), Some(discovery_port), |builder| {
        let builder = builder.plugin(tauri_plugin_wdio_webdriver::init_with_port(port));
        #[cfg(target_os = "macos")]
        let builder = builder.plugin(native_keyboard_plugin());
        builder
    });
}

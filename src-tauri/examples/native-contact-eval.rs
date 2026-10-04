//! Real desktop entry point for native contact evaluation. The driver dependency
//! is dev-only. Cargo's existing example dev-dependencies enable fast-test-kdf;
//! the separately built production app and CLI retain regular KDF parameters.

#[cfg(target_os = "macos")]
fn native_keyboard_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri::{Emitter, Listener};
    #[derive(serde::Deserialize)]
    struct KeyRequest {
        key: String,
        nonce: String,
    }
    tauri::plugin::Builder::new("native-contact-keyboard")
        .setup(|app, _| {
            let handle = app.clone();
            app.listen("native-contact-key", move |event| {
                let Ok(request) = serde_json::from_str::<KeyRequest>(event.payload()) else {
                    return;
                };
                let result_handle = handle.clone();
                let nonce = request.nonce.clone();
                let queue_error_handle = handle.clone();
                let queued = handle.run_on_main_thread(move || {
                    let result = dispatch_native_key(&request.key);
                    let _ = result_handle.emit(
                        "native-contact-key-result",
                        serde_json::json!({ "nonce": request.nonce, "error": result.err() }),
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

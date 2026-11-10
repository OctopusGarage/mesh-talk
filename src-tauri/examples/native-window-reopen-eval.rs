//! Exercise production close/reopen handling with real macOS windows and the
//! AppKit reopen delegate. Run with an absolute, disposable fixture data root.
//! This checks native dispatch, not a physical click on the Dock or menu bar.

#[cfg(target_os = "macos")]
fn main() {
    use std::time::{Duration, Instant};
    use tauri::Manager;

    assert!(
        std::env::args().any(|argument| argument == "--hidden"),
        "pass --hidden to verify hidden launch"
    );

    fn on_main<T: Send + 'static>(
        app: &tauri::AppHandle,
        action: impl FnOnce(&tauri::AppHandle) -> T + Send + 'static,
    ) -> T {
        let handle = app.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        app.run_on_main_thread(move || tx.send(action(&handle)).unwrap())
            .unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    fn wait_for(app: &tauri::AppHandle, visible: bool, minimized: bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let actual = on_main(app, |app| {
                let window = app.get_webview_window("main").unwrap();
                (window.is_visible().unwrap(), window.is_minimized().unwrap())
            });
            if actual == (visible, minimized) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "expected visibility/minimized {visible}/{minimized}, got {actual:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn reopen() {
        // Dispatch through Cocoa rather than a Tauri user-event callback: the
        // delegate must enter Tao outside its already-locked event handler.
        let block = block2::RcBlock::new(|| {
            use objc2::{MainThreadMarker, msg_send, runtime::Bool};
            use objc2_app_kit::NSApplication;
            let application = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            let delegate = application.delegate().unwrap();
            // SAFETY: Tauri installs an NSApplicationDelegate implementing this
            // AppKit selector; dispatch occurs on the native main thread.
            let _: Bool = unsafe {
                msg_send![&*delegate, applicationShouldHandleReopen: &*application, hasVisibleWindows: Bool::NO]
            };
        });
        // SAFETY: the block captures no thread-bound objects and runs on Cocoa's main queue.
        unsafe { objc2_foundation::NSOperationQueue::mainQueue().addOperationWithBlock(&block) };
    }

    let root = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("absolute disposable fixture data root required"),
    );
    assert!(root.is_absolute());
    std::fs::create_dir_all(&root).unwrap();
    mesh_talk::run_tauri_configured(Some(root), None, |builder| {
        builder.plugin(
            tauri::plugin::Builder::<tauri::Wry, ()>::new("native-window-reopen-eval")
                .on_event(|app, event| {
                    if matches!(event, tauri::RunEvent::Ready) {
                        let app = app.clone();
                        std::thread::spawn(move || {
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                // Hidden launch must stay hidden until explicit reopen.
                                wait_for(&app, false, false);
                                reopen();
                                wait_for(&app, true, false);
                                for _ in 0..3 {
                                    on_main(&app, |app| app.get_webview_window("main").unwrap().close().unwrap());
                                    wait_for(&app, false, false);
                                    reopen();
                                    wait_for(&app, true, false);
                                }
                                on_main(&app, |app| app.get_webview_window("main").unwrap().minimize().unwrap());
                                // AppKit reports a miniaturized window as not visible.
                                wait_for(&app, false, true);
                                reopen();
                                wait_for(&app, true, false);
                                on_main(&app, |app| assert!(app.get_webview_window("main").unwrap().is_focused().unwrap()));
                            }));
                            if result.is_ok() {
                                eprintln!("PASS: hidden launch, repeated close/reopen, minimized reopen and focus");
                            }
                            app.exit(if result.is_ok() { 0 } else { 1 });
                        });
                    }
                })
                .build(),
        )
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This native reopen evaluation requires macOS.");
    std::process::exit(1);
}

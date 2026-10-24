//! Disposable Linux SDK acceptance. No product runtime, renderer IPC or real profile.

#[cfg(target_os = "linux")]
mod linux {
    use sha2::{Digest, Sha256};
    use std::{fs, io::Write, os::unix::fs::PermissionsExt, path::Path, time::Duration};
    use tauri::Manager;
    use tauri_plugin_updater::UpdaterExt;

    const OLD: &str = concat!(env!("CARGO_PKG_VERSION"), "-nightly.900001.1");
    const NEW: &str = concat!(env!("CARGO_PKG_VERSION"), "-nightly.900001.2");

    fn record(root: &Path, event: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("events.log"))
            .expect("fixture event log");
        writeln!(file, "{} {event}", std::process::id()).expect("record event");
    }

    fn wait(root: &Path, name: &str) {
        for _ in 0..600 {
            if root.join(name).is_file() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("fixture controller did not provide {name}");
    }

    pub fn run() {
        let appimage = std::path::PathBuf::from(
            std::env::var_os("APPIMAGE").expect("actual AppImage runtime"),
        );
        assert_eq!(appimage.file_name().unwrap(), "UpdaterAcceptance.AppImage");
        let root = appimage.parent().unwrap().parent().unwrap().to_path_buf();
        assert!(root.join("disposable-updater-fixture").is_file());
        assert_eq!(std::env::var_os("HOME").unwrap(), root.join("home"));
        let appdir =
            std::path::PathBuf::from(std::env::var_os("APPDIR").expect("actual AppImage AppDir"));
        let identity = fs::read_to_string(appdir.join("logical-identity"))
            .expect("AppImage resource identity");
        assert!(identity == OLD || identity == NEW);
        record(&root, &format!("launch {identity}"));

        let mut context = tauri::generate_context!();
        context.config_mut().identifier = "com.mesh-talk.updater-linux-acceptance".into();
        context.config_mut().product_name = Some("UpdaterAcceptance".into());
        context.config_mut().app.windows.clear();
        let setup_root = root.clone();
        let app = tauri::Builder::default()
            .plugin(tauri_plugin_updater::Builder::new().build())
            .setup(move |app| {
                let window = tauri::WebviewWindowBuilder::new(app, "acceptance",
                    tauri::WebviewUrl::External("about:blank".parse().unwrap()))
                    .title(format!("UpdaterAcceptance {identity}"))
                    .inner_size(640.0, 320.0).build()?;
                window.eval(&format!("document.body.innerHTML = '<h1>Mesh Talk updater acceptance</h1><p>{identity}</p>'; document.body.style = 'background:#fff;color:#111;padding:32px;font:20px sans-serif'"))?;
                let root = setup_root.clone();
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    assert_eq!(tauri::process::current_binary(&handle.env()).unwrap(), appimage);
                    let marker = root.join("home/.mesh-talk/acceptance-marker");
                    assert_eq!(fs::read_to_string(marker).unwrap(), "preserve fixture data");
                    if identity == NEW {
                        assert!(root.join("installed").is_file());
                        record(&root, "relaunched-new-identity-data-preserved");
                        fs::write(root.join("passed"), "signed AppImage replaced and relaunched").unwrap();
                        wait(&root, "exit-approved");
                        handle.exit(0);
                        return;
                    }
                    fs::write(root.join("old-ready"), std::process::id().to_string()).unwrap();
                    wait(&root, "start-install");
                    let bytes = fs::read(root.join("update.AppImage")).unwrap();
                    assert_eq!(&bytes[..4], b"\x7fELF");
                    assert_eq!(&bytes[8..11], b"AI\x02");
                    let signature = fs::read_to_string(root.join("update.sig")).unwrap();
                    let key = fs::read_to_string(root.join("public.key")).unwrap();
                    let original = fs::read(&appimage).unwrap();
                    let mode = fs::metadata(&appimage).unwrap().permissions().mode() & 0o777;
                    let prepare = |version: &str, key: &str| {
                        handle.updater_builder().pubkey(key)
                            .endpoints(vec!["https://api.github.com/repos/OctopusGarage/mesh-talk/releases".parse().unwrap()]).unwrap()
                            .version_comparator(|_, remote| remote.version > semver::Version::parse(OLD).unwrap())
                            .build().unwrap()
                            .restore_update(serde_json::json!({
                                "version": version,
                                "url": "https://example.invalid/host-owned-fixture.AppImage",
                                "signature": signature.trim()
                            })).unwrap().unwrap()
                    };
                    let update = prepare(NEW, key.trim());
                    let mut tampered = bytes.clone();
                    tampered[0] ^= 1;
                    assert!(update.verify(&tampered).is_err());
                    assert!(update.install(&tampered).is_err());
                    assert_eq!(fs::read(&appimage).unwrap(), original);
                    record(&root, "tampered-payload-rejected-old-AppImage-preserved");
                    assert!(prepare(NEW, "invalid-public-key").install(&bytes).is_err());
                    assert_eq!(fs::read(&appimage).unwrap(), original);
                    record(&root, "wrong-key-rejected-old-AppImage-preserved");
                    assert!(prepare(concat!(env!("CARGO_PKG_VERSION"), "-nightly.900001.3"), key.trim()).install(&bytes).is_err());
                    assert_eq!(fs::read(&appimage).unwrap(), original);
                    record(&root, "relabelled-version-rejected-old-AppImage-preserved");
                    update.verify(&bytes).unwrap();
                    update.install(&bytes).unwrap();
                    assert_eq!(fs::read(&appimage).unwrap(), bytes);
                    assert_eq!(fs::metadata(&appimage).unwrap().permissions().mode() & 0o777, mode);
                    record(&root, &format!("signed-full-AppImage-installed sha256={:x} mode={mode:o}", Sha256::digest(&bytes)));
                    fs::write(root.join("installed"), NEW).unwrap();
                    handle.request_restart();
                });
                Ok(())
            })
            .build(context).expect("fixture app");
        app.run(move |_, event| match event {
            tauri::RunEvent::ExitRequested { .. } => record(&root, "exit-requested"),
            tauri::RunEvent::Exit => record(&root, "exit"),
            _ => {}
        });
    }
}

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("native-update-linux-eval requires Linux");
}

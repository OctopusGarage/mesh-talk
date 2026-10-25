//! Disposable Windows NSIS acceptance harness. Uses its own install identity and profile.

// Optional fixture branch exercises the production decoder before the real SDK install.
#[cfg(target_os = "windows")]
#[path = "../src/updates/delta_codec.rs"]
mod delta_codec;

#[cfg(target_os = "windows")]
fn fixture_payload(root: &std::path::Path, full_name: &str) -> Vec<u8> {
    use sha2::{Digest, Sha256};
    use std::{fs, io};
    let metadata = root.join("delta-fixture.json");
    if !metadata.is_file() {
        return fs::read(root.join(full_name)).unwrap();
    }
    let metadata: serde_json::Value = serde_json::from_slice(&fs::read(metadata).unwrap()).unwrap();
    let size = metadata["target_size"].as_u64().unwrap();
    let hash = metadata["target_sha256"].as_str().unwrap();
    let canonical_size = metadata["canonical_full_size"].as_u64().unwrap();
    let base = fs::read(std::env::current_exe().unwrap()).unwrap();
    let patch = fs::read(root.join("update.patch.zst")).unwrap();
    assert!(!patch.is_empty());
    assert!(
        (patch.len() as u64) * 10 <= canonical_size * 7,
        "fixture delta must save at least 30% against canonical compressed full"
    );
    assert!(
        delta_codec::reconstruct(&vec![0; base.len()], &patch, io::sink(), size, hash, || {
            false
        })
        .is_err()
    );
    assert!(
        delta_codec::reconstruct(
            &base,
            &patch[..patch.len() - 1],
            io::sink(),
            size,
            hash,
            || false
        )
        .is_err()
    );
    assert!(
        delta_codec::reconstruct(&base, &patch, io::sink(), size, &"0".repeat(64), || false)
            .is_err()
    );
    let mut bytes = Vec::new();
    delta_codec::reconstruct(&base, &patch, &mut bytes, size, hash, || false).unwrap();
    fs::write(root.join("reconstructed-native-payload"), &bytes).unwrap();
    fs::write(root.join("delta-evidence.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "scope": "isolated native SDK fixture; production decoder; no release discovery or public delta acceptance",
        "base_sha256": hex::encode(Sha256::digest(&base)),
        "patch_bytes": patch.len(), "canonical_compressed_full_bytes": canonical_size,
        "reconstructed_bytes": bytes.len(), "reconstructed_sha256": hex::encode(Sha256::digest(&bytes)),
        "negative_cases": ["wrong base", "truncated patch", "wrong target digest"]
    })).unwrap()).unwrap();
    bytes
}

#[cfg(target_os = "windows")]
fn main() {
    use std::{fs, io::Write, path::Path, time::Duration};
    use tauri_plugin_updater::UpdaterExt;

    fn record(root: &Path, event: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("events.log"))
            .expect("fixture event log");
        writeln!(file, "{} {event}", std::process::id()).expect("record event");
    }

    let exe = std::env::current_exe().expect("fixture executable");
    assert_eq!(exe.file_name().unwrap(), "WindowsUpdaterAcceptance.exe");
    let install = exe.parent().unwrap().to_path_buf();
    let root = install.parent().unwrap().to_path_buf();
    assert!(root.is_absolute());
    assert_eq!(
        root.components()
            .next()
            .unwrap()
            .as_os_str()
            .to_string_lossy()
            .to_ascii_lowercase(),
        "d:"
    );
    assert!(root.ancestors().any(|path| {
        path.file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("UpdaterAcceptance"))
    }));
    assert!(root.join("disposable-updater-fixture").is_file());
    let old = format!("{}-nightly.900002.1", env!("CARGO_PKG_VERSION"));
    let new = format!("{}-nightly.900002.2", env!("CARGO_PKG_VERSION"));
    let identity = fs::read_to_string(install.join("logical-identity")).unwrap();
    assert!(identity == old || identity == new);
    record(&root, &format!("launch {identity}"));

    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "com.mesh-talk.windows-updater-acceptance".into();
    context.config_mut().product_name = Some("WindowsUpdaterAcceptance".into());
    context.config_mut().app.windows.clear();
    let setup_root = root.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|_, _, _| {}))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External("about:blank".parse().unwrap()),
            )
            .title("Mesh Talk — disposable Windows updater acceptance")
            .inner_size(640.0, 360.0)
            .data_directory(setup_root.join("webview"))
            .build()?;
            let root = setup_root.clone();
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let marker = root.join("home/.mesh-talk/acceptance-marker");
                assert_eq!(fs::read_to_string(marker).unwrap(), "preserve fixture data");
                if identity == new {
                    record(&root, "relaunched-new-identity-data-preserved");
                    fs::write(
                        root.join("passed"),
                        "same-base signed NSIS replaced and relaunched",
                    )
                    .unwrap();
                    handle.exit(0);
                    return;
                }
                let bytes = fixture_payload(&root, "update.nsis.exe");
                let signature = fs::read_to_string(root.join("update.sig")).unwrap();
                let key = fs::read_to_string(root.join("public.key")).unwrap();
                let old_version = semver::Version::parse(&old).unwrap();
                let prepare = |version: &str, key: &str| {
                    handle
                        .updater_builder()
                        .pubkey(key)
                        .endpoints(vec![
                            "https://example.invalid/host-owned-fixture"
                                .parse()
                                .unwrap(),
                        ])
                        .unwrap()
                        .on_before_exit(|| {})
                        .version_comparator({
                            let old_version = old_version.clone();
                            move |_, remote| remote.version > old_version
                        })
                        .build()
                        .unwrap()
                        .restore_update(serde_json::json!({
                            "version": version,
                            "url": "https://example.invalid/host-owned-fixture.nsis.exe",
                            "signature": signature.trim()
                        }))
                        .unwrap()
                        .unwrap()
                };
                let update = prepare(&new, key.trim());
                let mut tampered = bytes.clone();
                tampered[0] ^= 1;
                assert!(update.install(&tampered).is_err());
                assert_eq!(
                    fs::read_to_string(install.join("logical-identity")).unwrap(),
                    old
                );
                record(&root, "tampered-payload-rejected-old-install-preserved");
                assert!(prepare(&new, "invalid-public-key").install(&bytes).is_err());
                record(&root, "wrong-key-rejected-old-install-preserved");
                assert!(
                    prepare(
                        &format!("{}-nightly.900002.3", env!("CARGO_PKG_VERSION")),
                        key.trim()
                    )
                    .install(&bytes)
                    .is_err()
                );
                record(&root, "relabelled-version-rejected-old-install-preserved");
                update.verify(&bytes).unwrap();
                record(&root, "signed-installer-verified-awaiting-handoff");
                fs::write(root.join("ready"), "verified").unwrap();
                for _ in 0..600 {
                    if root.join("go-install").is_file() {
                        record(&root, "installer-handoff-requested");
                        if let Err(error) = update.install(&bytes) {
                            record(&root, &format!("installer-launch-failed {error}"));
                            fs::write(root.join("launch-failed"), error.to_string()).unwrap();
                        }
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                record(&root, "handoff-timeout");
                handle.exit(1);
            });
            Ok(())
        })
        .build(context)
        .expect("fixture app");
    app.run(move |_, event| match event {
        tauri::RunEvent::ExitRequested { .. } => record(&root, "exit-requested"),
        tauri::RunEvent::Exit => record(&root, "exit"),
        _ => {}
    });
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("native-update-windows-eval requires Windows");
}

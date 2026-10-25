//! Disposable macOS SDK acceptance harness. Never runs the product's profile/runtime.
//! Host fixtures live beside `install/UpdaterAcceptance.app`; no renderer IPC is exposed.

// Optional fixture branch exercises the production decoder before the real SDK install.
#[cfg(target_os = "macos")]
#[path = "../src/updates/delta_codec.rs"]
mod delta_codec;

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn main() {
    use std::{fs, io::Write, path::Path};
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
    let bundle = exe.parent().unwrap().parent().unwrap().parent().unwrap();
    assert_eq!(bundle.file_name().unwrap(), "UpdaterAcceptance.app");
    let root = bundle.parent().unwrap().parent().unwrap().to_path_buf();
    assert!(root.join("disposable-updater-fixture").is_file());
    let identity_path = bundle.join("Contents/Resources/logical-identity");
    let identity = fs::read_to_string(&identity_path).expect("fixture identity");
    let base = env!("CARGO_PKG_VERSION");
    let old = format!("{base}-nightly.900001.1");
    let new = format!("{base}-nightly.900001.2");
    assert!(identity == old || identity == new);
    record(&root, &format!("launch {identity}"));

    let mut context = tauri::generate_context!();
    context.config_mut().identifier = "com.mesh-talk.updater-acceptance".into();
    context.config_mut().product_name = Some("UpdaterAcceptance".into());
    context.config_mut().app.windows.clear();
    let setup_root = root.clone();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(move |app| {
            let root = setup_root.clone();
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                let marker = root.join("home/.mesh-talk/acceptance-marker");
                assert_eq!(fs::read_to_string(marker).unwrap(), "preserve fixture data");
                if identity == new {
                    assert!(root.join("installed").is_file());
                    record(&root, "relaunched-new-identity-data-preserved");
                    if root.join("launchagent-origin").is_file() {
                        fs::write(
                            root.join("relaunched-ready"),
                            std::process::id().to_string(),
                        )
                        .unwrap();
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(45);
                        while !root.join("allow-new-exit").is_file() {
                            assert!(
                                std::time::Instant::now() < deadline,
                                "runner did not release new process"
                            );
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        record(&root, "launchagent-origin-new-process-survived-old-job");
                    }
                    fs::write(
                        root.join("passed"),
                        "same-base signed bundle replaced and relaunched",
                    )
                    .unwrap();
                    handle.exit(0);
                    return;
                }
                let bytes = fixture_payload(&root, "update.app.tar.gz");
                let signature = fs::read_to_string(root.join("update.sig")).unwrap();
                let key = fs::read_to_string(root.join("public.key")).unwrap();
                let current = semver::Version::parse(&old).unwrap();
                let prepare = |version: &str, key: &str| {
                    let current = current.clone();
                    handle
                        .updater_builder()
                        .pubkey(key)
                        .endpoints(vec![
                            "https://api.github.com/repos/OctopusGarage/mesh-talk/releases"
                                .parse()
                                .unwrap(),
                        ])
                        .unwrap()
                        .version_comparator(move |_, remote| remote.version > current)
                        .build()
                        .unwrap()
                        .restore_update(serde_json::json!({
                            "version": version,
                            "url": "https://example.invalid/host-owned-fixture.app.tar.gz",
                            "signature": signature.trim()
                        }))
                        .unwrap()
                        .unwrap()
                };
                let update = prepare(&new, key.trim());
                let mut tampered = bytes.clone();
                tampered[0] ^= 1;
                assert!(update.verify(&tampered).is_err());
                assert!(update.install(&tampered).is_err());
                assert_eq!(fs::read_to_string(&identity_path).unwrap(), old);
                record(&root, "tampered-payload-rejected-old-bundle-preserved");
                assert!(prepare(&new, "invalid-public-key").install(&bytes).is_err());
                assert_eq!(fs::read_to_string(&identity_path).unwrap(), old);
                record(&root, "wrong-key-rejected-old-bundle-preserved");
                assert!(
                    prepare(&format!("{base}-nightly.900001.3"), key.trim())
                        .install(&bytes)
                        .is_err()
                );
                assert_eq!(fs::read_to_string(&identity_path).unwrap(), old);
                record(&root, "relabelled-version-rejected-old-bundle-preserved");
                update.verify(&bytes).unwrap();
                if root.join("expect-authorization-cancel").is_file() {
                    record(&root, "awaiting-owned-authorization-cancel");
                    let error = update
                        .install(&bytes)
                        .expect_err("authorization must be canceled");
                    assert_eq!(fs::read_to_string(&identity_path).unwrap(), old);
                    assert_eq!(
                        fs::read_to_string(root.join("home/.mesh-talk/acceptance-marker")).unwrap(),
                        "preserve fixture data"
                    );
                    record(
                        &root,
                        &format!("authorization-canceled-old-bundle-preserved {error}"),
                    );
                    let main_handle = handle.clone();
                    handle
                        .run_on_main_thread(move || {
                            record(&root, "old-process-main-loop-responsive-after-cancel");
                            fs::write(
                                root.join("passed"),
                                "canceled authorization preserved old bundle and data",
                            )
                            .unwrap();
                            main_handle.exit(0);
                        })
                        .unwrap();
                    return;
                }
                update.install(&bytes).unwrap();
                assert_eq!(fs::read_to_string(&identity_path).unwrap(), new);
                record(&root, "signed-full-bundle-installed");
                fs::write(root.join("installed"), &new).unwrap();
                handle.request_restart();
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

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("native-update-eval requires macOS");
}

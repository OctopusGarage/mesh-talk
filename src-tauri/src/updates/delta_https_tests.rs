//! Signed delta transport acceptance over a real local HTTPS connection.
use super::*;

#[cfg(any(target_os = "macos", target_os = "windows"))]
async fn exercise_signed_delta(mode: &str) {
    use base64::Engine;
    let full_flow = mode.starts_with("full_");
    let mode = mode.strip_prefix("full_").unwrap_or(mode);
    let _state = TEST_STATE.lock().await;
    clear_stage().await;
    let app = test_app();
    let server = HttpsRelease::new().await;
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/delta-native.json")).unwrap();
    let decode = |v: &serde_json::Value| {
        base64::engine::general_purpose::STANDARD
            .decode(v.as_str().unwrap())
            .unwrap()
    };
    let platform = format!("{}_{}", std::env::consts::OS, std::env::consts::ARCH);
    let catalog_fixture = if matches!(
        mode,
        "oversize_target" | "oversize_base" | "unsupported_codec" | "catalog_identity"
    ) {
        &fixture["platforms"][&platform][mode]
    } else {
        &fixture["platforms"][&platform]
    };
    let catalog_bytes = decode(&catalog_fixture["catalog"]);
    let catalog: serde_json::Value = serde_json::from_slice(&catalog_bytes).unwrap();
    let patch = decode(&fixture["patch"]);
    let full = vec![7; 2 * 1024 * 1024];
    let target = decode(&fixture["target"]);
    let base = decode(&fixture["base"]);
    let tag = format!("v{VERSION}");
    let mut asset_values = Vec::new();
    for (name, bytes) in [
        (
            catalog["fallback"]["name"].as_str().unwrap().to_owned(),
            full,
        ),
        (
            format!("{}.sig", catalog["fallback"]["name"].as_str().unwrap()),
            fixture["full_signature"]
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        ),
        (
            catalog["target"]["name"].as_str().unwrap().to_owned(),
            target.clone(),
        ),
        (
            catalog["bases"][0]["patch"]["name"]
                .as_str()
                .unwrap()
                .to_owned(),
            patch,
        ),
        (
            format!(
                "{}.delta.json",
                catalog["fallback"]["name"].as_str().unwrap()
            ),
            catalog_bytes,
        ),
        (
            format!(
                "{}.delta.json.sig",
                catalog["fallback"]["name"].as_str().unwrap()
            ),
            catalog_fixture["signature"]
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        ),
        (
            format!("{}.sig", catalog["target"]["name"].as_str().unwrap()),
            fixture["target_signature"]
                .as_str()
                .unwrap()
                .as_bytes()
                .to_vec(),
        ),
    ] {
        let size = if name == catalog["target"]["name"].as_str().unwrap() {
            catalog["target"]["size"].as_u64().unwrap()
        } else {
            bytes.len() as u64
        };
        asset_values.push(serde_json::json!({"name":name,"browser_download_url":format!("https://github.com/OctopusGarage/mesh-talk/releases/download/{tag}/{name}"),"size":size,"digest":format!("sha256:{:x}",Sha256::digest(&bytes)),"state":"uploaded"}));
        server.routes.lock().unwrap().insert(
            format!("/OctopusGarage/mesh-talk/releases/download/{tag}/{name}"),
            Response {
                body: bytes,
                pause_body: None,
            },
        );
    }
    let mut release: Release = serde_json::from_value(
        serde_json::json!({"tag_name":tag,"draft":false,"prerelease":true,"assets":asset_values}),
    )
    .unwrap();
    if mode == "bad_target_signature" {
        let signature_name = format!("{}.sig", catalog["target"]["name"].as_str().unwrap());
        let asset = release
            .assets
            .iter_mut()
            .find(|asset| asset.name == signature_name)
            .unwrap();
        let invalid = super::tests::FIXTURE_SIGNATURE.as_bytes().to_vec();
        asset.size = invalid.len() as u64;
        asset.digest = Some(format!("sha256:{:x}", Sha256::digest(&invalid)));
        server
            .routes
            .lock()
            .unwrap()
            .get_mut(&format!(
                "/OctopusGarage/mesh-talk/releases/download/{tag}/{signature_name}"
            ))
            .unwrap()
            .body = invalid;
    }
    if mode == "patch_failure" {
        let patch_name = catalog["bases"][0]["patch"]["name"].as_str().unwrap();
        server.routes.lock().unwrap().remove(&format!(
            "/OctopusGarage/mesh-talk/releases/download/{tag}/{patch_name}"
        ));
    }
    if mode == "missing_catalog" {
        release
            .assets
            .retain(|asset| !asset.name.ends_with(".delta.json"));
    }
    if mode == "missing_target" {
        release
            .assets
            .retain(|asset| asset.name != catalog["target"]["name"].as_str().unwrap());
    }
    if matches!(
        mode,
        "catalog_network" | "catalog_signature_network" | "target_signature_network"
    ) {
        let name = match mode {
            "catalog_network" => format!(
                "{}.delta.json",
                catalog["fallback"]["name"].as_str().unwrap()
            ),
            "catalog_signature_network" => format!(
                "{}.delta.json.sig",
                catalog["fallback"]["name"].as_str().unwrap()
            ),
            _ => format!("{}.sig", catalog["target"]["name"].as_str().unwrap()),
        };
        server.routes.lock().unwrap().remove(&format!(
            "/OctopusGarage/mesh-talk/releases/download/{tag}/{name}"
        ));
    }
    if mode == "bad_catalog_digest" {
        let name = format!(
            "{}.delta.json",
            catalog["fallback"]["name"].as_str().unwrap()
        );
        let mut routes = server.routes.lock().unwrap();
        let response = routes
            .get_mut(&format!(
                "/OctopusGarage/mesh-talk/releases/download/{tag}/{name}"
            ))
            .unwrap();
        response.body[0] ^= 1;
    }
    let canonical =
        selected_asset(&release, catalog["fallback"]["name"].as_str().unwrap()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("base-executable");
    std::fs::write(
        &executable,
        if mode == "wrong_base" {
            b"unrecognized executable".to_vec()
        } else {
            base
        },
    )
    .unwrap();
    let (_sender, cancel) = tokio::sync::watch::channel(false);
    if mode == "canceled" {
        _sender.send(true).unwrap();
    }
    let key = if mode == "bad_catalog_signature" {
        super::tests::FIXTURE_KEY
    } else {
        fixture["public_key"].as_str().unwrap()
    };
    let mut host = server.host(app.app.handle(), key);
    if full_flow {
        host.base_source = delta::BaseSource {
            tag: "v99.0.0-nightly.41.1",
            executable: &executable,
        };
        if mode == "low_disk" {
            host.available_space = |_| Ok(0);
        }
        let metadata = serde_json::json!({"tag_name":tag,"body":"delta HTTPS fixture","draft":false,"prerelease":true,
            "assets":release.assets.iter().map(|asset|serde_json::json!({"name":asset.name,"browser_download_url":asset.browser_download_url,"size":asset.size,"digest":asset.digest,"state":asset.state})).collect::<Vec<_>>()});
        server.routes.lock().unwrap().insert(
            format!("/updater-test/releases/tags/{tag}"),
            Response {
                body: serde_json::to_vec(&metadata).unwrap(),
                pause_body: None,
            },
        );
        let phase_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let callback_mode = mode.to_owned();
        let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_progress = observed.clone();
        let on_progress = Channel::new(move |message| {
            let tauri::ipc::InvokeResponseBody::Json(json) = message else {
                panic!("JSON progress")
            };
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            if value["phase"] == "preparing"
                && phase_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1
                && callback_mode == "cancel_reconstruct"
            {
                crate::updates::cancel_update_download();
            }
            observed_progress.lock().unwrap().push(value);
            Ok(())
        });
        if mode == "cancel_patch" {
            let patch_name = catalog["bases"][0]["patch"]["name"].as_str().unwrap();
            server
                .routes
                .lock()
                .unwrap()
                .get_mut(&format!(
                    "/OctopusGarage/mesh-talk/releases/download/{tag}/{patch_name}"
                ))
                .unwrap()
                .pause_body = Some(Arc::new(tokio::sync::Notify::new()));
        }
        let prepare = prepare_from(
            host,
            VERSION.into(),
            Some(UpdateChannel::Nightly),
            on_progress,
        );
        let result = if mode == "cancel_patch" {
            let cancel_in_flight = async {
                loop {
                    if server
                        .requests
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|path| path.ends_with(".zst"))
                    {
                        crate::updates::cancel_update_download();
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::join!(prepare, cancel_in_flight).0
            })
            .await
            .unwrap()
        } else {
            prepare.await
        };
        let requests = server.requests.lock().unwrap().clone();
        let full_requests = requests
            .iter()
            .filter(|path| path.ends_with(canonical.name.as_str()))
            .count();
        if mode.starts_with("cancel_") || mode.starts_with("bad_") || mode == "catalog_identity" {
            assert!(result.is_err(), "must stop without fallback: {mode}");
            assert_eq!(full_requests, 0);
            assert!(STAGED.get().unwrap().lock().await.is_none());
        } else {
            let download = result.unwrap();
            assert!(download.installable);
            let is_delta = mode == "delta";
            assert_eq!(
                full_requests,
                usize::from(!is_delta),
                "canonical full fetched exactly once on fallback"
            );
            let installed_bytes = std::fs::read(&download.path).unwrap();
            assert_eq!(
                installed_bytes,
                if is_delta {
                    target.clone()
                } else {
                    vec![7; 2 * 1024 * 1024]
                }
            );
            assert_eq!(
                download.file_name,
                if is_delta {
                    catalog["target"]["name"].as_str().unwrap()
                } else {
                    canonical.name.as_str()
                }
            );
            if mode == "low_disk" {
                assert!(!requests.iter().any(|path| path.ends_with(".zst")));
            }
            if !is_delta && mode != "missing_catalog" {
                assert!(
                    observed
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|value| value["phase"] == "fallback" && value["downloaded"] == 0)
                );
            }
        }
        registration_retired();
        clear_stage().await;
        return;
    }
    let result = delta::prepare(
        &host,
        &release,
        canonical,
        directory.path(),
        &delta::BaseSource {
            tag: "v99.0.0-nightly.41.1",
            executable: &executable,
        },
        &Channel::new(|_| Ok(())),
        cancel,
    )
    .await;
    if matches!(
        mode,
        "bad_catalog_signature" | "bad_target_signature" | "canceled"
    ) {
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("must fail closed: {mode}"),
        };
        if mode == "canceled" {
            assert_eq!(error, "Download canceled");
            assert!(server.requests.lock().unwrap().is_empty());
        }
        assert!(!directory.path().join("payload").exists());
        return;
    }
    let result = result.unwrap();
    if matches!(mode, "wrong_base" | "patch_failure") {
        assert!(matches!(result, delta::Preparation::Fallback));
        assert!(!directory.path().join("payload").exists());
        assert!(
            !server
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|path| path.ends_with(canonical.name.as_str()))
        );
        return;
    }
    assert!(
        matches!(result, delta::Preparation::Ready(_, _)),
        "expected actual delta preparation"
    );
    assert_eq!(
        std::fs::read(directory.path().join("payload")).unwrap(),
        target
    );
    let requests = server.requests.lock().unwrap();
    assert!(
        !requests
            .iter()
            .any(|path| path.ends_with(canonical.name.as_str()))
    );
    assert!(
        !requests
            .iter()
            .any(|path| path.ends_with(catalog["target"]["name"].as_str().unwrap()))
    );
    assert_eq!(
        requests.len(),
        4,
        "catalog, catalog signature, patch, target signature only"
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_reconstructs_native_bytes_without_downloading_full_or_raw_target() {
    exercise_signed_delta("delta").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_unrecognized_base_requests_full_fallback_without_partial_stage() {
    exercise_signed_delta("wrong_base").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_patch_network_failure_requests_full_fallback() {
    exercise_signed_delta("patch_failure").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_invalid_catalog_authentication_fails_closed() {
    exercise_signed_delta("bad_catalog_signature").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_invalid_native_signature_fails_closed() {
    exercise_signed_delta("bad_target_signature").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_user_cancellation_does_not_trigger_fallback() {
    exercise_signed_delta("canceled").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_stages_actual_patch_route() {
    exercise_signed_delta("full_delta").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_falls_back_once_for_unrecognized_base() {
    exercise_signed_delta("full_wrong_base").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_falls_back_once_after_patch_network_error() {
    exercise_signed_delta("full_patch_failure").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_low_disk_uses_full_without_patch_download() {
    exercise_signed_delta("full_low_disk").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_missing_catalog_preserves_full_path() {
    exercise_signed_delta("full_missing_catalog").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_missing_target_uses_full() {
    exercise_signed_delta("full_missing_target").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_cancel_during_patch_stops_without_full() {
    exercise_signed_delta("full_cancel_patch").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_cancel_at_reconstruction_stops_without_full() {
    exercise_signed_delta("full_cancel_reconstruct").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_catalog_auth_failure_never_falls_back() {
    exercise_signed_delta("full_bad_catalog_signature").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_native_preparation_target_auth_failure_never_falls_back() {
    exercise_signed_delta("full_bad_target_signature").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_oversized_target_falls_back_to_canonical_full() {
    exercise_signed_delta("full_oversize_target").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_oversized_base_falls_back_to_canonical_full() {
    exercise_signed_delta("full_oversize_base").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_unsupported_codec_falls_back_to_canonical_full() {
    exercise_signed_delta("full_unsupported_codec").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_authenticated_identity_contradiction_never_falls_back() {
    exercise_signed_delta("full_catalog_identity").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_catalog_transport_failure_falls_back_once() {
    exercise_signed_delta("full_catalog_network").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_catalog_signature_transport_failure_falls_back_once() {
    exercise_signed_delta("full_catalog_signature_network").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_target_signature_transport_failure_falls_back_once() {
    exercise_signed_delta("full_target_signature_network").await;
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
#[tokio::test]
async fn signed_delta_catalog_integrity_failure_never_falls_back() {
    exercise_signed_delta("full_bad_catalog_digest").await;
}

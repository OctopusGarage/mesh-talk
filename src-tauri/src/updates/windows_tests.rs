//! Public, throwaway signing vectors exercise the real Windows launch-failure boundary.

use super::*;
use tauri::Manager;

const KEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDEzMjg0NTk4NjcxMDM2ODMKUldTRE5oQm5tRVVvRXdrcklZVXBuVGFiZ1I1S013UzVSU1hJSVdPTmFnUGI3UTBRWms4YVFZSEEK";
const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVTRE5oQm5tRVVvRXlPMnFJUG5nbWZPYjkwRS84dkNleE1UYit2R3V2NXNBbW8rQ0tFbHMvNm10OWFSMXRCRU93SmhEK1RUcERQWk16Q3RxU2wwakhDR29Jazg2UFVJS3d3PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkyODQ2NjQwCWZpbGU6bWFsZm9ybWVkLmV4ZQl2ZXJzaW9uOjk5LjAuMC1uaWdodGx5LjQyLjEKV05WektodEJ5eE1ZTVdmVGwrNUsrbzQ1clBTVHBtclVsOUlSMXV2L0FsZGZmb3NFTHdlc01Za1JZdXNUWHpwOFNPanlxeGExZFUzVDZEYjVaK2RSQnc9PQo=";

struct RunningResource;
impl tauri::Resource for RunningResource {}

#[tauri::command]
fn launch_failure_probe() -> &'static str {
    "running"
}

#[test]
fn installer_launch_failure_preserves_resources_and_renderer_ipc() {
    let mut context = tauri::test::mock_context(tauri::test::noop_assets());
    context.config_mut().plugins.0.insert(
        "updater".into(),
        serde_json::json!({"pubkey": "", "requireSignedVersion": true}),
    );
    let app = tauri::test::mock_builder()
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![launch_failure_probe])
        .build(context)
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let resource = app.resources_table().add(RunningResource);
    let payload = ReleaseAsset {
        name: "fixture.nsis.exe".into(),
        browser_download_url: "https://example.invalid/fixture.nsis.exe".into(),
        digest: None,
        size: 64,
        state: "uploaded".into(),
    };
    let update = signed_update(
        app.handle(),
        "99.0.0-nightly.42.1",
        &payload,
        SIGNATURE,
        KEY,
    )
    .unwrap();
    let mut malformed = [0_u8; 64];
    malformed[..2].copy_from_slice(b"MZ");
    // Valid signing and executable magic reach ShellExecuteW; Windows rejects
    // the malformed PE without spawning an installer or exiting this process.
    update.verify(&malformed).unwrap();
    assert!(update.install(malformed).is_err());
    assert!(app.resources_table().has(resource));
    let response = tauri::test::get_ipc_response(
        &webview,
        tauri::webview::InvokeRequest {
            cmd: "launch_failure_probe".into(),
            callback: tauri::ipc::CallbackFn(0),
            error: tauri::ipc::CallbackFn(1),
            url: "http://tauri.localhost".parse().unwrap(),
            body: tauri::ipc::InvokeBody::Json(serde_json::json!({})),
            headers: Default::default(),
            invoke_key: tauri::test::INVOKE_KEY.into(),
        },
    );
    assert_eq!(
        response
            .unwrap()
            .deserialize::<serde_json::Value>()
            .unwrap(),
        serde_json::json!("running")
    );
}

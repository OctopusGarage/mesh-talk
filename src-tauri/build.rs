fn main() {
    // Tauri warns whenever this legacy variable is present, even on non-Windows
    // builds. Remove it before tauri-build and preserve its Windows override.
    println!("cargo:rerun-if-env-changed=STATIC_VCRUNTIME");
    let legacy_static_vc_runtime =
        std::env::var_os("STATIC_VCRUNTIME").map(|value| value != "false");
    if legacy_static_vc_runtime.is_some() {
        // SAFETY: This build script does not spawn threads before changing its own environment.
        unsafe { std::env::remove_var("STATIC_VCRUNTIME") };
    }

    // Expose the build target triple to the binary (cargo sets TARGET for build scripts
    // only) so the Diagnostics "Environment" section can show it via `option_env!("TARGET")`.
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=TARGET={target}");
    }
    println!("cargo:rerun-if-env-changed=MESH_TALK_VARIANT");
    let variant = if std::env::var("MESH_TALK_VARIANT").as_deref() == Ok("lite") {
        "lite"
    } else {
        "default"
    };
    println!("cargo:rustc-env=MESH_TALK_VARIANT={variant}");
    println!("cargo:rerun-if-env-changed=MESH_TALK_BUILD_TAG");
    let base_tag = format!("v{}", std::env::var("CARGO_PKG_VERSION").unwrap());
    let build_tag =
        std::env::var("MESH_TALK_BUILD_TAG").unwrap_or_else(|_| format!("{base_tag}-dev"));
    let valid_nightly = build_tag
        .strip_prefix(&format!("{base_tag}-nightly."))
        .is_some_and(|suffix| {
            let numbers: Vec<_> = suffix.split('.').collect();
            numbers.len() == 2
                && numbers.iter().all(|value| {
                    value
                        .parse::<u64>()
                        .is_ok_and(|number| number > 0 && number.to_string() == *value)
                })
        });
    // Unlabelled local builds are development builds. Only the release workflow
    // supplies a stable or Nightly identity; native package versions stay numeric.
    assert!(
        build_tag == base_tag || build_tag == format!("{base_tag}-dev") || valid_nightly,
        "invalid published build identity"
    );
    println!("cargo:rustc-env=MESH_TALK_BUILD_TAG={build_tag}");
    println!(
        "cargo:rustc-env=MESH_TALK_BUILD_VERSION={}",
        &build_tag[1..]
    );
    let windows_msvc = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if windows_msvc {
        // Tauri normally embeds its Common Controls v6 manifest only in binaries.
        // Mock-runtime IPC tests also link native menu APIs (TaskDialogIndirect),
        // so library test executables need the same activation-context dependency.
        // Let the linker embed it for every executable; remove only the RC manifest
        // to avoid duplicate resources while preserving application icons/version.
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rerun-if-changed=windows.manifest");
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows.manifest");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        let mut windows_attributes = tauri_build::WindowsAttributes::new_without_app_manifest();
        if let Some(static_vc_runtime) = legacy_static_vc_runtime {
            windows_attributes = windows_attributes.static_vc_runtime(static_vc_runtime);
        }
        let attributes = tauri_build::Attributes::new().windows_attributes(windows_attributes);
        tauri_build::try_build(attributes).expect("failed to build Windows application resources");
    } else {
        tauri_build::build();
    }
}

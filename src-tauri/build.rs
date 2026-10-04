fn main() {
    // Expose the build target triple to the binary (cargo sets TARGET for build scripts
    // only) so the Diagnostics "Environment" section can show it via `option_env!("TARGET")`.
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=TARGET={target}");
    }
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
        let attributes = tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest());
        tauri_build::try_build(attributes).expect("failed to build Windows application resources");
    } else {
        tauri_build::build();
    }
}

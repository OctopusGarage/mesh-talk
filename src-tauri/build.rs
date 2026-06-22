fn main() {
    // Expose the build target triple to the binary (cargo sets TARGET for build scripts
    // only) so the Diagnostics "Environment" section can show it via `option_env!("TARGET")`.
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=TARGET={target}");
    }
    tauri_build::build()
}

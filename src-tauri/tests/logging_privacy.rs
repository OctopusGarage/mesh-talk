//! Guard the startup logging boundary without requiring a Tauri window. Public
//! test device IDs and record counts remain available in their diagnostic paths.

#[test]
fn node_startup_log_does_not_include_the_account_identifier() {
    let source = include_str!("../src/commands.rs");
    let diagnostic = source
        .lines()
        .find(|line| line.contains("log::info!") && line.contains("Node started"))
        .expect("node startup still has a diagnostic");
    assert_eq!(diagnostic.trim(), "log::info!(\"Node started\");");
    assert!(
        !source.contains("log::warn!(\"Node failed to start: {e}\")"),
        "startup errors must not bypass account-path redaction"
    );
}

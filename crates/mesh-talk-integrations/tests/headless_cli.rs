//! Exercise the real headless process, credential lifecycle, and durable CLI sends.
#![cfg(unix)]

use mesh_talk_integrations::{client::LocalClient, private_files::write_private_json};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    time::timeout,
};

async fn start(root: &Path, password: &Path, config: &Path) -> (Child, Value) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mesh-talk-integrations"))
        .arg("serve")
        .arg("--data-dir")
        .arg(root)
        .arg("--password-file")
        .arg(password)
        .arg("--admin-config")
        .arg(config)
        .args(["--discovery-port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let line = timeout(
        Duration::from_secs(60),
        BufReader::new(output).lines().next_line(),
    )
    .await
    .expect("headless host did not become ready")
    .unwrap()
    .expect("headless host exited before readiness");
    let ready: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(ready["status"], "ready");
    (child, ready)
}

async fn cli(config: &Path, command: &str, arguments: &[&str]) -> Value {
    let output = timeout(
        Duration::from_secs(30),
        Command::new(env!("CARGO_BIN_EXE_mesh-talk-integrations"))
            .arg(command)
            .arg("--config")
            .arg(config)
            .args(arguments)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("CLI operation timed out")
    .unwrap();
    assert!(
        output.status.success(),
        "{command} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

async fn stop(child: &mut Child) {
    let pid = child.id().expect("host exited before shutdown");
    // SAFETY: this is the live child owned by this test; SIGTERM is its shutdown API.
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) }, 0);
    let status = timeout(Duration::from_secs(30), child.wait())
        .await
        .expect("headless host did not stop")
        .unwrap();
    assert!(status.success(), "headless host failed during shutdown");
}

#[tokio::test]
async fn headless_cli_sends_once_revokes_credentials_and_restarts_cleanly() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("bot");
    let password = directory.path().join("password.json");
    let admin = directory.path().join("admin.json");
    let producer = directory.path().join("producer.json");
    write_private_json(&password, &json!({"password":"test-password"})).unwrap();
    let (mut child, ready) = start(&root, &password, &admin).await;
    let account = ready["account_id"].as_str().unwrap();
    let destination = format!("account:{account}");
    cli(&admin, "admin-context", &[]).await;
    cli(
        &admin,
        "grant-create",
        &[
            "--client-id",
            "cli-smoke",
            "--destination",
            &destination,
            "--describe",
            "--out",
            producer.to_str().unwrap(),
        ],
    )
    .await;
    let context = cli(&producer, "context", &[]).await;
    assert_eq!(context["read_access"], false);
    cli(&producer, "list-conversations", &[]).await;
    let arguments = [
        "--destination",
        destination.as_str(),
        "--text",
        "Saved by the headless CLI.",
        "--idempotency-key",
        "cli-smoke-message",
    ];
    let accepted = cli(&producer, "send", &arguments).await;
    let retried = cli(&producer, "send", &arguments).await;
    let accepted_id = accepted["message"]["id"].as_str().unwrap();
    assert_eq!(accepted["acceptance"], "accepted");
    assert_eq!(accepted["duplicate"], false);
    assert_eq!(retried["message"]["id"], accepted_id);
    assert_eq!(retried["duplicate"], true);
    let receipt = cli(
        &producer,
        "status",
        &["--idempotency-key", "cli-smoke-message"],
    )
    .await;
    assert_eq!(receipt["message_id"], accepted_id);
    cli(&admin, "audit-list", &[]).await;
    let grants = cli(&admin, "grant-list", &[]).await;
    let id = grants["grants"][0]["id"].as_str().unwrap();
    cli(&admin, "grant-revoke", &["--grant-id", id]).await;
    assert!(
        LocalClient::from_file(&producer)
            .unwrap()
            .call("context", json!({}))
            .await
            .is_err()
    );
    stop(&mut child).await;
    assert!(!admin.exists());
    assert!(!root.join("accounts/bot/integration-endpoint.json").exists());

    let (mut child, restarted) = start(&root, &password, &admin).await;
    assert_eq!(restarted["account_id"], ready["account_id"]);
    assert!(
        LocalClient::from_file(&producer)
            .unwrap()
            .call("context", json!({}))
            .await
            .is_err()
    );
    cli(&admin, "admin-context", &[]).await;
    stop(&mut child).await;
    assert!(!admin.exists());
}

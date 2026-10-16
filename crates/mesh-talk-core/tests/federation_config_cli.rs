//! Operator workflow uses public identity pins only and never opens a keystore.
use std::process::{Command, Output};

fn run(file: &std::path::Path, operation: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
        .arg("federation")
        .arg("--file")
        .arg(file)
        .args(operation)
        .output()
        .unwrap()
}

#[test]
fn common_config_enrollment_listing_and_whole_identity_revocation() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relays.conf");
    let initialized = run(&path, &["init"]);
    assert!(
        initialized.status.success(),
        "configuration init requires no password or keystore: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let id = "a".repeat(32);
    let second = "b".repeat(32);
    for seed in [
        format!("{id}@192.168.1.2:47474"),
        format!("{id}@100.64.0.2:47474"),
        format!("{second}@[fd7a:115c:a1e0::2]:47474"),
    ] {
        assert!(run(&path, &["enroll", &seed]).status.success());
    }
    let listed = run(&path, &["list"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout).matches(&id).count(),
        2
    );
    let unchanged = std::fs::read(&path).unwrap();
    assert!(
        !run(&path, &["enroll", "short@127.0.0.1:0"])
            .status
            .success()
    );
    assert!(
        !run(&path, &["init"]).status.success(),
        "init must not overwrite existing trust"
    );
    assert_eq!(std::fs::read(&path).unwrap(), unchanged);
    assert!(run(&path, &["revoke", &id.to_uppercase()]).status.success());
    let listed = run(&path, &["list"]);
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !text.contains(&id),
        "revocation removes every route for that identity"
    );
    assert!(text.contains(&second));
    assert!(String::from_utf8_lossy(&run(&path, &["revoke", &second]).stderr).contains("restart"));
}

#[test]
fn manifests_validate_before_any_keystore_or_password_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relays.conf");
    let keystore = temp.path().join("missing").join("identity");
    std::fs::write(&path, "mesh-talk-federation-v2\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
        .args(["--post-office", "--bridge", "--name", "relay", "--keystore"])
        .arg(&keystore)
        .arg("--password-file")
        .arg(temp.path().join("absent"))
        .arg("--federation-config")
        .arg(&path)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported federation manifest"));
    assert!(!keystore.parent().unwrap().exists());
}

#[test]
fn manifests_are_bounded_normalized_and_preserve_alternate_endpoints() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relays.conf");
    let id = "ab".repeat(16);
    std::fs::write(&path, format!("# public pins\n\nmesh-talk-federation-v1\n{}@127.0.0.1:47474\n{id}@127.0.0.1:47474\n{id}@127.0.0.2:47474\n", id.to_uppercase())).unwrap();
    let listed = run(&path, &["list"]);
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout).matches(&id).count(),
        2
    );
    for invalid in [
        "mesh-talk-federation-v2\n".to_string(),
        "\n# no header\n".to_string(),
        format!("mesh-talk-federation-v1\n{id}@localhost:47474\n"),
        format!("mesh-talk-federation-v1\n{id}@127.0.0.1:0\n"),
        "#".repeat(65537),
        format!(
            "mesh-talk-federation-v1\n{}",
            format!("{id}@127.0.0.1:47474\n").repeat(257)
        ),
    ] {
        std::fs::write(&path, &invalid).unwrap();
        assert!(
            !run(&path, &["list"]).status.success(),
            "accepted invalid manifest"
        );
        assert!(
            !run(&path, &["enroll", &format!("{id}@127.0.0.3:47474")])
                .status
                .success()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), invalid);
    }
}

#[test]
fn manifest_requires_bridge_and_conflicts_with_inline_peers() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relays.conf");
    for extras in [
        vec![],
        vec!["--post-office"],
        vec![
            "--post-office",
            "--bridge",
            "--federation-peer",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:47474",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
            .args([
                "--name",
                "relay",
                "--keystore",
                "unused",
                "--password",
                "unused",
            ])
            .args(extras)
            .arg("--federation-config")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(
            !String::from_utf8_lossy(&output.stderr)
                .contains("unexpected argument '--federation-config'")
        );
    }
}

#[test]
fn mutation_lock_prevents_lost_updates_and_readers_keep_atomic_snapshot_access() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("relays.conf");
    assert!(run(&path, &["init"]).status.success());
    let id = "a".repeat(32);
    assert!(
        run(&path, &["enroll", &format!("{id}@127.0.0.1:47474")])
            .status
            .success()
    );
    let before = std::fs::read(&path).unwrap();
    let lock_path = temp.path().join("relays.conf.lock");
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.try_lock().unwrap();
    let alias = temp.path().join(".").join("relays.conf");
    for file in [&path, &alias] {
        for operation in [
            vec!["enroll", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb@127.0.0.2:47474"],
            vec!["revoke", &id],
            vec!["init"],
        ] {
            let result = run(file, &operation);
            assert!(
                !result.status.success(),
                "mutation must fail while another writer holds the companion lock"
            );
            let error = String::from_utf8_lossy(&result.stderr);
            assert!(
                error.contains("retry"),
                "clear contention guidance: {error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        assert!(
            run(file, &["list"]).status.success(),
            "readers never take mutation lock"
        );
    }
    #[cfg(unix)]
    {
        let alias_dir = temp.path().join("alias");
        std::os::unix::fs::symlink(temp.path(), &alias_dir).unwrap();
        let result = run(&alias_dir.join("relays.conf"), &["revoke", &id]);
        assert!(
            !result.status.success(),
            "canonical parent aliases share the same lock"
        );
        assert!(String::from_utf8_lossy(&result.stderr).contains("retry"));
    }
    drop(lock);
    assert!(lock_path.exists(), "lock inode must persist after release");
    assert!(run(&path, &["revoke", &id]).status.success());
    assert!(
        run(
            &path,
            &["enroll", "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb@127.0.0.2:47474"]
        )
        .status
        .success()
    );
    let listed = run(&path, &["list"]);
    assert!(!String::from_utf8_lossy(&listed.stdout).contains(&id));
    assert!(String::from_utf8_lossy(&listed.stdout).contains("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"));
}

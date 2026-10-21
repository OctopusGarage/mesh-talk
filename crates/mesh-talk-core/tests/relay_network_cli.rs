//! Offline network administration never shares an enrollment private key with a relay.
use mesh_talk_core::identity::device::DeviceIdentity;
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
        .arg("network")
        .args(args)
        .output()
        .unwrap()
}
fn path(path: &Path) -> &str {
    path.to_str().unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn offline_enrollment_inspection_and_revocation_work_without_relay_passwords() {
    let temp = tempfile::tempdir().unwrap();
    let key = temp.path().join("authority.key");
    let policy = temp.path().join("network.policy");
    let certificate = temp.path().join("relay.membership");
    let relay = DeviceIdentity::generate().user_id();
    success(run(&[
        "init",
        "--authority-key",
        path(&key),
        "--policy",
        path(&policy),
    ]));
    let before = std::fs::read(&policy).unwrap();
    assert!(
        !run(&[
            "init",
            "--authority-key",
            path(&key),
            "--policy",
            path(&policy)
        ])
        .status
        .success()
    );
    assert_eq!(std::fs::read(&policy).unwrap(), before);
    success(run(&[
        "enroll",
        "--authority-key",
        path(&key),
        "--policy",
        path(&policy),
        "--device",
        &relay,
        "--membership",
        path(&certificate),
        "--valid-days",
        "7",
    ]));
    let status = success(run(&[
        "inspect",
        "--policy",
        path(&policy),
        "--membership",
        path(&certificate),
    ]));
    assert!(status.contains(&relay));
    assert!(status.contains("not attested"));
    success(run(&[
        "revoke",
        "--policy",
        path(&policy),
        "--device",
        &relay,
    ]));
    assert!(
        !run(&[
            "inspect",
            "--policy",
            path(&policy),
            "--membership",
            path(&certificate)
        ])
        .status
        .success()
    );
    assert!(
        !run(&[
            "enroll",
            "--authority-key",
            path(&key),
            "--policy",
            path(&policy),
            "--device",
            &relay,
            "--membership",
            path(&temp.path().join("revoked"))
        ])
        .status
        .success()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn invalid_network_configuration_fails_before_password_or_identity_access() {
    let temp = tempfile::tempdir().unwrap();
    let policy = temp.path().join("bad.policy");
    std::fs::write(&policy, "invalid").unwrap();
    let missing = temp.path().join("missing");
    let result = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
        .args(["--post-office", "--name", "relay", "--keystore"])
        .arg(missing.join("identity"))
        .arg("--password-file")
        .arg(missing.join("password"))
        .arg("--relay-network")
        .arg(&policy)
        .arg("--relay-membership")
        .arg(missing.join("membership"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("network policy"));
    assert!(!missing.exists());
}

#[test]
fn startup_binds_membership_to_existing_identity_before_creating_relay_state() {
    use mesh_talk_core::postoffice::network::{NetworkMembership, NetworkPolicy, unix_now};
    let temp = tempfile::tempdir().unwrap();
    let identity_path = temp.path().join("identity.keystore");
    mesh_talk_core::identity::keystore::load_or_create(&identity_path, "pw").unwrap();
    let authority = DeviceIdentity::generate();
    let wrong_device = DeviceIdentity::generate();
    let policy = temp.path().join("network.policy");
    let membership = temp.path().join("relay.membership");
    std::fs::write(
        &policy,
        NetworkPolicy::new(authority.public().ed25519_pub)
            .encode()
            .unwrap(),
    )
    .unwrap();
    std::fs::write(
        &membership,
        NetworkMembership::issue(
            &authority,
            &wrong_device.user_id(),
            unix_now(),
            std::time::Duration::from_secs(3600),
        )
        .unwrap()
        .encode()
        .unwrap(),
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_mesh-talk-node"))
        .args([
            "--post-office",
            "--name",
            "relay",
            "--password",
            "pw",
            "--keystore",
        ])
        .arg(&identity_path)
        .arg("--relay-network")
        .arg(policy)
        .arg("--relay-membership")
        .arg(membership)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("membership does not match"));
    assert!(!temp.path().join("account.keystore").exists());
    assert!(!temp.path().join("relay.log").exists());
}

#[test]
fn policy_mutation_lock_serializes_revocation_and_enrollment() {
    let temp = tempfile::tempdir().unwrap();
    let key = temp.path().join("authority.key");
    let policy = temp.path().join("network.policy");
    let certificate = temp.path().join("relay.membership");
    success(run(&[
        "init",
        "--authority-key",
        path(&key),
        "--policy",
        path(&policy),
    ]));
    let before = std::fs::read(&policy).unwrap();
    let lock = std::fs::File::options()
        .read(true)
        .write(true)
        .open(temp.path().join("network.policy.lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let device = DeviceIdentity::generate().user_id();
    for args in [
        vec!["revoke", "--policy", path(&policy), "--device", &device],
        vec![
            "enroll",
            "--authority-key",
            path(&key),
            "--policy",
            path(&policy),
            "--device",
            &device,
            "--membership",
            path(&certificate),
        ],
    ] {
        let result = run(&args);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("retry"));
    }
    success(run(&["inspect", "--policy", path(&policy)]));
    assert_eq!(std::fs::read(&policy).unwrap(), before);
    assert!(!certificate.exists());
}

//! Explicit growth and real previous-binary acceptance; excluded from ordinary gates.
use super::{
    ProfileStorageLease,
    context::ProfileStorage,
    publication::tests::{DECODES, HASHES, MAX_BATCH, MAX_EDGES, MAX_LIVE},
};
use crate::{
    eventlog::{ConversationId, Event, EventKind},
    identity::device::DeviceIdentity,
    storage::record_log::EncryptedRecordLog,
};

fn growth(rows: u64, payload_bytes: usize) {
    let fixture_started = std::time::Instant::now();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("relay.log");
    let identity = DeviceIdentity::generate();
    let (mut writer, _) = EncryptedRecordLog::<Event>::open(&path, "pw", b"MTLOG1").unwrap();
    for seq in 0..rows {
        writer
            .append(&Event::new(
                &identity,
                ConversationId::new([1; 32]),
                seq,
                vec![],
                seq,
                seq,
                EventKind::Message,
                vec![0; payload_bytes],
            ))
            .unwrap();
    }
    writer.sync().unwrap();
    drop(writer);
    let fixture_elapsed = fixture_started.elapsed();
    let corpus_bytes = std::fs::metadata(&path).unwrap().len();
    crate::storage::record_log::frame_observations::reset();
    super::publication::tests::start_timing();
    MAX_LIVE.with(|v| v.set(0));
    MAX_BATCH.with(|v| v.set(0));
    MAX_EDGES.with(|v| v.set(0));
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &path,
        "pw",
        123,
    )
    .unwrap();
    let phases = super::publication::tests::finish_timing();
    eprintln!(
        "rows={rows}; ciphertext_bytes_per_event={payload_bytes}; corpus_bytes={corpus_bytes}; fixture_ms={}; phase_elapsed_ms={phases:?}; maximum_legacy_frame_capacity={}",
        fixture_elapsed.as_millis(),
        crate::storage::record_log::frame_observations::maximum_capacity()
    );
    assert!(crate::storage::record_log::frame_observations::maximum_capacity() >= payload_bytes);
    assert!(
        crate::storage::record_log::frame_observations::maximum_capacity()
            <= payload_bytes * 2 + 1024
    );
    assert_eq!(MAX_LIVE.with(|v| v.get()), 1);
    assert_eq!(MAX_BATCH.with(|v| v.get()), 1);
    assert!(MAX_EDGES.with(|v| v.get()) <= 128);
    assert!(!profile.cleanup_pending().unwrap());
    drop(profile);
    HASHES.with(|v| v.set(0));
    DECODES.with(|v| v.set(0));
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &path,
        "pw",
        123,
    )
    .unwrap();
    assert_eq!(HASHES.with(|v| v.get()), 0);
    assert_eq!(DECODES.with(|v| v.get()), 0);
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the output on success; no pointer escapes.
        assert_eq!(
            unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
            0
        );
        // SAFETY: successful getrusage initialized every field we inspect.
        let usage = unsafe { usage.assume_init() };
        #[cfg(target_os = "macos")]
        let peak_bytes = usage.ru_maxrss as u64;
        #[cfg(not(target_os = "macos"))]
        let peak_bytes = usage.ru_maxrss as u64 * 1024;
        eprintln!(
            "whole_test_process_peak_rss_bytes={peak_bytes}; includes fixture generation and exceptional migration; not a Node/startup-only bound"
        );
    }
    eprintln!(
        "rows={rows}; max_live_decoded_events=1; max_commit_batch=1; child_page_limit=128; normal_reopen_legacy_hashes=0; normal_reopen_legacy_decodes=0"
    );
    drop(profile);
}
#[test]
#[ignore = "explicit 10k encrypted legacy migration growth acceptance"]
fn migration_growth_10k() {
    growth(10_000, 32)
}
#[test]
#[ignore = "explicit 100k encrypted legacy migration growth acceptance"]
fn migration_growth_100k() {
    growth(100_000, 32)
}

#[test]
#[ignore = "explicit 192MiB encrypted legacy payload growth acceptance"]
fn migration_growth_large_bodies() {
    growth(4096, 48 * 1024)
}

#[test]
#[ignore = "requires verified previous production binary; derives production KDF fixture"]
fn previous_production_binary_refuses_published_barrier_before_serving() {
    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit},
    };
    use sha2::{Digest, Sha256};
    use std::{
        io::Read,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    use zeroize::Zeroizing;
    let binary = std::env::var_os("MESH_LEGACY_RELAY_BINARY")
        .expect("set verified previous production CLI path");
    let expected =
        std::env::var("MESH_LEGACY_RELAY_SHA256").expect("set verified previous binary SHA256");
    let mut file = std::fs::File::open(&binary).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).unwrap();
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    assert_eq!(hex::encode(digest.finalize()), expected);
    let root = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let password = "isolated-publication-fixture";
    let salt = crate::storage::encryption::generate_salt();
    let nonce = crate::storage::encryption::generate_nonce().unwrap();
    let mut key = Zeroizing::new([0u8; 32]);
    pbkdf2::pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, 600_000, key.as_mut());
    let (ed, x) = identity.secret_bytes();
    let plaintext = Zeroizing::new(bincode::serialize(&(ed, x)).unwrap());
    let ciphertext = Aes256Gcm::new_from_slice(key.as_ref())
        .unwrap()
        .encrypt(
            &Nonce::try_from(nonce.as_slice()).unwrap(),
            plaintext.as_slice(),
        )
        .unwrap();
    let mut keystore = salt.to_vec();
    keystore.extend_from_slice(&nonce);
    keystore.extend_from_slice(&ciphertext);
    let keypath = root.path().join("identity.keystore");
    std::fs::write(&keypath, keystore).unwrap();
    let pwpath = root.path().join("password");
    std::fs::write(&pwpath, password).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pwpath, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let path = root.path().join("relay.log");
    let profile = ProfileStorage::open_relay(
        ProfileStorageLease::acquire(root.path()).unwrap(),
        &identity.public(),
        &path,
        password,
        123,
    )
    .unwrap();
    drop(profile);
    let barrier = std::fs::read(&path).unwrap();
    let db = std::fs::read(root.path().join("profile.db")).unwrap();
    let mut child = Command::new(binary)
        .args(["--keystore"])
        .arg(&keypath)
        .arg("--password-file")
        .arg(&pwpath)
        .args([
            "--name",
            "storage-publication-fixture",
            "--post-office",
            "--port",
            "0",
            "--discovery-port",
            "0",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("previous binary did not fail before serving");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("bad magic"),
        "expected specifically the migration barrier, received: {text}"
    );
    assert_eq!(std::fs::read(path).unwrap(), barrier);
    assert_eq!(std::fs::read(root.path().join("profile.db")).unwrap(), db);
}

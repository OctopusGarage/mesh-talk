thread_local! {
    static DECODED_ACTIVE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static DECODED_PEAK: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Serialize)]
struct TrackedRecord {
    n: u64,
    data: Vec<u8>,
    #[serde(skip)]
    decoded: bool,
}

impl<'de> Deserialize<'de> for TrackedRecord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = Rec::deserialize(deserializer)?;
        DECODED_ACTIVE.with(|active| {
            active.set(active.get() + 1);
            DECODED_PEAK.with(|peak| peak.set(peak.get().max(active.get())));
        });
        Ok(Self {
            n: record.n,
            data: record.data,
            decoded: true,
        })
    }
}

impl Drop for TrackedRecord {
    fn drop(&mut self) {
        if self.decoded {
            DECODED_ACTIVE.with(|active| active.set(active.get() - 1));
        }
    }
}

#[test]
fn visitor_retains_only_one_decoded_record() {
    DECODED_ACTIVE.with(|active| active.set(0));
    DECODED_PEAK.with(|peak| peak.set(0));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tracked.log");
    let (mut log, _) = EncryptedRecordLog::<TrackedRecord>::open(&path, "pw", TEST_MAGIC).unwrap();
    for n in 0..128 {
        log.append(&TrackedRecord {
            n,
            data: vec![n as u8; 4096],
            decoded: false,
        })
        .unwrap();
    }
    drop(log);
    let mut visits = 0;
    let _log =
        EncryptedRecordLog::<TrackedRecord>::open_with_visitor(&path, "pw", TEST_MAGIC, |record| {
            assert_eq!(record.n, visits);
            assert_eq!(record.data, vec![visits as u8; 4096]);
            visits += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(visits, 128);
    assert_eq!(DECODED_ACTIVE.with(|active| active.get()), 0);
    assert_eq!(DECODED_PEAK.with(|peak| peak.get()), 1);
}

#[test]
fn visitor_error_preserves_source_including_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("error.log");
    let expected = vec![rec(1, b"one"), rec(2, b"two"), rec(3, b"three")];
    let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    for record in &expected {
        log.append(record).unwrap();
    }
    drop(log);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&[0xAA, 0xBB])
        .unwrap();
    let source = std::fs::read(&path).unwrap();
    let mut visits = 0;
    let result = EncryptedRecordLog::<Rec>::open_with_visitor(&path, "pw", TEST_MAGIC, |_| {
        visits += 1;
        if visits == 2 {
            Err(LogError::Serialization("visitor stopped".into()))
        } else {
            Ok(())
        }
    });
    assert!(
        matches!(result, Err(LogError::Serialization(message)) if message == "visitor stopped")
    );
    assert_eq!(visits, 2);
    assert_eq!(std::fs::read(&path).unwrap(), source);
    let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    assert_eq!(records, expected);
}

#[test]
fn visitor_and_collector_agree_on_torn_final_frame() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("visitor.log");
    let copy = dir.path().join("collector.log");
    let expected = vec![rec(1, b"one"), rec(2, b"two")];
    let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    for record in &expected {
        log.append(record).unwrap();
    }
    let valid_len = std::fs::metadata(&path).unwrap().len();
    let torn = log.encode_record(&rec(3, b"unfinished")).unwrap();
    drop(log);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(&torn[..torn.len() - 1])
        .unwrap();
    std::fs::copy(&path, &copy).unwrap();
    let mut visited = Vec::new();
    let mut log = EncryptedRecordLog::<Rec>::open_with_visitor(&path, "pw", TEST_MAGIC, |record| {
        visited.push(record);
        Ok(())
    })
    .unwrap();
    let (_, collected) = EncryptedRecordLog::<Rec>::open(&copy, "pw", TEST_MAGIC).unwrap();
    assert_eq!(visited, expected);
    assert_eq!(visited, collected);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), valid_len);
    assert_eq!(std::fs::read(&path).unwrap(), std::fs::read(&copy).unwrap());
    log.append(&rec(4, b"after repair")).unwrap();
    drop(log);
    let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    assert_eq!(
        records,
        vec![rec(1, b"one"), rec(2, b"two"), rec(4, b"after repair")]
    );
}

#[test]
fn visitor_rejects_bad_authentication_or_decode_without_repair() {
    for bad_decode in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append(&rec(1, b"good")).unwrap();
        let bad_frame = if bad_decode {
            // Authenticated plaintext that cannot decode as Rec (missing its data field).
            let (ciphertext, nonce) = encrypt_data(&[0; 8], &log.key).unwrap();
            let mut frame = ((NONCE_SIZE + ciphertext.len()) as u32)
                .to_be_bytes()
                .to_vec();
            frame.extend_from_slice(&nonce);
            frame.extend_from_slice(&ciphertext);
            frame
        } else {
            let mut frame = log.encode_record(&rec(2, b"tampered")).unwrap();
            *frame.last_mut().unwrap() ^= 0xFF;
            frame
        };
        drop(log);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&bad_frame).unwrap();
        file.write_all(&[0xAA, 0xBB]).unwrap();
        drop(file);
        let source = std::fs::read(&path).unwrap();
        let mut visited = Vec::new();
        let result =
            EncryptedRecordLog::<Rec>::open_with_visitor(&path, "pw", TEST_MAGIC, |record| {
                visited.push(record);
                Ok(())
            });
        let expected_error = if bad_decode {
            "record decode:"
        } else {
            "record failed to decrypt"
        };
        assert!(
            matches!(result, Err(LogError::CorruptFile(message)) if message.starts_with(expected_error))
        );
        assert_eq!(visited, vec![rec(1, b"good")]);
        assert_eq!(std::fs::read(&path).unwrap(), source);
    }
}

#[test]
fn fresh_visitor_log_has_no_visits_and_writer_can_append() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh.log");
    let mut log = EncryptedRecordLog::<Rec>::open_with_visitor(&path, "pw", TEST_MAGIC, |_| {
        panic!("new log must not visit any records")
    })
    .unwrap();
    log.append(&rec(1, b"first")).unwrap();
    drop(log);
    let mut visits = 0;
    let _log = EncryptedRecordLog::<Rec>::open_with_visitor(&path, "pw", TEST_MAGIC, |record| {
        assert_eq!(record, rec(1, b"first"));
        visits += 1;
        Ok(())
    })
    .unwrap();
    assert_eq!(visits, 1);
}

#[test]
fn durability_confirmed_append_needs_no_repeated_sync() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<EncryptedRecordLog<Rec>>();
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) =
        EncryptedRecordLog::<Rec>::open(&dir.path().join("sync.log"), "pw", TEST_MAGIC).unwrap();
    log.append_durable(&rec(1, b"durable")).unwrap();
    sync_observations::reset();
    for _ in 0..3 {
        log.sync().unwrap();
    }
    assert_eq!(
        sync_observations::counts(),
        (0, 0),
        "an acknowledged durable prefix needs no further physical sync"
    );
}

#[test]
fn durability_nondurable_append_requires_one_sync_then_becomes_quiet() {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) =
        EncryptedRecordLog::<Rec>::open(&dir.path().join("sync.log"), "pw", TEST_MAGIC).unwrap();
    sync_observations::reset();
    log.append(&rec(1, b"pending")).unwrap();
    assert_eq!(sync_observations::counts(), (0, 0));
    log.sync().unwrap();
    assert_eq!(sync_observations::counts(), (1, 1));
    log.sync().unwrap();
    assert_eq!(sync_observations::counts(), (1, 1));
}

#[test]
fn durability_reopen_requires_fresh_prefix_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.log");
    let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    log.append(&rec(1, b"written without a durable acknowledgement"))
        .unwrap();
    drop(log);
    let (log, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    assert_eq!(
        records,
        vec![rec(1, b"written without a durable acknowledgement")]
    );
    sync_observations::reset();
    log.sync().unwrap();
    assert_eq!(
        sync_observations::counts(),
        (1, 1),
        "readable bytes do not prove this reopened prefix durable"
    );
    log.sync().unwrap();
    assert_eq!(sync_observations::counts(), (1, 1));
}

#[test]
fn durability_failed_file_or_parent_sync_is_retried() {
    for parent in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut log, _) =
            EncryptedRecordLog::<Rec>::open(&dir.path().join("sync.log"), "pw", TEST_MAGIC)
                .unwrap();
        log.append(&rec(1, b"pending")).unwrap();
        sync_observations::reset();
        if parent {
            sync_observations::fail_parent();
        } else {
            sync_observations::fail_file();
        }
        assert!(matches!(log.sync(), Err(LogError::Io(_))));
        assert_eq!(
            sync_observations::counts(),
            if parent { (1, 1) } else { (1, 0) }
        );
        log.sync().unwrap();
        let confirmed = if parent { (2, 2) } else { (2, 1) };
        assert_eq!(sync_observations::counts(), confirmed);
        log.sync().unwrap();
        assert_eq!(sync_observations::counts(), confirmed);
    }
}

#[test]
fn durability_failed_durable_append_rollback_cannot_restore_a_clean_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.log");
    let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    log.append_durable(&rec(1, b"accepted")).unwrap();
    sync_observations::reset();
    sync_observations::fail_parent();
    assert!(matches!(
        log.append_durable(&rec(2, b"rejected")),
        Err(LogError::Io(_))
    ));
    assert!(!log.poisoned);
    sync_observations::reset();
    log.sync().unwrap();
    assert_eq!(
        sync_observations::counts(),
        (1, 1),
        "rollback file sync alone cannot confirm parent durability"
    );
    log.sync().unwrap();
    assert_eq!(sync_observations::counts(), (1, 1));
    let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    assert_eq!(records, vec![rec(1, b"accepted")]);
}

#[test]
fn durability_poisoned_rollback_rejects_sync_without_io() {
    let dir = tempfile::tempdir().unwrap();
    let (mut log, _) =
        EncryptedRecordLog::<Rec>::open(&dir.path().join("sync.log"), "pw", TEST_MAGIC).unwrap();
    log.fail_next_append_rollback_for_test(7);
    assert!(log.append_durable(&rec(1, b"uncertain")).is_err());
    assert!(log.poisoned);
    sync_observations::reset();
    assert!(matches!(log.sync(), Err(LogError::CorruptFile(_))));
    assert_eq!(sync_observations::counts(), (0, 0));
}

#[test]
fn durability_committed_replacements_retry_failed_parent_sync() {
    for batch in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.log");
        let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        log.append_durable(&rec(1, b"old")).unwrap();
        sync_observations::reset();
        sync_observations::fail_parent();
        if batch {
            log.append_batch_atomic(&[rec(2, b"new")]).unwrap();
        } else {
            log.rewrite(&[rec(2, b"new")]).unwrap();
        }
        assert_eq!(sync_observations::counts(), (1, 1));
        let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
        let expected = if batch {
            vec![rec(1, b"old"), rec(2, b"new")]
        } else {
            vec![rec(2, b"new")]
        };
        assert_eq!(
            records, expected,
            "rename committed despite the failed directory sync"
        );
        sync_observations::reset();
        log.sync().unwrap();
        assert_eq!(sync_observations::counts(), (1, 1));
        log.sync().unwrap();
        assert_eq!(sync_observations::counts(), (1, 1));
    }
}

#[test]
fn durability_failed_rewrite_cannot_confirm_the_old_nondurable_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sync.log");
    let (mut log, _) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    log.append(&rec(1, b"old pending")).unwrap();
    let temporary = path.with_extension("compact-tmp");
    log.before_rewrite_rename = Some(Box::new(move || std::fs::remove_file(temporary).unwrap()));
    assert!(log.rewrite(&[rec(2, b"not committed")]).is_err());
    sync_observations::reset();
    log.sync().unwrap();
    assert_eq!(sync_observations::counts(), (1, 1));
    let (_, records) = EncryptedRecordLog::<Rec>::open(&path, "pw", TEST_MAGIC).unwrap();
    assert_eq!(records, vec![rec(1, b"old pending")]);
}

use super::*;
use crate::discovery::Roster;
use crate::identity::{account::Account, device::DeviceIdentity};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

fn open(dir: &Path, identity: DeviceIdentity, account: Account) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (channel, _) = mpsc::unbounded_channel();
    let (file, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        identity,
        account,
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

#[tokio::test]
async fn personal_file_is_durable_without_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let source = dir.path().join("source.txt");
    let bytes = b"personal attachment survives source deletion";
    std::fs::write(&source, bytes).unwrap();
    let node = open(dir.path(), identity, account);
    let (id, file) = node
        .enqueue_file_to_account(&target, &source, FileKind::File)
        .await
        .unwrap();
    std::fs::remove_file(&source).unwrap();
    let history = node.account_history(&target, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(history[0].from_me);
    assert!(history[0].file.is_some());
    assert!(node.delivery.lock().unwrap().message(id).is_none());
    assert_eq!(node.delivery_status(id), None);
    let output = dir.path().join("export.txt");
    node.save_file(file, &output).unwrap();
    assert_eq!(std::fs::read(&output).unwrap(), bytes);
    drop(node);
    let node = open(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    let history = node.account_history(&target, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert!(node.file_ready_to_save(file).unwrap());
    node.save_file(file, &dir.path().join("reopened.txt"))
        .unwrap();
    assert_eq!(
        std::fs::read(dir.path().join("reopened.txt")).unwrap(),
        bytes
    );
}

#[tokio::test]
async fn personal_file_projection_failure_recovers_once() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let source = dir.path().join("source");
    std::fs::write(&source, b"recovery").unwrap();
    let node = open(dir.path(), identity, account);
    node.received_files
        .lock()
        .unwrap()
        .fail_appends_for_test(true)
        .unwrap();
    let (id, file) = node
        .enqueue_file_to_account(&target, &source, FileKind::File)
        .await
        .unwrap();
    assert_eq!(
        node.delivery.lock().unwrap().pending_transactions().len(),
        1
    );
    node.received_files
        .lock()
        .unwrap()
        .fail_appends_for_test(false)
        .unwrap();
    drop(node);
    let node = open(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    node.recover_delivery(&mut node.delivery.lock().unwrap())
        .unwrap();
    let history = node.account_history(&target, 10).unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, id);
    assert_eq!(node.read_file(file).unwrap(), b"recovery");
}

#[tokio::test]
async fn personal_file_unaccepted_wal_leaves_no_history() {
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile");
    let identity = DeviceIdentity::generate();
    let secret = identity.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let source = dir.path().join("source");
    std::fs::write(&source, b"unaccepted").unwrap();
    let node = open(&profile, identity, account);
    let (manifest, _) = node.stage_file(&source, FileKind::File, |_| {}).unwrap();
    let host = super::super::conversation::account_conversation_id(&target, &target);
    let proof_path = profile.join("account-file-proofs.log");
    let before = std::fs::metadata(&proof_path).unwrap().len();
    // Use the record log's read-only append fault: Windows cannot rename a
    // directory containing the live log handles. Proof/chunk writes stay real.
    node.delivery
        .lock()
        .unwrap()
        .fail_journal_appends_for_test(true)
        .unwrap();
    let result = node.accept_staged_manifest_if(
        &[],
        host,
        Some(target.clone()),
        &manifest,
        None,
        &mut |accept| accept(),
    );
    node.delivery
        .lock()
        .unwrap()
        .fail_journal_appends_for_test(false)
        .unwrap();
    assert!(std::fs::metadata(&proof_path).unwrap().len() > before);
    assert!(result.is_err());
    assert!(
        node.delivery
            .lock()
            .unwrap()
            .pending_transactions()
            .is_empty()
    );
    assert!(node.account_history(&target, 10).unwrap().is_empty());
    drop(node);
    let node = open(
        &profile,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    assert!(node.account_history(&target, 10).unwrap().is_empty());
}

#[tokio::test]
async fn personal_file_signed_proof_binds_plaintext_and_host() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let source = dir.path().join("proof.txt");
    std::fs::write(&source, b"proof bytes").unwrap();
    let node = open(dir.path(), DeviceIdentity::generate(), account);
    let (id, _) = node
        .enqueue_file_to_account(&target, &source, FileKind::File)
        .await
        .unwrap();
    let host = super::super::conversation::account_conversation_id(&target, &target);
    let row = node.received_files.lock().unwrap().entries(&host)[0].clone();
    let proof = node
        .account_file_proofs
        .lock()
        .unwrap()
        .get(&id)
        .unwrap()
        .clone();
    assert!(eligible_manifest_row(&row, Some(&proof), None).is_some());
    let mut forged = row.clone();
    // Alter only display metadata: transfer identity remains the same, but signed
    // personal proof must bind the exact manifest rather than just its file key.
    let mut manifest = validated_manifest(&row.plaintext).unwrap();
    match &mut manifest {
        AnyManifest::V3(m) => m.v2.name = "forged.txt".into(),
        _ => panic!("small staged file uses V3"),
    }
    if let AnyManifest::V3(m) = manifest {
        forged.plaintext = m.encode();
    }
    assert!(eligible_manifest_row(&forged, Some(&proof), None).is_none());
    forged = row.clone();
    forged.conversation = ConversationId::new([99; 32]);
    assert!(eligible_manifest_row(&forged, Some(&proof), None).is_none());
    forged = row.clone();
    forged.event_id = EventId::new([98; 32]);
    assert!(eligible_manifest_row(&forged, Some(&proof), None).is_none());
    let mut invalid = proof.clone();
    invalid.ciphertext.push(1);
    assert!(eligible_manifest_row(&row, Some(&invalid), None).is_none());
    let invalid_binding = crate::eventlog::Event::new(
        &node.identity,
        host,
        2,
        vec![],
        2,
        proof.wall_clock + 1,
        EventKind::FileManifest,
        proof.ciphertext.clone(),
    );
    let mut forged = row.clone();
    forged.event_id = invalid_binding.id;
    forged.wall_clock = invalid_binding.wall_clock;
    forged.plaintext = crate::expiry::wrap(&row.plaintext, Some(proof.wall_clock + 60_000));
    assert!(eligible_manifest_row(&forged, Some(&invalid_binding), None).is_none());
    node.account_file_proofs
        .lock()
        .unwrap()
        .insert(invalid_binding.clone())
        .unwrap();
    node.received_files
        .lock()
        .unwrap()
        .record_file_alias_durable(&forged)
        .unwrap();
    node.reseed_live_file_book(&node.delivery.lock().unwrap())
        .unwrap();
    let file = validated_manifest(&row.plaintext).unwrap().file_conv();
    assert_eq!(node.files.lock().unwrap().manifest_events(file), vec![id]);
    let secret = node.identity.secret_bytes();
    let account_secret = node.account.secret_bytes();
    drop(node);
    let node = open(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    assert_eq!(node.files.lock().unwrap().manifest_events(file), vec![id]);
}

#[tokio::test]
async fn personal_file_store_requires_bound_local_owner() {
    use super::super::delivery_store::{DeliveryStore, DeliveryTransaction, OutgoingDelivery};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let node = open(dir.path(), DeviceIdentity::generate(), account);
    let source = dir.path().join("source");
    std::fs::write(&source, b"owner-bound").unwrap();
    let (manifest, _) = node.stage_file(&source, FileKind::File, |_| {}).unwrap();
    let host = super::super::conversation::account_conversation_id(&target, &target);
    let message = OutgoingDelivery {
        logical_id: EventId::new([97; 32]),
        sender_account: target.clone(),
        recipient_account: Some(target.clone()),
        conversation: host,
        wall_clock: 1,
        destinations: vec![],
    };
    let log = node.log.lock().unwrap();
    let plaintext = outgoing_manifest_bytes(&manifest);
    let unbound = DeliveryStore::open(&dir.path().join("unbound"), "pw").unwrap();
    assert!(
        unbound
            .prepare_outgoing_manifest(message.clone(), plaintext.clone(), &log)
            .is_err()
    );
    let mut store = node.delivery.lock().unwrap();
    let mut remote = message.clone();
    remote.recipient_account = Some("aa".repeat(16));
    assert!(
        store
            .prepare_outgoing_manifest(remote, plaintext.clone(), &log)
            .is_err()
    );
    let mut transaction = store
        .prepare_outgoing_manifest(message.clone(), plaintext.clone(), &log)
        .unwrap();
    if let DeliveryTransaction::OutgoingManifest { received, .. } = &mut transaction {
        received.from = DeviceIdentity::generate().user_id();
    }
    assert!(store.begin(transaction).is_err());
    let foreign = DeviceIdentity::generate();
    let foreign_store_dir = dir.path().join("foreign");
    let mut foreign_store = DeliveryStore::open(&foreign_store_dir, "pw").unwrap();
    foreign_store
        .bind_profile(
            &foreign_store_dir.join("events"),
            "pw",
            &foreign.public(),
            &target,
        )
        .unwrap();
    assert!(
        foreign_store
            .prepare_outgoing_manifest(message, plaintext, &log)
            .is_err()
    );
}

#[test]
fn personal_file_compact_manifest_reserves_proof_and_expiry_frame_overhead() {
    let identity = DeviceIdentity::generate();
    let mut prospective = FileManifestV3 {
        v2: FileManifestV2 {
            name: "frame-boundary.bin".into(),
            size: CHUNK_SIZE as u64,
            mime: "application/octet-stream".into(),
            checksum: [0; 32],
            file_key: [0; 32],
            file_nonce: [0; 8],
            file_conv: ConversationId::new([1; 32]),
            chunk_size: CHUNK_SIZE as u32,
            chunk_count: 1,
            chunk_hashes: vec![],
        },
        kind: FileKind::File,
    };
    let sealed_overhead = crate::dm::seal(&identity, &identity.public().x25519_pub, &[])
        .unwrap()
        .len();
    let template = crate::eventlog::Event::new(
        &identity,
        ConversationId::new([2; 32]),
        1,
        vec![],
        1,
        0,
        EventKind::FileManifest,
        vec![],
    );
    let fits = |plaintext: &[u8], personal: bool| {
        // Frame size depends on serialized lengths. Reuse one signed template
        // while finding the boundary instead of performing thousands of seals.
        let proof_overhead = if personal {
            PERSONAL_FILE_PROOF_MAGIC.len() + 32
        } else {
            0
        };
        let mut event = template.clone();
        event.ciphertext = vec![0; sealed_overhead + plaintext.len() + proof_overhead];
        super::super::session::event_fits_frame(&event)
    };
    let mut boundary = None;
    for count in 1..3000 {
        prospective.v2.chunk_count = count;
        prospective.v2.size = u64::from(count) * CHUNK_SIZE as u64;
        let mut manifest = prospective.clone();
        manifest.v2.chunk_hashes = vec![[0; 32]; count as usize];
        let plaintext = manifest.encode();
        if fits(&plaintext, false) && !fits(&crate::expiry::wrap(&plaintext, Some(1)), true) {
            boundary = Some(prospective.clone());
            break;
        }
    }
    let boundary = boundary.expect("a hash-list frame boundary must exist");
    assert!(
        compact_manifest_needed(&identity, &boundary).unwrap(),
        "choose MFM4 before personal proof/expiry wrappers overflow the frame"
    );
}

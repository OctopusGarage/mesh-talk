//! Real large-folder enqueue and staging acceptance, run separately from unit tests.
//! This checks the persisted manifest; recipient transfer has separate backend coverage.

use mesh_talk_core::{
    discovery::{Announce, Roster},
    dm,
    eventlog::EventKind,
    file::{AnyManifest, FileKind, decode_manifest},
    identity::{account::Account, device::DeviceIdentity},
    node::Node,
    recovery::{
        archive,
        key::RecoveryKey,
        snapshot::{self, SnapshotRecordV1},
    },
};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

#[tokio::test]
#[ignore = "large disk staging runs separately in backend acceptance"]
async fn large_directory_enqueue_uses_compact_manifest_instead_of_frame_error() {
    let dir = tempfile::tempdir().unwrap();
    let bob = DeviceIdentity::generate();
    let bp = Announce::new_with_account(&bob, &Account::generate(), "Bob", 9);
    let alice_identity = DeviceIdentity::generate();
    let alice_public = alice_identity.public();
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster.lock().unwrap().update(
        &bp,
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        &alice_identity.user_id(),
    );
    let (incoming, _) = mpsc::unbounded_channel();
    let (channel_incoming, _) = mpsc::unbounded_channel();
    let (file_incoming, _) = mpsc::unbounded_channel();
    let log_path = dir.path().join("events.log");
    let alice = Node::open_with_account(
        alice_identity,
        Account::generate(),
        roster,
        incoming,
        channel_incoming,
        file_incoming,
        &log_path,
        &dir.path().join("sent.log"),
        "pw",
    )
    .unwrap();
    let folder = dir.path().join("paintings");
    std::fs::create_dir(&folder).unwrap();
    // Write actual data: sparse files can be encoded differently by tar on CI.
    let mut source = std::fs::File::create(folder.join("large.bin")).unwrap();
    let block = vec![0xA5; 1024 * 1024];
    for _ in 0..110 {
        std::io::Write::write_all(&mut source, &block).unwrap();
    }
    drop(source);

    let (event_id, file_conv) = alice
        .enqueue_file_dm(&bob.user_id(), &folder, FileKind::File)
        .await
        .unwrap();
    let registered_chunks = alice
        .file_progress(file_conv)
        .unwrap()
        .expect("enqueued file must be registered")
        .total;
    assert!(registered_chunks > 2_000);
    // This fixture has no runtime workers. Stream the existing persisted source
    // through the production backup reader before closing it; reopening Node
    // could repair a missing event and mask an enqueue persistence regression.
    // The extra disk pass stays in this separately run large-disk scenario.
    let key = RecoveryKey::generate();
    let archive_path = dir.path().join("accepted-event.mtbackup");
    alice
        .write_encrypted_recovery_archive(
            std::fs::File::create(&archive_path).unwrap(),
            &key,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64,
        )
        .unwrap();
    alice.close_profile().unwrap();
    let plaintext = dir.path().join("accepted-event.snapshot");
    archive::read_archive(
        std::fs::File::open(archive_path).unwrap(),
        std::fs::File::create(&plaintext).unwrap(),
        key.as_bytes(),
    )
    .unwrap();
    let mut selected = None;
    snapshot::read_snapshot(std::fs::File::open(plaintext).unwrap(), |record| {
        if let SnapshotRecordV1::SignedEvent { event } = record
            && event.id == event_id
        {
            assert!(
                selected.is_none(),
                "accepted manifest identity must be unique"
            );
            selected = Some(event);
        }
        Ok(())
    })
    .unwrap();
    let event = selected.expect("enqueued manifest must be durably exportable without replay");
    assert!(event.verify_integrity() && event.verify_signature());
    assert_eq!(event.kind, EventKind::FileManifest);
    let plaintext = dm::open(&bob, &alice_public.x25519_pub, &event.ciphertext).unwrap();
    let Some(AnyManifest::V4(manifest)) = decode_manifest(&plaintext) else {
        panic!("large folder must use the compact manifest");
    };
    assert_eq!(manifest.v3.v2.file_conv, file_conv);
    assert_eq!(manifest.v3.v2.name, "paintings.tar");
    assert_eq!(manifest.v3.v2.mime, "application/x-mesh-talk-directory-tar");
    assert_eq!(manifest.v3.v2.chunk_count, registered_chunks);
}
